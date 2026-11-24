//! Écriture des exports dans un fil séparé : vidéo MP4 (H.264 ou AV1, 1 ou 2 passes),
//! GIF animé, séquence d'images, et images fixes (PNG, JPEG, TIFF, BMP, TGA, WebP).
//!
//! L'interface envoie des images RGBA (sRGB) par un canal borné et reste réactive.

use crate::ffmpeg;
use crate::video::{self, Av1Pass, Codec, FrameRate, H264Rate, RateMode, VideoParams, Yuv};
use anyhow::{anyhow, bail, Context as _, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::JoinHandle;

// ------------------------------------------------------------------ formats

/// Format d'image (photo ou séquence).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Tiff,
    Webp,
    Bmp,
    Tga,
}

impl ImageFormat {
    pub const ALL: [ImageFormat; 6] =
        [ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::Tiff, ImageFormat::Webp, ImageFormat::Bmp, ImageFormat::Tga];
    pub fn label(self) -> &'static str {
        match self {
            ImageFormat::Png => "PNG",
            ImageFormat::Jpeg => "JPEG",
            ImageFormat::Tiff => "TIFF",
            ImageFormat::Webp => tr!("WebP (sans perte)", "WebP (lossless)"),
            ImageFormat::Bmp => "BMP",
            ImageFormat::Tga => "TGA (Targa)",
        }
    }
    pub fn ext(self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpg",
            ImageFormat::Tiff => "tif",
            ImageFormat::Webp => "webp",
            ImageFormat::Bmp => "bmp",
            ImageFormat::Tga => "tga",
        }
    }
    pub fn hint(self) -> &'static str {
        match self {
            ImageFormat::Png => tr!("Sans perte, compressé : le choix sûr pour le web et la retouche.", "Lossless, compressed: the safe choice for the web and editing."),
            ImageFormat::Jpeg => tr!("Avec perte, fichiers légers : partage, réseaux sociaux.", "Lossy, small files: sharing, social media."),
            ImageFormat::Tiff => tr!("Sans perte, non compressé : impression, Photoshop, montage.", "Lossless, uncompressed: printing, Photoshop, editing."),
            ImageFormat::Webp => tr!("Sans perte, plus léger que le PNG : web.", "Lossless, smaller than PNG: web."),
            ImageFormat::Bmp => tr!("Sans perte, non compressé : compatibilité Windows ancienne.", "Lossless, uncompressed: old Windows compatibility."),
            ImageFormat::Tga => tr!("Sans perte : séquences pour les logiciels de compositing et de jeu.", "Lossless: sequences for compositing and game software."),
        }
    }
    pub fn has_quality(self) -> bool {
        self == ImageFormat::Jpeg
    }
}

/// Écrit une image RGBA 8 bits.
pub fn write_image(path: &Path, w: u32, h: u32, rgba: &[u8], format: ImageFormat, quality: u8) -> Result<()> {
    use image::{ExtendedColorType, ImageEncoder};
    if format == ImageFormat::Png {
        return crate::app::write_png(path, w, h, rgba);
    }
    let file = std::fs::File::create(path).with_context(|| trf!("création de {}", "creating {}", path.display()))?;
    let mut out = BufWriter::new(file);
    // l'alpha du rendu n'a pas de sens (fond toujours opaque) : RVB
    let rgb: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
    let rgb8 = ExtendedColorType::Rgb8;
    match format {
        ImageFormat::Png => unreachable!(),
        ImageFormat::Jpeg => {
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100)).write_image(&rgb, w, h, rgb8)?
        }
        ImageFormat::Tiff => image::codecs::tiff::TiffEncoder::new(&mut out).write_image(&rgb, w, h, rgb8)?,
        ImageFormat::Webp => image::codecs::webp::WebPEncoder::new_lossless(&mut out).write_image(&rgb, w, h, rgb8)?,
        ImageFormat::Bmp => image::codecs::bmp::BmpEncoder::new(&mut out).write_image(&rgb, w, h, rgb8)?,
        ImageFormat::Tga => image::codecs::tga::TgaEncoder::new(&mut out).write_image(&rgb, w, h, rgb8)?,
    }
    out.flush()?;
    Ok(())
}

