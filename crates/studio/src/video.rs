//! Export vidéo MP4 : H.264 (OpenH264) ou AV1 (rav1e), multiplexage MP4 (ISO BMFF) en Rust.
//!
//! Débit : CBR, VBR 1 passe, VBR 2 passes ou CRF (qualité constante). Les fréquences d'images
//! sont des fractions exactes (23,976 = 24000/1001).

use anyhow::{anyhow, bail, Context as _, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

// ------------------------------------------------------------------ réglages

/// Codec de la vidéo.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Codec {
    H264,
    Av1,
}

impl Codec {
    /// Plus grande image encodable (H.264 : niveau 5.2 d'OpenH264).
    pub fn max_size(self) -> Option<(u32, u32)> {
        match self {
            Codec::H264 => Some((3840, 2160)),
            Codec::Av1 => None,
        }
    }
    pub fn fits(self, w: u32, h: u32) -> bool {
        match self.max_size() {
            Some((a, b)) => w.max(h) <= a && w.min(h) <= b,
            None => true,
        }
    }
    /// Plage du CRF et sa valeur par défaut.
    pub fn crf_range(self) -> (u8, u8, u8) {
        match self {
            Codec::H264 => (0, 51, 20),
            Codec::Av1 => (0, 63, 28),
        }
    }
    pub fn supports(self, mode: RateMode) -> bool {
        !(self == Codec::Av1 && mode == RateMode::Cbr)
    }
    pub fn supports_max_bitrate(self) -> bool {
        self == Codec::H264
    }
    /// Débit le plus élevé accepté (Mbit/s).
    pub fn max_mbps(self) -> f32 {
        match self {
            Codec::H264 => 240.0,
            Codec::Av1 => 400.0,
        }
    }
}

/// Mode d'encodage du débit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RateMode {
    Cbr,
    Vbr1,
    Vbr2,
    Crf,
}

impl RateMode {
    pub const ALL: [RateMode; 4] = [RateMode::Cbr, RateMode::Vbr1, RateMode::Vbr2, RateMode::Crf];
    pub fn label(self) -> &'static str {
        match self {
            RateMode::Cbr => tr!("CBR (débit constant)", "CBR (constant bitrate)"),
            RateMode::Vbr1 => tr!("VBR, 1 passe", "VBR, 1 pass"),
            RateMode::Vbr2 => tr!("VBR, 2 passes", "VBR, 2 passes"),
            RateMode::Crf => tr!("CRF (qualité constante)", "CRF (constant quality)"),
        }
    }
    pub fn hint(self) -> &'static str {
        match self {
            RateMode::Cbr => tr!("Même débit tout le long : taille prévisible, pour la diffusion en direct ou les vieux lecteurs.", "Same bitrate throughout: predictable size, for live streaming or old players."),
            RateMode::Vbr1 => tr!("Plus de débit pour les passages complexes, moins pour les calmes ; un seul encodage.", "More bitrate for complex scenes, less for calm ones; a single encode."),
            RateMode::Vbr2 => tr!(
                "Une première passe analyse toute la vidéo, la seconde répartit le débit au mieux : \
                 meilleure qualité pour une taille donnée, encodage environ deux fois plus long.",
                "A first pass analyzes the whole video, the second spreads the bitrate as well as possible: \
                 better quality for a given size, about twice as long to encode."
            ),
            RateMode::Crf => tr!("Vise une qualité constante : le débit (et donc la taille) suit la complexité des images.", "Aims for constant quality: the bitrate (and so the size) follows the complexity of the frames."),
        }
    }
    pub fn uses_bitrate(self) -> bool {
        self != RateMode::Crf
    }
}

/// Vitesse d'encodage (contre efficacité de compression).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Speed {
    Slow,
    Medium,
    Fast,
}

impl Speed {
    pub const ALL: [Speed; 3] = [Speed::Slow, Speed::Medium, Speed::Fast];
    pub fn label(self) -> &'static str {
        match self {
            Speed::Slow => tr!("Lente (meilleure)", "Slow (best)"),
            Speed::Medium => tr!("Moyenne", "Medium"),
            Speed::Fast => tr!("Rapide (brouillon)", "Fast (draft)"),
        }
    }
}

/// Fréquence d'images exacte (fraction).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameRate {
    pub num: u32,
    pub den: u32,
}