/// Format de sortie d'un export animé.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoFormat {
    H264,
    Av1,
    Gif,
    Sequence,
}

impl VideoFormat {
    pub const ALL: [VideoFormat; 4] = [VideoFormat::H264, VideoFormat::Av1, VideoFormat::Gif, VideoFormat::Sequence];
    pub fn label(self) -> &'static str {
        match self {
            VideoFormat::H264 => "H.264 (MP4)",
            VideoFormat::Av1 => "AV1 (MP4)",
            VideoFormat::Gif => tr!("GIF animé", "Animated GIF"),
            VideoFormat::Sequence => tr!("Séquence d'images", "Image sequence"),
        }
    }
    pub fn hint(self) -> &'static str {
        match self {
            VideoFormat::H264 => tr!("Le plus compatible : tous les lecteurs, téléphones, réseaux sociaux, logiciels de montage. Jusqu'à 4K.", "The most compatible: every player, phone, social network and editing software. Up to 4K."),
            VideoFormat::Av1 => tr!("Fichiers plus légers à qualité égale, jusqu'à 8K. Lu par Windows 11, VLC, les navigateurs, Premiere et Kdenlive ; encodage plus lent.", "Smaller files at equal quality, up to 8K. Plays in Windows 11, VLC, browsers, Premiere and Kdenlive; slower to encode."),
            VideoFormat::Gif => tr!("Animation sans son, 256 couleurs par image : courtes boucles pour le web et les messageries. Préférez une petite taille.", "Silent animation, 256 colors per frame: short loops for the web and chat apps. Prefer a small size."),
            VideoFormat::Sequence => tr!("Une image par fichier, sans perte : pour le compositing ou un montage en dehors du studio.", "One image per file, lossless: for compositing or editing outside the studio."),
        }
    }
    pub fn codec(self) -> Option<Codec> {
        match self {
            VideoFormat::H264 => Some(Codec::H264),
            VideoFormat::Av1 => Some(Codec::Av1),
            _ => None,
        }
    }
    pub fn ext(self) -> &'static str {
        match self {
            VideoFormat::H264 | VideoFormat::Av1 => "mp4",
            VideoFormat::Gif => "gif",
            VideoFormat::Sequence => "",
        }
    }
}

// ------------------------------------------------------------------ travaux

/// Ce que le fil d'écriture produit.
pub enum Job {
    Video { path: PathBuf, params: VideoParams },
    /// vidéo encodée par ffmpeg (`exe`) avec l'encodeur `backend`
    Ffmpeg { path: PathBuf, params: VideoParams, exe: PathBuf, backend: ffmpeg::Backend },
    Gif { path: PathBuf, width: u32, height: u32, rate: FrameRate, repeat: bool },
    Sequence { dir: PathBuf, base: String, format: ImageFormat, quality: u8, width: u32, height: u32 },
}

/// Avancement du fil d'écriture.
#[derive(Default)]
pub struct Progress {
    /// images traitées dans l'étape en cours
    pub done: AtomicUsize,
    /// étape : 0 = encodage en une passe (ou écriture), 1 = réception avant 2 passes,
    /// 2 = première passe, 3 = seconde passe
    pub stage: AtomicUsize,
}

/// Fil d'écriture d'un export.
pub struct Exporter {
    tx: Option<SyncSender<Vec<u8>>>,
    handle: Option<JoinHandle<Result<PathBuf>>>,
    pub progress: Arc<Progress>,
}

impl Exporter {
    pub fn start(job: Job) -> Result<Self> {
        if let Job::Video { params, .. } | Job::Ffmpeg { params, .. } = &job {
            params.check()?;
        }
        let (tx, rx) = sync_channel::<Vec<u8>>(3);
        let progress = Arc::new(Progress::default());
        let p2 = progress.clone();
        let handle = std::thread::Builder::new()
            .name("export".into())
            .spawn(move || run(job, rx, &p2))
            .context("création du fil d'export")?;
        Ok(Self { tx: Some(tx), handle: Some(handle), progress })
    }

    /// Envoie une image RGBA. Renvoie l'image si la file est pleine (à réessayer plus tard).
    pub fn try_push(&self, rgba: Vec<u8>) -> Result<Option<Vec<u8>>> {
        let tx = self.tx.as_ref().ok_or_else(|| anyhow!(tr!("export fermé", "export closed")))?;
        match tx.try_send(rgba) {
            Ok(()) => Ok(None),
            Err(TrySendError::Full(img)) => Ok(Some(img)),
            Err(TrySendError::Disconnected(_)) => bail!(tr!("l'export s'est arrêté", "the export stopped")),
        }
    }

    /// Envoie une image en attendant une place dans la file.
    pub fn push_blocking(&self, rgba: Vec<u8>) -> Result<()> {
        let tx = self.tx.as_ref().ok_or_else(|| anyhow!(tr!("export fermé", "export closed")))?;
        tx.send(rgba).map_err(|_| anyhow!(tr!("l'export s'est arrêté", "the export stopped")))
    }

    /// Ferme l'entrée : le fil termine puis écrit le fichier.
    pub fn close_input(&mut self) {
        self.tx = None;
    }

    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(|h| h.is_finished())
    }

    /// Attend la fin et renvoie le chemin écrit.
    pub fn finish(mut self) -> Result<PathBuf> {
        self.tx = None;
        match self.handle.take() {
            Some(h) => h.join().map_err(|_| anyhow!(tr!("le fil d'export a planté", "the export thread crashed")))?,
            None => bail!(tr!("export déjà terminé", "export already finished")),
        }
    }
}