impl FrameRate {
    pub const fn new(num: u32, den: u32) -> Self {
        Self { num, den }
    }
    pub const PRESETS: [FrameRate; 13] = [
        FrameRate::new(10, 1),
        FrameRate::new(12, 1),
        FrameRate::new(15, 1),
        FrameRate::new(24000, 1001),
        FrameRate::new(24, 1),
        FrameRate::new(25, 1),
        FrameRate::new(30000, 1001),
        FrameRate::new(30, 1),
        FrameRate::new(50, 1),
        FrameRate::new(60000, 1001),
        FrameRate::new(60, 1),
        FrameRate::new(120000, 1001),
        FrameRate::new(120, 1),
    ];
    pub fn fps(self) -> f64 {
        self.num as f64 / self.den.max(1) as f64
    }
    /// Fréquence d'un nombre décimal (29,97 → 30000/1001).
    pub fn from_fps(fps: f32) -> Self {
        let fps = fps.max(0.1) as f64;
        for p in Self::PRESETS {
            if p.den == 1001 && (p.fps() - fps).abs() < 0.006 {
                return p;
            }
        }
        if (fps - fps.round()).abs() < 1e-3 {
            return Self::new(fps.round() as u32, 1);
        }
        Self::new((fps * 1000.0).round() as u32, 1000)
    }
    /// « 23,976 », « 30 ».
    pub fn label(self) -> String {
        if self.den == 1 {
            self.num.to_string()
        } else {
            let s = format!("{:.3}", self.fps());
            s.trim_end_matches('0').trim_end_matches('.').replace('.', tr!(",", "."))
        }
    }
    /// Échelle de temps MP4 et durée d'une image dans cette échelle.
    fn timescale(self) -> (u32, u32) {
        if self.den == 1 {
            (self.num * 1000, 1000)
        } else {
            (self.num, self.den)
        }
    }
}

/// Paramètres d'un encodage vidéo.
#[derive(Clone, Copy, Debug)]
pub struct VideoParams {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub rate: FrameRate,
    pub mode: RateMode,
    /// débit cible (bit/s)
    pub target_bps: u32,
    /// débit maximal (bit/s, VBR H.264)
    pub max_bps: u32,
    pub crf: u8,
    pub speed: Speed,
}

impl VideoParams {
    pub fn check(&self) -> Result<()> {
        if self.width % 2 != 0 || self.height % 2 != 0 || self.width < 16 || self.height < 16 {
            bail!("{}", trf!("dimensions vidéo invalides : {}×{} (paires et >= 16)", "invalid video size: {}×{} (must be even and >= 16)", self.width, self.height));
        }
        if !self.codec.fits(self.width, self.height) {
            bail!(tr!("H.264 : 3840×2160 au plus ; choisissez AV1 pour une vidéo plus grande", "H.264: 3840×2160 at most; choose AV1 for a larger video"));
        }
        if self.mode.uses_bitrate() && self.target_bps < 10_000 {
            bail!(tr!("débit cible trop faible", "target bitrate too low"));
        }
        Ok(())
    }
}

// ------------------------------------------------------------------ images YUV

/// Image YUV 4:2:0 (BT.709, plage limitée).
pub struct Yuv {
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

impl Yuv {
    pub fn byte_len(w: u32, h: u32) -> usize {
        let (w, h) = (w as usize, h as usize);
        w * h + 2 * (w / 2) * (h / 2)
    }
    pub fn write_to(&self, out: &mut impl Write) -> std::io::Result<()> {
        out.write_all(&self.y)?;
        out.write_all(&self.u)?;
        out.write_all(&self.v)
    }
    pub fn from_bytes(b: &[u8], w: u32, h: u32) -> Self {
        let (ys, cs) = ((w * h) as usize, ((w / 2) * (h / 2)) as usize);
        Self { y: b[..ys].to_vec(), u: b[ys..ys + cs].to_vec(), v: b[ys + cs..ys + 2 * cs].to_vec() }
    }
}

/// RGBA sRGB -> YUV 4:2:0, BT.709, plage limitée (en parallèle par rangées).
pub fn rgba_to_yuv(rgba: &[u8], w: u32, h: u32) -> Yuv {
    let (w, h) = (w as usize, h as usize);
    let (cw, ch) = (w / 2, h / 2);
    let mut y = vec![0u8; w * h];
    let mut u = vec![0u8; cw * ch];
    let mut v = vec![0u8; cw * ch];
    y.par_chunks_mut(w).enumerate().for_each(|(row, out)| {
        let src = &rgba[row * w * 4..(row + 1) * w * 4];
        for (x, o) in out.iter_mut().enumerate() {
            let p = &src[x * 4..x * 4 + 3];
            let l = (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) / 255.0;
            *o = (16.0 + 219.0 * l).round().clamp(0.0, 255.0) as u8;
        }
    });
    u.par_chunks_mut(cw).zip(v.par_chunks_mut(cw)).enumerate().for_each(|(cy, (uo, vo))| {
        for cx in 0..cw {
            let mut acc = [0.0f32; 3];
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let i = ((cy * 2 + dy) * w + cx * 2 + dx) * 4;
                for c in 0..3 {
                    acc[c] += rgba[i + c] as f32;
                }
            }
            let (r, g, b) = (acc[0] / 1020.0, acc[1] / 1020.0, acc[2] / 1020.0);
            let l = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            uo[cx] = (128.0 + 224.0 * (b - l) / 1.8556).round().clamp(0.0, 255.0) as u8;
            vo[cx] = (128.0 + 224.0 * (r - l) / 1.5748).round().clamp(0.0, 255.0) as u8;
        }
    });
    Yuv { y, u, v }
}

/// Source d'images d'une passe : `None` à la fin.
pub type FrameSource<'a> = dyn FnMut() -> Result<Option<Yuv>> + 'a;

/// Résultat d'un encodage : configuration du décodeur et échantillons (données, image clé).
pub struct Encoded {
    pub entry: SampleEntry,
    pub samples: Vec<(Vec<u8>, bool)>,
}

/// Description du flux dans le MP4.
pub enum SampleEntry {
    Av1 { av1c: Vec<u8> },
    H264 { avcc: Vec<u8> },
}

fn threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)
}

// ------------------------------------------------------------------ AV1

pub enum Av1Pass<'a> {
    Single,
    First,
    /// résumé et données image par image de la première passe
    Second(&'a [u8], &'a [Box<[u8]>]),
}

/// Données de la première passe AV1.
pub struct Av1PassData {
    pub summary: Vec<u8>,
    pub frames: Vec<Box<[u8]>>,
}

pub fn encode_av1(
    p: &VideoParams,
    pass: Av1Pass,
    next: &mut FrameSource,
    done: &AtomicUsize,
) -> Result<(Encoded, Option<Av1PassData>)> {
    use rav1e::config::RateControlConfig;
    use rav1e::prelude::*;
    let speed = match p.speed {
        Speed::Slow => 7,
        Speed::Medium => 9,
        Speed::Fast => 10,
    };
    let mut enc = EncoderConfig::with_speed_preset(speed);
    enc.width = p.width as usize;
    enc.height = p.height as usize;
    enc.bit_depth = 8;
    enc.chroma_sampling = ChromaSampling::Cs420;
    enc.time_base = Rational::new(p.rate.den as u64, p.rate.num as u64);
    let fps = p.rate.fps();
    enc.min_key_frame_interval = 12;
    enc.max_key_frame_interval = ((fps * 4.0).round() as u64).max(24);
    match p.mode {
        RateMode::Crf => {
            // échelle 0..63 des encodeurs AV1 -> quantificateur 0..255
            enc.quantizer = (p.crf as usize * 4).min(255);
        }
        _ => {
            enc.bitrate = p.target_bps.min(i32::MAX as u32) as i32;
        }
    }
    enc.pixel_range = PixelRange::Limited;
    enc.color_description = Some(ColorDescription {
        color_primaries: ColorPrimaries::BT709,
        transfer_characteristics: TransferCharacteristics::BT709,
        matrix_coefficients: MatrixCoefficients::BT709,
    });
    let threads = threads();
    // découpage en tuiles : indispensable pour que l'encodage utilise tous les cœurs
    enc.tiles = threads.clamp(1, 16);
    let rc = match pass {
        Av1Pass::Single => RateControlConfig::new(),
        Av1Pass::First => RateControlConfig::new().with_emit_data(true),
        Av1Pass::Second(summary, _) => {
            RateControlConfig::from_summary_slice(summary).map_err(|e| anyhow!("données de première passe : {e}"))?
        }
    };
    let cfg = Config::new().with_encoder_config(enc).with_threads(threads).with_rate_control(rc);
    let mut ctx: Context<u8> = cfg.new_context().map_err(|e| anyhow!("configuration AV1 : {e:?}"))?;

    let w = p.width as usize;
    let mut samples = Vec::new();
    let mut summary = None;
    let mut pass_frames = Vec::new();
    let mut pass_in = match pass {
        Av1Pass::Second(_, f) => f.iter(),
        _ => [].iter(),
    };
    let first = matches!(pass, Av1Pass::First);
    let second = matches!(pass, Av1Pass::Second(..));
    let mut flushed = false;
    loop {
        if second {
            while ctx.rc_second_pass_data_required() > 0 {
                let d = pass_in.next().ok_or_else(|| anyhow!("données de première passe épuisées"))?;
                ctx.rc_send_pass_data(d).map_err(|e| anyhow!("données de première passe : {e:?}"))?;
            }
        }
        let (emit, finished) = match ctx.receive_packet() {
            Ok(pkt) => {
                let key = pkt.frame_type == FrameType::KEY;
                samples.push((strip_temporal_delimiter(pkt.data), key));
                done.fetch_add(1, Ordering::Relaxed);
                (true, false)
            }
            Err(EncoderStatus::NeedMoreData) => {
                if flushed {
                    break;
                }
                match next()? {
                    Some(yuv) => {
                        let mut frame = ctx.new_frame();
                        frame.planes[0].copy_from_raw_u8(&yuv.y, w, 1);
                        frame.planes[1].copy_from_raw_u8(&yuv.u, w / 2, 1);
                        frame.planes[2].copy_from_raw_u8(&yuv.v, w / 2, 1);
                        ctx.send_frame(frame).map_err(|e| anyhow!("envoi d'image : {e:?}"))?;
                    }
                    None => {
                        ctx.flush();
                        flushed = true;
                    }
                }
                (false, false)
            }
            Err(EncoderStatus::Encoded) => (true, false),
            Err(EncoderStatus::LimitReached) => (true, true),
            Err(e) => bail!("encodage AV1 : {e:?}"),
        };
        if first && emit {
            match ctx.rc_receive_pass_data() {
                Some(RcData::Frame(d)) => pass_frames.push(d),
                Some(RcData::Summary(s)) => summary = Some(s.to_vec()),
                None => {}
            }
        }
        if finished {
            break;
        }
    }
    if samples.is_empty() {
        bail!(tr!("aucune image encodée", "no frame encoded"));
    }
    let av1c = ctx.container_sequence_header();
    let data = if first {
        Some(Av1PassData {
            summary: summary.ok_or_else(|| anyhow!("résumé de première passe absent"))?,
            frames: pass_frames,
        })
    } else {
        None
    };
    Ok((Encoded { entry: SampleEntry::Av1 { av1c }, samples }, data))
}