fn run(job: Job, rx: Receiver<Vec<u8>>, progress: &Progress) -> Result<PathBuf> {
    match job {
        Job::Video { path, params } => {
            let enc = if params.mode == RateMode::Vbr2 {
                two_pass(&path, &params, rx, progress)?
            } else {
                let (w, h) = (params.width, params.height);
                let mut next = || -> Result<Option<Yuv>> { Ok(rx.recv().ok().map(|rgba| video::rgba_to_yuv(&rgba, w, h))) };
                match params.codec {
                    Codec::Av1 => video::encode_av1(&params, Av1Pass::Single, &mut next, &progress.done)?.0,
                    Codec::H264 => {
                        let mbps = params.target_bps;
                        let rate = match params.mode {
                            RateMode::Crf => H264Rate::ConstantQp(params.crf),
                            RateMode::Cbr => H264Rate::Bitrate { target: mbps, max: mbps, qp: (0, 51) },
                            _ => H264Rate::Bitrate { target: mbps, max: params.max_bps, qp: (0, 51) },
                        };
                        video::encode_h264(&params, rate, params.speed, &[], &mut next, &progress.done)?
                    }
                }
            };
            video::write_mp4(&path, params.width, params.height, params.rate, &enc)?;
            Ok(path)
        }
        Job::Ffmpeg { path, params: p, exe, backend } => {
            let encode =
                |pass, next: &mut dyn FnMut() -> Result<Option<Yuv>>| ffmpeg::encode(&exe, backend, &p, &path, pass, next, &progress.done);
            if p.mode == RateMode::Vbr2 && backend.true_two_pass() {
                let staged = Staged::receive(&path, &p, rx, progress)?;
                // statistiques de la 1re passe, à côté de la vidéo, effacées à la fin
                let log = path.with_extension("2passes");
                struct Remove(Vec<PathBuf>);
                impl Drop for Remove {
                    fn drop(&mut self) {
                        for f in &self.0 {
                            let _ = std::fs::remove_file(f);
                        }
                    }
                }
                let _logs = Remove(ffmpeg::pass_log_files(&log));
                begin(progress, 2);
                encode(ffmpeg::Pass::First(&log), &mut *staged.frames()?)?;
                begin(progress, 3);
                encode(ffmpeg::Pass::Second(&log), &mut *staged.frames()?)?;
            } else {
                let (w, h) = (p.width, p.height);
                let mut next = || -> Result<Option<Yuv>> { Ok(rx.recv().ok().map(|rgba| video::rgba_to_yuv(&rgba, w, h))) };
                encode(ffmpeg::Pass::Single, &mut next)?;
            }
            Ok(path)
        }
        Job::Gif { path, width, height, rate, repeat } => {
            let file = std::fs::File::create(&path).with_context(|| trf!("création de {}", "creating {}", path.display()))?;
            let mut enc = gif::Encoder::new(BufWriter::new(file), width as u16, height as u16, &[])?;
            enc.set_repeat(if repeat { gif::Repeat::Infinite } else { gif::Repeat::Finite(0) })?;
            // délais en centièmes de seconde, arrondis sans dérive (24 i/s : 4, 4, 5, 4, 4, 5...)
            let cs = |k: u64| (k as f64 * 100.0 / rate.fps()).round() as u64;
            let mut k = 0u64;
            for mut rgba in rx.iter() {
                let mut frame = gif::Frame::from_rgba_speed(width as u16, height as u16, &mut rgba, 10);
                frame.delay = (cs(k + 1) - cs(k)).max(2) as u16;
                enc.write_frame(&frame)?;
                k += 1;
                progress.done.fetch_add(1, Ordering::Relaxed);
            }
            if k == 0 {
                bail!(tr!("aucune image", "no frame"));
            }
            drop(enc);
            Ok(path)
        }
        Job::Sequence { dir, base, format, quality, width, height } => {
            let mut k = 0usize;
            for rgba in rx.iter() {
                let path = dir.join(format!("{base}_{k:05}.{}", format.ext()));
                write_image(&path, width, height, &rgba, format, quality)?;
                k += 1;
                progress.done.fetch_add(1, Ordering::Relaxed);
            }
            Ok(dir)
        }
    }
}

/// VBR 2 passes : les images reçues sont gardées (en YUV) dans un fichier temporaire à côté
/// de la vidéo, analysées par une première passe, puis encodées par la seconde.
fn two_pass(path: &Path, p: &VideoParams, rx: Receiver<Vec<u8>>, progress: &Progress) -> Result<video::Encoded> {
    let staged = Staged::receive(path, p, rx, progress)?;
    match p.codec {
        Codec::Av1 => {
            begin(progress, 2);
            let (_, data) = video::encode_av1(p, Av1Pass::First, &mut *staged.frames()?, &progress.done)?;
            let data = data.context("première passe AV1")?;
            begin(progress, 3);
            Ok(video::encode_av1(p, Av1Pass::Second(&data.summary, &data.frames), &mut *staged.frames()?, &progress.done)?.0)
        }
        Codec::H264 => {
            // 1re passe : quantificateur fixe, pour mesurer la complexité de chaque passage
            begin(progress, 2);
            let first =
                video::encode_h264(p, H264Rate::ConstantQp(26), video::Speed::Fast, &[], &mut *staged.frames()?, &progress.done)?;
            let sizes: Vec<usize> = first.samples.iter().map(|s| s.0.len()).collect();
            let schedule = video::two_pass_schedule(&sizes, p.rate.fps(), p.target_bps, p.max_bps);
            // 2e passe : débit réparti seconde par seconde selon la complexité mesurée
            begin(progress, 3);
            let start = schedule.first().map(|s| s.1).unwrap_or(p.target_bps);
            let rate = H264Rate::Bitrate { target: start, max: p.max_bps, qp: (0, 51) };
            video::encode_h264(p, rate, p.speed, &schedule, &mut *staged.frames()?, &progress.done)
        }
    }
}