/// Les échantillons MP4 ne doivent pas contenir l'OBU "temporal delimiter".
fn strip_temporal_delimiter(mut data: Vec<u8>) -> Vec<u8> {
    if data.len() >= 2 && data[0] == 0x12 && data[1] == 0x00 {
        data.drain(0..2);
    }
    data
}

// ------------------------------------------------------------------ H.264

/// Réglage du contrôle de débit d'un encodeur H.264.
#[derive(Clone, Copy)]
pub enum H264Rate {
    /// quantificateur fixe
    ConstantQp(u8),
    /// débit cible et maximal (bit/s), quantificateurs permis
    Bitrate { target: u32, max: u32, qp: (u8, u8) },
}

/// Encodeur OpenH264 (API C, compilée depuis les sources).
struct H264 {
    api: openh264_sys2::DynamicAPI,
    enc: *mut openh264_sys2::ISVCEncoder,
    info: Box<openh264_sys2::SFrameBSInfo>,
    w: u32,
    h: u32,
}

// l'encodeur n'est utilisé que par un fil à la fois
unsafe impl Send for H264 {}

impl H264 {
    fn new(p: &VideoParams, rate: H264Rate, speed: Speed) -> Result<Self> {
        use openh264_sys2::*;
        let api = DynamicAPI::from_source();
        let mut enc: *mut ISVCEncoder = std::ptr::null_mut();
        unsafe {
            if api.WelsCreateSVCEncoder(&mut enc) != 0 || enc.is_null() {
                bail!(tr!("création de l'encodeur H.264", "could not create the H.264 encoder"));
            }
        }
        let me = Self { api, enc, info: Box::default(), w: p.width, h: p.height };
        let vt = unsafe { &**enc };
        let fps = p.rate.fps() as f32;
        let threads = threads().clamp(1, 16);
        let mut ps = SEncParamExt::default();
        unsafe {
            let mut quiet: i32 = WELS_LOG_QUIET as i32;
            (vt.SetOption.context("vtable")?)(enc, ENCODER_OPTION_TRACE_LEVEL, (&raw mut quiet).cast());
            (vt.GetDefaultParams.context("vtable")?)(enc, &mut ps);
        }
        ps.iUsageType = CAMERA_VIDEO_REAL_TIME;
        ps.iPicWidth = p.width as i32;
        ps.iPicHeight = p.height as i32;
        ps.fMaxFrameRate = fps;
        ps.iSpatialLayerNum = 1;
        ps.iTemporalLayerNum = 1;
        ps.iComplexityMode = match speed {
            Speed::Slow => HIGH_COMPLEXITY,
            Speed::Medium => MEDIUM_COMPLEXITY,
            Speed::Fast => LOW_COMPLEXITY,
        };
        // une image clé toutes les 2 s : on peut se déplacer dans la vidéo
        ps.uiIntraPeriod = ((fps * 2.0).round() as u32).max(1);
        ps.iEntropyCodingModeFlag = 1; // CABAC (profil High)
        ps.bEnableFrameSkip = false;
        ps.bEnableDenoise = false;
        ps.bEnableSceneChangeDetect = true;
        ps.bEnableAdaptiveQuant = true;
        ps.bEnableBackgroundDetection = true;
        ps.iLoopFilterDisableIdc = 0;
        ps.iMultipleThreadIdc = threads as u16;
        ps.eSpsPpsIdStrategy = CONSTANT_ID;
        let l = &mut ps.sSpatialLayers[0];
        l.iVideoWidth = p.width as i32;
        l.iVideoHeight = p.height as i32;
        l.fFrameRate = fps;
        l.uiProfileIdc = PRO_HIGH;
        l.sSliceArgument.uiSliceMode = if threads > 1 { SM_FIXEDSLCNUM_SLICE } else { SM_SINGLE_SLICE };
        l.sSliceArgument.uiSliceNum = threads as u32;
        l.bVideoSignalTypePresent = true;
        l.uiVideoFormat = VF_UNDEF as u8;
        l.bFullRange = false;
        l.bColorDescriptionPresent = true;
        l.uiColorPrimaries = CP_BT709 as u8;
        l.uiTransferCharacteristics = TRC_BT709 as u8;
        l.uiColorMatrix = CM_BT709 as u8;
        match rate {
            H264Rate::ConstantQp(qp) => {
                ps.iRCMode = RC_OFF_MODE;
                ps.iTargetBitrate = 1_000_000;
                l.iSpatialBitrate = 1_000_000;
                l.iMaxSpatialBitrate = UNSPECIFIED_BIT_RATE as i32;
                ps.iMaxBitrate = UNSPECIFIED_BIT_RATE as i32;
                l.iDLayerQp = qp.min(51) as i32;
                ps.iMinQp = 0;
                ps.iMaxQp = 51;
            }
            H264Rate::Bitrate { target, max, qp } => {
                let max = max.max(target);
                ps.iRCMode = RC_BITRATE_MODE;
                ps.iTargetBitrate = target as i32;
                ps.iMaxBitrate = max as i32;
                l.iSpatialBitrate = target as i32;
                l.iMaxSpatialBitrate = max as i32;
                ps.iMinQp = qp.0 as i32;
                ps.iMaxQp = qp.1 as i32;
            }
        }
        unsafe {
            let r = (vt.InitializeExt.context("vtable")?)(enc, &ps);
            if r != 0 {
                bail!("{}", trf!("paramètres H.264 refusés (code {r})", "H.264 settings rejected (code {r})"));
            }
            let mut quiet: i32 = WELS_LOG_QUIET as i32;
            (vt.SetOption.context("vtable")?)(enc, ENCODER_OPTION_TRACE_LEVEL, (&raw mut quiet).cast());
            let mut fmt: i32 = videoFormatI420 as i32;
            (vt.SetOption.context("vtable")?)(enc, ENCODER_OPTION_DATAFORMAT, (&raw mut fmt).cast());
        }
        Ok(me)
    }

    /// Change le débit cible en cours d'encodage (bit/s).
    fn set_bitrate(&mut self, bps: u32) -> Result<()> {
        use openh264_sys2::*;
        let vt = unsafe { &**self.enc };
        let mut info = TagBitrateInfo { iLayer: SPATIAL_LAYER_ALL, iBitrate: bps.min(i32::MAX as u32) as i32 };
        let r = unsafe { (vt.SetOption.context("vtable")?)(self.enc, ENCODER_OPTION_BITRATE, (&raw mut info).cast()) };
        if r != 0 {
            bail!("{}", trf!("débit H.264 refusé (code {r})", "H.264 bitrate rejected (code {r})"));
        }
        Ok(())
    }

    /// Encode une image ; renvoie ses unités NAL (sans code de début).
    fn encode(&mut self, yuv: &Yuv, index: u64, rate: FrameRate) -> Result<Vec<Vec<u8>>> {
        use openh264_sys2::*;
        let vt = unsafe { &**self.enc };
        let (w, h) = (self.w as i32, self.h as i32);
        let pic = SSourcePicture {
            iColorFormat: videoFormatI420 as i32,
            iStride: [w, w / 2, w / 2, 0],
            pData: [yuv.y.as_ptr().cast_mut(), yuv.u.as_ptr().cast_mut(), yuv.v.as_ptr().cast_mut(), std::ptr::null_mut()],
            iPicWidth: w,
            iPicHeight: h,
            uiTimeStamp: (index * 1000 * rate.den as u64 / rate.num.max(1) as u64) as i64,
            bPsnrY: false,
            bPsnrU: false,
            bPsnrV: false,
        };
        let r = unsafe { (vt.EncodeFrame.context("vtable")?)(self.enc, &pic, &mut *self.info) };
        if r != 0 {
            bail!("{}", trf!("encodage H.264 (code {r})", "H.264 encoding (code {r})"));
        }
        let mut nals = Vec::new();
        if self.info.eFrameType == videoFrameTypeSkip {
            return Ok(nals);
        }
        for li in 0..self.info.iLayerNum.max(0) as usize {
            let layer = &self.info.sLayerInfo[li];
            let mut off = 0usize;
            for ni in 0..layer.iNalCount.max(0) as usize {
                let len = unsafe { *layer.pNalLengthInByte.add(ni) } as usize;
                let nal = unsafe { std::slice::from_raw_parts(layer.pBsBuf.add(off), len) };
                off += len;
                let start = if nal.starts_with(&[0, 0, 0, 1]) {
                    4
                } else if nal.starts_with(&[0, 0, 1]) {
                    3
                } else {
                    0
                };
                if nal.len() > start {
                    nals.push(nal[start..].to_vec());
                }
            }
        }
        Ok(nals)
    }
}