/// Début d'une étape du fil d'écriture : son compteur repart de zéro.
fn begin(progress: &Progress, stage: usize) {
    progress.done.store(0, Ordering::Relaxed);
    progress.stage.store(stage, Ordering::Relaxed);
}

/// Images d'un export en 2 passes, gardées (en YUV) dans un fichier temporaire à côté de
/// la vidéo pour être relues à chaque passe ; le fichier est effacé avec cette valeur.
struct Staged {
    tmp: PathBuf,
    w: u32,
    h: u32,
}

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.tmp);
    }
}

impl Staged {
    /// Étape 1 : reçoit toutes les images.
    fn receive(path: &Path, p: &VideoParams, rx: Receiver<Vec<u8>>, progress: &Progress) -> Result<Self> {
        let staged = Staged { tmp: path.with_extension("2passes.tmp"), w: p.width, h: p.height };
        progress.stage.store(1, Ordering::Relaxed);
        let file = std::fs::File::create(&staged.tmp).with_context(|| trf!("création de {}", "creating {}", staged.tmp.display()))?;
        let mut out = BufWriter::with_capacity(1 << 22, file);
        let mut n = 0usize;
        for rgba in rx.iter() {
            video::rgba_to_yuv(&rgba, staged.w, staged.h).write_to(&mut out).context(tr!("fichier temporaire des 2 passes", "2-pass temporary file"))?;
            n += 1;
            progress.done.fetch_add(1, Ordering::Relaxed);
        }
        out.flush()?;
        drop(out);
        if n == 0 {
            bail!(tr!("aucune image", "no frame"));
        }
        Ok(staged)
    }

    /// Relit les images depuis le début.
    fn frames(&self) -> Result<Box<dyn FnMut() -> Result<Option<Yuv>> + '_>> {
        let (w, h) = (self.w, self.h);
        let frame_len = Yuv::byte_len(w, h);
        let mut r = BufReader::with_capacity(1 << 22, std::fs::File::open(&self.tmp)?);
        Ok(Box::new(move || {
            let mut buf = vec![0u8; frame_len];
            match r.read_exact(&mut buf) {
                Ok(()) => Ok(Some(Yuv::from_bytes(&buf, w, h))),
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
                Err(e) => Err(e.into()),
            }
        }))
    }
}

/// Nom de fichier libre : `base.ext`, sinon `base_001.ext`, `base_002.ext`...
pub fn free_path(dir: &Path, base: &str, ext: &str, overwrite: bool) -> Result<PathBuf> {
    let first = dir.join(format!("{base}.{ext}"));
    if overwrite || !first.exists() {
        return Ok(first);
    }
    (1..100_000)
        .map(|i| dir.join(format!("{base}_{i:03}.{ext}")))
        .find(|p| !p.exists())
        .ok_or_else(|| anyhow!("{}", trf!("trop de fichiers « {base} » dans {}", "too many “{base}” files in {}", dir.display())))
}