impl Drop for H264 {
    fn drop(&mut self) {
        use openh264_sys2::API;
        unsafe {
            if let Some(f) = (**self.enc).Uninitialize {
                f(self.enc);
            }
            self.api.WelsDestroySVCEncoder(self.enc);
        }
    }
}

/// Encode toutes les images d'une source en H.264. `schedule` : débits cibles (bit/s) à
/// appliquer à partir de certaines images (seconde passe d'un VBR 2 passes).
pub fn encode_h264(
    p: &VideoParams,
    rate: H264Rate,
    speed: Speed,
    schedule: &[(usize, u32)],
    next: &mut FrameSource,
    done: &AtomicUsize,
) -> Result<Encoded> {
    let mut enc = H264::new(p, rate, speed)?;
    let (mut sps, mut pps) = (None, None);
    let mut samples = Vec::new();
    let mut index = 0u64;
    let mut sched = schedule.iter().peekable();
    while let Some(yuv) = next()? {
        while let Some(&&(at, bps)) = sched.peek() {
            if at as u64 > index {
                break;
            }
            enc.set_bitrate(bps)?;
            sched.next();
        }
        let nals = enc.encode(&yuv, index, p.rate)?;
        index += 1;
        let mut sample = Vec::new();
        let mut key = false;
        for nal in nals {
            match nal[0] & 0x1f {
                7 => {
                    sps.get_or_insert(nal);
                }
                8 => {
                    pps.get_or_insert(nal);
                }
                t => {
                    key |= t == 5;
                    sample.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                    sample.extend_from_slice(&nal);
                }
            }
        }
        if sample.is_empty() {
            bail!("{}", trf!("image {index} non encodée", "frame {index} not encoded"));
        }
        samples.push((sample, key));
        done.fetch_add(1, Ordering::Relaxed);
    }
    if samples.is_empty() {
        bail!(tr!("aucune image encodée", "no frame encoded"));
    }
    let (sps, pps) = (sps.context("SPS H.264 absent")?, pps.context("PPS H.264 absent")?);
    // AVCDecoderConfigurationRecord
    let mut avcc = vec![1, sps[1], sps[2], sps[3], 0xff, 0xe1];
    avcc.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    avcc.extend_from_slice(&sps);
    avcc.push(1);
    avcc.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    avcc.extend_from_slice(&pps);
    if matches!(sps[1], 100 | 110 | 122 | 144) {
        // profils High : chroma 4:2:0, 8 bits, pas d'extension SPS
        avcc.extend_from_slice(&[0xfc | 1, 0xf8, 0xf8, 0]);
    }
    Ok(Encoded { entry: SampleEntry::H264 { avcc }, samples })
}

/// Seconde passe d'un VBR 2 passes : débit de chaque seconde d'après la taille des images
/// de la première passe (à quantificateur fixe, donc reflet de leur complexité). Les passages
/// complexes reçoivent plus de débit, les calmes moins (exposant 0,6 comme le « qcomp »
/// de x264) ; la moyenne reste le débit cible et aucune seconde ne dépasse le maximum.
pub fn two_pass_schedule(sizes: &[usize], fps: f64, target: u32, max: u32) -> Vec<(usize, u32)> {
    let seg = (fps.round() as usize).max(1);
    let n = sizes.len().max(1) as f64;
    let segs: Vec<(usize, f64, f64)> = sizes
        .chunks(seg)
        .enumerate()
        .map(|(i, c)| {
            let per_frame = c.iter().sum::<usize>() as f64 / c.len() as f64;
            (i * seg, c.len() as f64, per_frame.max(1.0).powf(0.6))
        })
        .collect();
    let (t, hi) = (target as f64, max.max(target) as f64);
    let lo = t * 0.25;
    let wsum: f64 = segs.iter().map(|s| s.1 * s.2).sum();
    let mut rates: Vec<f64> = segs.iter().map(|s| (t * n * s.2 / wsum.max(1e-9)).clamp(lo, hi)).collect();
    // les plafonds font baisser la moyenne : on redistribue
    for _ in 0..8 {
        let avg = segs.iter().zip(&rates).map(|(s, r)| s.1 * r).sum::<f64>() / n;
        if (avg - t).abs() < t * 0.002 {
            break;
        }
        for r in &mut rates {
            *r = (*r * t / avg).clamp(lo, hi);
        }
    }
    segs.iter().zip(rates).map(|(s, r)| (s.0, r.round() as u32)).collect()
}

// ------------------------------------------------------------------ MP4

fn mp4_box(kind: &[u8; 4], content: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(content.len() + 8);
    b.extend_from_slice(&((content.len() + 8) as u32).to_be_bytes());
    b.extend_from_slice(kind);
    b.extend_from_slice(content);
    b
}

fn full_box(kind: &[u8; 4], version: u8, flags: u32, content: &[u8]) -> Vec<u8> {
    let mut c = Vec::with_capacity(content.len() + 4);
    c.push(version);
    c.extend_from_slice(&flags.to_be_bytes()[1..]);
    c.extend_from_slice(content);
    mp4_box(kind, &c)
}

struct Buf(Vec<u8>);
impl Buf {
    fn new() -> Self {
        Self(Vec::new())
    }
    fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }
    fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn u64(&mut self, v: u64) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.0.extend_from_slice(b);
        self
    }
    fn matrix(&mut self) -> &mut Self {
        for v in [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
            self.u32(v);
        }
        self
    }
}