/// Nom de fichier sans caractères interdits par Windows ni extension.
pub fn clean_name(name: &str, exts: &[&str], default: &str) -> String {
    let mut n: String = name.trim().chars().map(|c| if r#"<>:"/\|?*"#.contains(c) || c.is_control() { '_' } else { c }).collect();
    let lower = n.to_ascii_lowercase();
    for e in exts {
        let dot = format!(".{e}");
        if lower.ends_with(&dot) {
            n.truncate(n.len() - dot.len());
            break;
        }
    }
    let n = n.trim().trim_end_matches('.').to_string();
    if n.is_empty() {
        default.into()
    } else {
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::Speed;

    /// Images de test : un dégradé qui défile et un carré qui se déplace.
    fn frames(w: u32, h: u32, n: usize) -> Vec<Vec<u8>> {
        (0..n)
            .map(|k| {
                let mut v = vec![0u8; (w * h * 4) as usize];
                for y in 0..h {
                    for x in 0..w {
                        let i = ((y * w + x) * 4) as usize;
                        v[i] = ((x + k as u32 * 3) % 256) as u8;
                        v[i + 1] = ((y * 2) % 256) as u8;
                        v[i + 2] = 128;
                        v[i + 3] = 255;
                        let (cx, cy) = (40 + k as u32 * 4, h / 2);
                        if x.abs_diff(cx) < 20 && y.abs_diff(cy) < 20 {
                            v[i..i + 3].copy_from_slice(&[255, 255, 255]);
                        }
                    }
                }
                v
            })
            .collect()
    }

    fn run_job(job: Job, imgs: &[Vec<u8>]) -> PathBuf {
        let mut e = Exporter::start(job).unwrap();
        for f in imgs {
            e.push_blocking(f.clone()).unwrap();
        }
        e.close_input();
        e.finish().unwrap()
    }

    fn out_dir() -> PathBuf {
        let d = std::env::temp_dir().join("planet_studio_export_tests");
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn video_modes() {
        let (w, h, n) = (320u32, 180u32, 48usize);
        let imgs = frames(w, h, n);
        for codec in [Codec::H264, Codec::Av1] {
            for mode in RateMode::ALL {
                if !codec.supports(mode) {
                    continue;
                }
                for rate in [FrameRate::new(30, 1), FrameRate::new(24000, 1001)] {
                    let params = VideoParams {
                        codec,
                        width: w,
                        height: h,
                        rate,
                        mode,
                        target_bps: 800_000,
                        max_bps: 1_200_000,
                        crf: codec.crf_range().2,
                        speed: Speed::Fast,
                    };
                    let path = out_dir().join(format!("{codec:?}_{mode:?}_{}.mp4", rate.num));
                    let p = run_job(Job::Video { path, params }, &imgs);
                    let size = std::fs::metadata(&p).unwrap().len();
                    let secs = n as f64 / rate.fps();
                    let kbps = size as f64 * 8.0 / secs / 1000.0;
                    println!("{codec:?} {mode:?} {} i/s : {size} octets, {kbps:.0} kbit/s", rate.label());
                    assert!(size > 1000);
                    assert!(!p.with_extension("2passes.tmp").exists());
                }
            }
        }
    }

    /// Contenu difficile (bruit) : le débit obtenu doit suivre le débit demandé.
    #[test]
    fn h264_rate_control() {
        let (w, h, n) = (640u32, 360u32, 60usize);
        let mut seed = 12345u32;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let imgs: Vec<Vec<u8>> = frames(w, h, n)
            .into_iter()
            .map(|mut f| {
                for p in f.chunks_exact_mut(4) {
                    let d = (rnd() % 60) as i32 - 30;
                    for c in &mut p[..3] {
                        *c = (*c as i32 + d).clamp(0, 255) as u8;
                    }
                }
                f
            })
            .collect();
        for (mode, target) in [(RateMode::Cbr, 2_000_000u32), (RateMode::Vbr1, 2_000_000), (RateMode::Vbr2, 2_000_000), (RateMode::Vbr2, 4_000_000)] {
            let params = VideoParams {
                codec: Codec::H264,
                width: w,
                height: h,
                rate: FrameRate::new(30, 1),
                mode,
                target_bps: target,
                max_bps: target * 3 / 2,
                crf: 20,
                speed: Speed::Medium,
            };
            let p = run_job(Job::Video { path: out_dir().join(format!("rc_{mode:?}_{target}.mp4")), params }, &imgs);
            let kbps = std::fs::metadata(&p).unwrap().len() as f64 * 8.0 / (n as f64 / 30.0) / 1000.0;
            println!("H264 {mode:?} cible {} kbit/s : {kbps:.0} kbit/s", target / 1000);
            assert!(kbps > target as f64 / 1000.0 * 0.6 && kbps < target as f64 / 1000.0 * 1.5, "{mode:?} {kbps}");
        }
    }

    /// Chaque encodeur ffmpeg qui marche sur cette machine, dans chaque mode de débit
    /// (ignoré sans ffmpeg).
    #[test]
    fn ffmpeg_encoders() {
        let Some(ff) = ffmpeg::probe() else {
            println!("ffmpeg absent : test ignoré");
            return;
        };
        let (w, h, n) = (640u32, 360u32, 60usize);
        let imgs = frames(w, h, n);
        for codec in [Codec::H264, Codec::Av1] {
            for backend in ffmpeg::Backend::FFMPEG {
                if !ff.has(codec, backend) {
                    println!("{codec:?} {backend:?} : indisponible");
                    continue;
                }
                for mode in RateMode::ALL {
                    if !codec.supports(mode) {
                        continue;
                    }
                    let params = VideoParams {
                        codec,
                        width: w,
                        height: h,
                        rate: FrameRate::new(30000, 1001),
                        mode,
                        target_bps: 2_000_000,
                        max_bps: 3_000_000,
                        crf: codec.crf_range().2,
                        speed: Speed::Medium,
                    };
                    let path = out_dir().join(format!("ff_{codec:?}_{backend:?}_{mode:?}.mp4"));
                    let job = Job::Ffmpeg { path, params, exe: ff.exe.clone(), backend };
                    let p = run_job(job, &imgs);
                    let size = std::fs::metadata(&p).unwrap().len();
                    let kbps = size as f64 * 8.0 / (n as f64 / params.rate.fps()) / 1000.0;
                    println!("{codec:?} {backend:?} {mode:?} : {kbps:.0} kbit/s");
                    assert!(size > 1000);
                    assert!(!p.with_extension("2passes.tmp").exists());
                    assert!(ffmpeg::pass_log_files(&p.with_extension("2passes")).iter().all(|f| !f.exists()));
                }
            }
        }
    }

    #[test]
    fn gif_and_images() {
        let (w, h) = (160u32, 90u32);
        let imgs = frames(w, h, 10);
        let p = run_job(Job::Gif { path: out_dir().join("anim.gif"), width: w, height: h, rate: FrameRate::new(24, 1), repeat: true }, &imgs);
        assert!(std::fs::metadata(p).unwrap().len() > 1000);
        for f in ImageFormat::ALL {
            let path = out_dir().join(format!("img.{}", f.ext()));
            write_image(&path, w, h, &imgs[0], f, 90).unwrap();
            assert!(std::fs::metadata(&path).unwrap().len() > 100, "{f:?}");
        }
        let dir = out_dir().join("seq");
        std::fs::create_dir_all(&dir).unwrap();
        run_job(Job::Sequence { dir: dir.clone(), base: "img".into(), format: ImageFormat::Jpeg, quality: 90, width: w, height: h }, &imgs);
        assert!(dir.join("img_00009.jpg").exists());
    }

    #[test]
    fn names() {
        assert_eq!(clean_name(" film.MP4 ", &["mp4"], "x"), "film");
        assert_eq!(clean_name("a/b:c", &[], "x"), "a_b_c");
        assert_eq!(clean_name("  ", &[], "x"), "x");
        assert_eq!(FrameRate::from_fps(29.97), FrameRate::new(30000, 1001));
        assert_eq!(FrameRate::new(24000, 1001).label(), "23,976");
        assert_eq!(FrameRate::new(120000, 1001).label(), "119,88");
        let s = video::two_pass_schedule(&[100, 100, 400, 400, 100, 100], 2.0, 1000, 1500);
        assert_eq!(s.len(), 3);
        assert!(s[1].1 > s[0].1 && s[1].1 <= 1500, "{s:?}");
        let avg = s.iter().map(|x| x.1 as f64).sum::<f64>() / 3.0;
        assert!((avg - 1000.0).abs() < 5.0, "{s:?}");
    }
}