/// Écrit un fichier MP4 contenant une piste vidéo (AV1 ou H.264).
pub fn write_mp4(path: &Path, w: u32, h: u32, rate: FrameRate, enc: &Encoded) -> Result<()> {
    let samples = &enc.samples;
    let (timescale, delta) = rate.timescale();
    let n = samples.len() as u32;
    let duration = n as u64 * delta as u64;
    let (fourcc, codec_brand, label, config): (&[u8; 4], &[u8; 4], &[u8], Vec<u8>) = match &enc.entry {
        SampleEntry::Av1 { av1c } => (b"av01", b"av01", b"AV1 PlanetStudio", mp4_box(b"av1C", av1c)),
        SampleEntry::H264 { avcc } => (b"avc1", b"avc1", b"H.264 PlanetStudio", mp4_box(b"avcC", avcc)),
    };

    let ftyp = mp4_box(
        b"ftyp",
        &Buf::new().bytes(b"isom").u32(0x200).bytes(b"isom").bytes(b"iso2").bytes(codec_brand).bytes(b"mp41").0,
    );
    let mdat_payload: usize = samples.iter().map(|s| s.0.len()).sum();
    // mdat en 64 bits : en-tête de 16 octets
    let mdat_start = ftyp.len() as u64;
    let mut offsets = Vec::with_capacity(samples.len());
    let mut off = mdat_start + 16;
    for s in samples {
        offsets.push(off);
        off += s.0.len() as u64;
    }

    // --- stbl
    let mut entry = Buf::new();
    entry.bytes(&[0; 6]).u16(1) // réservé + data_reference_index
        .u16(0).u16(0).u32(0).u32(0).u32(0)
        .u16(w as u16).u16(h as u16)
        .u32(0x0048_0000).u32(0x0048_0000).u32(0).u16(1);
    let mut name = [0u8; 32];
    name[0] = label.len() as u8;
    name[1..=label.len()].copy_from_slice(label);
    entry.bytes(&name).u16(0x18).u16(0xffff);
    entry.bytes(&config);
    // colr nclx : BT.709, plage limitée
    entry.bytes(&mp4_box(b"colr", &Buf::new().bytes(b"nclx").u16(1).u16(1).u16(1).u8(0).0));
    let stsd = full_box(b"stsd", 0, 0, &Buf::new().u32(1).bytes(&mp4_box(fourcc, &entry.0)).0);
    let stts = full_box(b"stts", 0, 0, &Buf::new().u32(1).u32(n).u32(delta).0);
    let keys: Vec<u32> = samples.iter().enumerate().filter(|(_, s)| s.1).map(|(i, _)| i as u32 + 1).collect();
    let mut stss_b = Buf::new();
    stss_b.u32(keys.len() as u32);
    for k in &keys {
        stss_b.u32(*k);
    }
    let stss = full_box(b"stss", 0, 0, &stss_b.0);
    let stsc = full_box(b"stsc", 0, 0, &Buf::new().u32(1).u32(1).u32(1).u32(1).0);
    let mut stsz_b = Buf::new();
    stsz_b.u32(0).u32(n);
    for s in samples {
        stsz_b.u32(s.0.len() as u32);
    }
    let stsz = full_box(b"stsz", 0, 0, &stsz_b.0);
    let mut co64_b = Buf::new();
    co64_b.u32(n);
    for o in &offsets {
        co64_b.u64(*o);
    }
    let co64 = full_box(b"co64", 0, 0, &co64_b.0);
    let stbl = mp4_box(b"stbl", &[stsd, stts, stss, stsc, stsz, co64].concat());

    let vmhd = full_box(b"vmhd", 0, 1, &Buf::new().u16(0).u16(0).u16(0).u16(0).0);
    let dref = full_box(b"dref", 0, 0, &Buf::new().u32(1).bytes(&full_box(b"url ", 0, 1, &[])).0);
    let dinf = mp4_box(b"dinf", &dref);
    let minf = mp4_box(b"minf", &[vmhd, dinf, stbl].concat());
    let mdhd = full_box(b"mdhd", 1, 0, &Buf::new().u64(0).u64(0).u32(timescale).u64(duration).u16(0x55c4).u16(0).0);
    let hdlr = full_box(b"hdlr", 0, 0, &Buf::new().u32(0).bytes(b"vide").u32(0).u32(0).u32(0).bytes(b"PlanetStudio\0").0);
    let mdia = mp4_box(b"mdia", &[mdhd, hdlr, minf].concat());
    let mut tkhd_b = Buf::new();
    tkhd_b.u64(0).u64(0).u32(1).u32(0).u64(duration).u32(0).u32(0).u16(0).u16(0).u16(0).u16(0).matrix().u32(w << 16).u32(h << 16);
    let tkhd = full_box(b"tkhd", 1, 3, &tkhd_b.0);
    let trak = mp4_box(b"trak", &[tkhd, mdia].concat());
    let mut mvhd_b = Buf::new();
    mvhd_b.u64(0).u64(0).u32(timescale).u64(duration).u32(0x0001_0000).u16(0x0100).u16(0).u32(0).u32(0).matrix();
    for _ in 0..6 {
        mvhd_b.u32(0);
    }
    mvhd_b.u32(2);
    let mvhd = full_box(b"mvhd", 1, 0, &mvhd_b.0);
    let moov = mp4_box(b"moov", &[mvhd, trak].concat());

    let mut f = std::io::BufWriter::new(std::fs::File::create(path).with_context(|| trf!("création de {}", "creating {}", path.display()))?);
    f.write_all(&ftyp)?;
    f.write_all(&1u32.to_be_bytes())?;
    f.write_all(b"mdat")?;
    f.write_all(&((mdat_payload as u64) + 16).to_be_bytes())?;
    for s in samples {
        f.write_all(&s.0)?;
    }
    f.write_all(&moov)?;
    f.flush()?;
    Ok(())
}
