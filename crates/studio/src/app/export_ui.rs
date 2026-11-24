//! Fenêtre « Exporter », dans l'esprit d'Adobe Media Encoder : onglets Vidéo et Image,
//! réglages à gauche, aperçu de la caméra de prise à droite (transport, repères
//! d'entrée et de sortie), résumé et bouton d'export en bas.

use super::{post_fx_controls, projects_dir, StudioApp};
use crate::export::{self, Exporter, ImageFormat, Job, VideoFormat};
use crate::ffmpeg::{self, Backend, Ffmpeg};
use crate::post::PostFx;
use crate::render::Lines;
use crate::video::{Codec, FrameRate, RateMode, Speed, VideoParams};
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use glam::Vec3;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

// ------------------------------------------------------------------ réglages

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(super) enum ExportTab {
    Video,
    Image,
}

/// Images exportées d'une vidéo.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum RangeMode {
    /// toute la timeline
    Full,
    /// entre les repères d'entrée et de sortie
    InOut,
    /// début et fin saisis
    Custom,
}

impl RangeMode {
    const ALL: [RangeMode; 3] = [RangeMode::Full, RangeMode::InOut, RangeMode::Custom];
    fn label(self) -> &'static str {
        match self {
            RangeMode::Full => tr!("Plage entière (timeline)", "Full range (timeline)"),
            RangeMode::InOut => tr!("Entrée / Sortie", "In / Out"),
            RangeMode::Custom => tr!("Personnalisée", "Custom"),
        }
    }
}

/// Réglages d'export, gardés dans la configuration de l'application.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct ExportSettings {
    pub tab: ExportTab,
    pub dir: String,
    pub video: VideoSettings,
    pub image: ImageSettings,
    /// garder les proportions en changeant la largeur ou la hauteur du format de prise
    pub shot_lock: bool,
    /// vitesses d'encodage mesurées sur cette machine (mégapixels par seconde), par passe,
    /// codec et vitesse : estiment la durée des 2 passes avant qu'elles commencent
    pub encode_speeds: BTreeMap<String, f32>,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            tab: ExportTab::Video,
            dir: projects_dir().join("rendu").display().to_string(),
            video: VideoSettings::default(),
            image: ImageSettings::default(),
            shot_lock: true,
            encode_speeds: BTreeMap::new(),
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct VideoSettings {
    pub format: VideoFormat,
    /// nom du fichier (sans extension) ; préfixe des images d'une séquence
    pub name: String,
    /// taille de la vidéo : format de prise de la caméra × ce facteur (mêmes proportions)
    pub scale: f32,
    /// `None` : celle du projet
    pub rate: Option<FrameRate>,
    pub mode: RateMode,
    pub target_mbps: f32,
    pub max_mbps: f32,
    pub crf_h264: u8,
    pub crf_av1: u8,
    pub speed: Speed,
    /// encodeur des vidéos MP4 : intégré, ou ffmpeg (processeur ou carte graphique)
    pub encoder: Backend,
    pub seq_format: ImageFormat,
    pub seq_quality: u8,
    pub gif_loop: bool,
    pub range: RangeMode,
    pub custom: (i32, i32),
    pub grid: bool,
}

impl Default for VideoSettings {
    fn default() -> Self {
        Self {
            format: VideoFormat::H264,
            name: "film".into(),
            scale: 1.0,
            rate: None,
            mode: RateMode::Vbr1,
            target_mbps: 10.0,
            max_mbps: 15.0,
            crf_h264: Codec::H264.crf_range().2,
            crf_av1: Codec::Av1.crf_range().2,
            speed: Speed::Medium,
            encoder: Backend::Builtin,
            seq_format: ImageFormat::Png,
            seq_quality: 92,
            gif_loop: true,
            range: RangeMode::Full,
            custom: (0, 120),
            grid: false,
        }
    }
}

impl VideoSettings {
    fn crf(&self, codec: Codec) -> u8 {
        match codec {
            Codec::H264 => self.crf_h264,
            Codec::Av1 => self.crf_av1,
        }
    }
    /// Débits conseillés pour la taille, la fréquence et le codec.
    fn recommend(&mut self, (w, h): (u32, u32), fps: f64) {
        let Some(codec) = self.format.codec() else { return };
        let px = (w * h) as f64 / (1920.0 * 1080.0);
        let base = if codec == Codec::H264 { 10.0 } else { 6.0 };
        let t = (base * px.powf(0.8) * (fps / 30.0).powf(0.6) * 2.0).round() / 2.0;
        let t = t.clamp(0.5, codec.max_mbps() as f64) as f32;
        self.target_mbps = t;
        self.max_mbps = (t * 1.5).min(codec.max_mbps());
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct ImageSettings {
    pub format: ImageFormat,
    /// qualité JPEG (1..100)
    pub quality: u8,
    pub name: String,
    /// taille de l'image : format de prise de la caméra × ce facteur (mêmes proportions)
    pub scale: f32,
    /// faux : `photo_001`, `photo_002`... si le fichier existe déjà
    pub overwrite: bool,
    pub grid: bool,
}

impl Default for ImageSettings {
    fn default() -> Self {
        Self {
            format: ImageFormat::Png,
            quality: 92,
            name: "photo".into(),
            // Full HD × 2 = 4K
            scale: 2.0,
            overwrite: false,
            grid: false,
        }
    }
}

/// Nom affiché d'un format de `SHOT_FORMATS`.
fn shot_format_name(name: &'static str) -> &'static str {
    if !crate::i18n::en() {
        return name;
    }
    match name {
        "Ultra-large 21:9" => "Ultrawide 21:9",
        "Ultra-large 21:9 (3440)" => "Ultrawide 21:9 (3440)",
        "Cinéma 2,39:1" => "Cinema 2.39:1",
        "Carré 1:1" => "Square 1:1",
        "A4 paysage (√2:1)" => "A4 landscape (√2:1)",
        n => n,
    }
}

/// Formats de prise de vue de la caméra (cadre de la vue « Prise », taille ×1 des exports).
const SHOT_FORMATS: &[(&str, u32, u32)] = &[
    ("HD 720p 16:9", 1280, 720),
    ("Full HD 1080p 16:9", 1920, 1080),
    ("QHD 1440p 16:9", 2560, 1440),
    ("4K UHD 16:9", 3840, 2160),
    ("Ultra-large 21:9", 2560, 1080),
    ("Ultra-large 21:9 (3440)", 3440, 1440),
    ("Cinéma 2,39:1", 1920, 804),
    ("Vertical 9:16 (Shorts, Reels, TikTok)", 1080, 1920),
    ("Carré 1:1", 1080, 1080),
    ("Portrait 4:5 (Instagram)", 1080, 1350),
    ("Photo 3:2", 1800, 1200),
    ("A4 paysage (√2:1)", 1754, 1240),
];

/// Facteurs de taille proposés pour les exports (× le format de prise).
const SCALES: &[f32] = &[0.5, 1.0, 1.5, 2.0, 3.0, 4.0];

const MAX_W: u32 = 15360;
const MAX_H: u32 = 8640;

/// Format de prise × facteur, mêmes proportions, dans les limites de taille.
fn scaled((cw, ch): (u32, u32), scale: f32) -> (u32, u32) {
    let s = (scale.max(0.01) as f64).min(MAX_W as f64 / cw as f64).min(MAX_H as f64 / ch as f64);
    (((cw as f64 * s).round() as u32).clamp(16, MAX_W), ((ch as f64 * s).round() as u32).clamp(16, MAX_H))
}

fn scale_label(f: f32) -> String {
    if (f - 1.0).abs() < 1e-4 {
        tr!("×1, format de la caméra", "×1, camera format").into()
    } else if f.fract().abs() < 1e-4 {
        format!("×{}", f.round() as u32)
    } else {
        format!("×{}", fr_num(f as f64, 2).trim_end_matches('0'))
    }
}

// ------------------------------------------------------------------ état

/// Repère déplacé sur la barre de l'aperçu.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Drag {
    Playhead,
    In,
    Out,
}

/// Clé de l'aperçu : il est refait quand elle change.
type PreviewKey = (u32, (u32, u32), [u32; 7], PostFx, bool);

#[derive(Default)]
struct Preview {
    tex: Option<egui::TextureHandle>,
    key: Option<PreviewKey>,
    at: Option<Instant>,
    /// taille d'affichage voulue (pixels physiques), mesurée à l'image précédente
    want: (u32, u32),
}

/// État de la fenêtre et de l'export en cours.
#[derive(Default)]
pub(super) struct ExportState {
    pub open: bool,
    /// repères d'entrée et de sortie (images de la timeline, incluses), enregistrés dans le projet
    pub marks: Option<(i32, i32)>,
    exporter: Option<Exporter>,
    /// image rendue en attente d'une place dans la file du fil d'écriture
    pending: Option<Vec<u8>>,
    started: Option<Instant>,
    /// prochaine image à rendre (indice dans l'export)
    current: Option<usize>,
    count: usize,
    /// image de la timeline de l'image 0, et pas entre deux images exportées
    first: f64,
    step: f64,
    /// taille, suréchantillonnage et grille de l'export en cours (figés à son lancement)
    size: Option<(u32, u32)>,
    supersample: u32,
    grid: bool,
    two_pass: bool,
    /// clé des vitesses d'encodage de l'export en cours (codec et vitesse), et pixels par image
    speed_key: String,
    pixels: f64,
    /// étape du fil d'écriture vue à la dernière mise à jour, et l'instant où elle a commencé
    stage: usize,
    stage_at: Option<Instant>,
    /// durée mesurée de la première passe
    pass1_secs: Option<f32>,
    format: Option<VideoFormat>,
    output: PathBuf,
    message: String,
    error: bool,
    drag: Option<Drag>,
    preview: Preview,
    ffmpeg: FfmpegState,
    /// téléchargement de ffmpeg en cours
    download: Option<(Arc<ffmpeg::DownloadProgress>, std::thread::JoinHandle<anyhow::Result<PathBuf>>)>,
    download_error: Option<String>,
    /// après un téléchargement : choisir le meilleur encodeur une fois ffmpeg essayé
    pick_encoder: bool,
    /// exports mis en file d'attente, et la file en cours d'export
    queue: Vec<QueueItem>,
    queue_run: Option<QueueRun>,
}

/// Ce qui fait l'image d'un export, mémorisé à son ajout dans la file d'attente : scène
/// (acteurs et leur animation, clés de caméra, soleil, ambiance), éclairage, post-traitement,
/// particules, grille, fréquence et plage de la timeline. Le décor (carte) n'en fait pas partie.
#[derive(Clone)]
struct ProjectState {
    scene: crate::scene::Snapshot,
    lighting: crate::environment::Lighting,
    post_fx: PostFx,
    particles: crate::particles::Particles,
    grid_yaw: f32,
    grid_origin: Vec3,
    fps: f32,
    start: i32,
    end: i32,
    marks: Option<(i32, i32)>,
}

/// Export en file d'attente.
struct QueueItem {
    /// fichier produit (nom seul)
    file: String,
    details: String,
    state: ProjectState,
    video: VideoSettings,
    dir: String,
    status: QueueStatus,
}

#[derive(Clone, PartialEq)]
enum QueueStatus {
    Waiting,
    Running,
    Done(String),
    Failed(String),
    Stopped,
}

/// File d'attente en cours d'export.
struct QueueRun {
    /// export en cours (indice dans la file)
    current: Option<usize>,
    /// projet et réglages de l'utilisateur, remis à la fin
    restore: ProjectState,
    video: VideoSettings,
    dir: String,
    started: Instant,
    stop: bool,
}

/// Recherche de ffmpeg (en arrière-plan, à la première ouverture de la fenêtre).
#[derive(Default)]
enum FfmpegState {
    #[default]
    NotSearched,
    Searching(std::thread::JoinHandle<Option<Ffmpeg>>),
    Done(Option<Ffmpeg>),
}

impl ExportState {
    pub fn busy(&self) -> bool {
        self.current.is_some() || self.exporter.is_some()
    }

    pub fn queue_running(&self) -> bool {
        self.queue_run.is_some()
    }

    /// Lance la recherche de ffmpeg si besoin et en relève le résultat.
    fn poll_ffmpeg(&mut self) {
        match &self.ffmpeg {
            FfmpegState::NotSearched => self.ffmpeg = FfmpegState::Searching(std::thread::spawn(ffmpeg::probe)),
            FfmpegState::Searching(h) if h.is_finished() => {
                let FfmpegState::Searching(h) = std::mem::take(&mut self.ffmpeg) else { unreachable!() };
                self.ffmpeg = FfmpegState::Done(h.join().ok().flatten());
            }
            _ => {}
        }
    }

    fn ffmpeg(&self) -> Option<&Ffmpeg> {
        match &self.ffmpeg {
            FfmpegState::Done(f) => f.as_ref(),
            _ => None,
        }
    }

    fn ffmpeg_searching(&self) -> bool {
        matches!(self.ffmpeg, FfmpegState::NotSearched | FfmpegState::Searching(_))
    }

    /// Lance le téléchargement de ffmpeg (à la demande de l'utilisateur).
    fn start_ffmpeg_download(&mut self) {
        if self.download.is_some() {
            return;
        }
        let progress = Arc::new(ffmpeg::DownloadProgress::default());
        let p = progress.clone();
        let handle = std::thread::spawn(move || ffmpeg::download(&p));
        self.download = Some((progress, handle));
        self.download_error = None;
    }

    /// Relève la fin du téléchargement de ffmpeg : il est alors cherché et essayé de nouveau.
    fn poll_ffmpeg_download(&mut self) {
        if !self.download.as_ref().is_some_and(|(_, h)| h.is_finished()) {
            return;
        }
        let (_, handle) = self.download.take().unwrap();
        match handle.join() {
            Ok(Ok(_)) => {
                self.ffmpeg = FfmpegState::NotSearched;
                self.pick_encoder = true;
            }
            Ok(Err(e)) => self.download_error = Some(format!("{e:#}")),
            Err(_) => self.download_error = Some(tr!("le téléchargement a planté", "the download crashed").into()),
        }
    }

    /// Ligne « ffmpeg » sous le choix de l'encodeur : version trouvée, ou proposition de le
    /// télécharger (seulement s'il est introuvable).
    fn ffmpeg_status_ui(&mut self, ui: &mut egui::Ui) {
        if self.ffmpeg_searching() {
            hint(ui, tr!("Recherche de ffmpeg et des encodeurs de la carte graphique…", "Looking for ffmpeg and the graphics card's encoders…"));
            return;
        }
        if let Some(f) = self.ffmpeg() {
            let (version, exe) = (f.version.clone(), f.exe.display().to_string());
            ui.horizontal(|ui| {
                hint(ui, &trf!("ffmpeg {version} trouvé.", "ffmpeg {version} found."));
                if ui.small_button("↻").on_hover_text(trf!("{exe}\nChercher de nouveau", "{exe}\nSearch again")).clicked() {
                    self.ffmpeg = FfmpegState::NotSearched;
                }
            });
            return;
        }
        let pkg = &ffmpeg::PACKAGE;
        let mo = |b: u64| fr_num(b as f64 / 1e6, 0);
        if let Some((p, _)) = &self.download {
            let got = p.bytes.load(Ordering::Relaxed);
            let text = match p.stage.load(Ordering::Relaxed) {
                0 => trf!("Téléchargement de ffmpeg : {} / {} Mo", "Downloading ffmpeg: {} / {} MB", mo(got), mo(pkg.size)),
                1 => tr!("Vérification de l'archive…", "Checking the archive…").into(),
                _ => tr!("Installation…", "Installing…").into(),
            };
            let cancel = ui
                .horizontal(|ui| {
                    ui.add(egui::ProgressBar::new(got as f32 / pkg.size as f32).text(text).desired_width(330.0));
                    ui.small_button(tr!("Annuler", "Cancel")).clicked()
                })
                .inner;
            if cancel {
                p.cancel.store(true, Ordering::Relaxed);
            }
            return;
        }
        hint(ui, tr!("ffmpeg introuvable : l'encodage par la carte graphique (bien plus rapide) n'est pas disponible.", "ffmpeg not found: encoding on the graphics card (much faster) is not available."));
        let mut rescan = false;
        ui.horizontal(|ui| {
            let label = trf!("⬇ Télécharger ffmpeg {} ({} Mo)", "⬇ Download ffmpeg {} ({} MB)", pkg.version, mo(pkg.size));
            let tip = trf!(
                "Source : {}\nVérifié par son empreinte SHA-256, puis installé pour votre compte seulement :\n{}\n\
                 ffmpeg est un logiciel libre distinct (licence GPL), sa licence est copiée à côté.",
                "Source: {}\nVerified by its SHA-256 checksum, then installed for your account only:\n{}\nffmpeg is separate free software (GPL license); its license is copied next to it.",
                pkg.source,
                ffmpeg::install_dir().map(|d| d.display().to_string()).unwrap_or_default()
            );
            if ui.button(label).on_hover_text(tip).clicked() {
                self.start_ffmpeg_download();
            }
            rescan = ui.small_button("↻").on_hover_text(tr!("Chercher de nouveau (ffmpeg installé à la main)", "Search again (ffmpeg installed by hand)")).clicked();
        });
        if rescan {
            self.ffmpeg = FfmpegState::NotSearched;
        }
        if let Some(e) = &self.download_error {
            ui.colored_label(Color32::from_rgb(240, 110, 90), trf!("Échec : {e}", "Failed: {e}"));
        }
    }
}

// ------------------------------------------------------------------ outils

fn fr_num(v: f64, decimals: usize) -> String {
    format!("{v:.decimals$}").replace('.', tr!(",", "."))
}

fn fmt_bytes(b: f64) -> String {
    if b >= 1e9 {
        format!("{} {}", fr_num(b / 1e9, 2), tr!("Go", "GB"))
    } else if b >= 1e6 {
        format!("{} {}", fr_num(b / 1e6, 1), tr!("Mo", "MB"))
    } else {
        format!("{} {}", fr_num(b / 1e3, 0), tr!("Ko", "KB"))
    }
}

/// Durée lisible : « 45 s », « 3 min 05 s », « 2 h 30 ».
fn fmt_dur(secs: f32) -> String {
    let s = secs.max(0.0).round() as u64;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 {
        format!("{h} h {m:02}")
    } else if m >= 10 {
        format!("{m} min")
    } else if m > 0 {
        format!("{m} min {s:02} s")
    } else {
        format!("{s} s")
    }
}

/// Temps restant estimé d'un export.
enum Eta {
    /// pas encore assez d'images pour estimer
    Unknown,
    /// fin de l'export entier
    Total(f32),
    /// fin du rendu seulement : la vitesse des 2 passes n'est pas encore connue
    RenderOnly(f32),
}

/// Code temporel HH:MM:SS:II.
fn timecode(frame: f32, fps: f32) -> String {
    let fps_i = fps.round().max(1.0) as i64;
    let f = frame.round() as i64;
    let (sign, f) = if f < 0 { ("-", -f) } else { ("", f) };
    let secs = f / fps_i;
    format!("{sign}{:02}:{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60, f % fps_i)
}

fn ratio_label(w: u32, h: u32) -> String {
    fn gcd(a: u32, b: u32) -> u32 {
        if b == 0 {
            a
        } else {
            gcd(b, a % b)
        }
    }
    let g = gcd(w, h).max(1);
    let (a, b) = (w / g, h / g);
    if a <= 64 && b <= 64 {
        format!("{a}:{b}")
    } else {
        format!("{}:1", fr_num(w as f64 / h.max(1) as f64, 2))
    }
}

/// Réduit une image (plus proche voisin) pour l'aperçu.
fn shrink(rgba: &[u8], w: u32, h: u32, max_w: u32, max_h: u32) -> (Vec<u8>, u32, u32) {
    let s = (max_w as f32 / w as f32).min(max_h as f32 / h as f32).min(1.0);
    let (ow, oh) = (((w as f32 * s) as u32).max(1), ((h as f32 * s) as u32).max(1));
    let mut out = vec![0u8; (ow * oh * 4) as usize];
    for y in 0..oh {
        let sy = (y as u64 * h as u64 / oh as u64) as u32;
        for x in 0..ow {
            let sx = (x as u64 * w as u64 / ow as u64) as u32;
            let i = ((sy * w + sx) * 4) as usize;
            let o = ((y * ow + x) * 4) as usize;
            out[o..o + 4].copy_from_slice(&rgba[i..i + 4]);
        }
    }
    (out, ow, oh)
}

/// Titre de section, à la manière des panneaux d'Adobe.
fn section(ui: &mut egui::Ui, title: &str, open: bool, add: impl FnOnce(&mut egui::Ui)) {
    egui::CollapsingHeader::new(egui::RichText::new(title).strong().size(14.0))
        .id_salt(("export_section", title))
        .default_open(open)
        .show(ui, |ui| {
            ui.add_space(2.0);
            add(ui);
            ui.add_space(4.0);
        });
}

fn hint(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(egui::RichText::new(text).small().weak()).wrap());
}

fn size_info(ui: &mut egui::Ui, w: u32, h: u32) {
    ui.label(
        egui::RichText::new(format!("{}  ·  {} Mpx", ratio_label(w, h), fr_num(w as f64 * h as f64 / 1e6, 1)))
            .small()
            .weak(),
    );
}

/// Format de prise de vue : préréglages, largeur × hauteur, proportions verrouillables.
/// `compact` : préréglages dans une liste déroulante (sinon en liste, pour un menu).
/// Renvoie le nouveau format s'il a changé.
pub(super) fn shot_format_ui(ui: &mut egui::Ui, format: [u32; 2], lock: &mut bool, compact: bool) -> Option<[u32; 2]> {
    let [mut w, mut h] = format;
    let current = SHOT_FORMATS.iter().find(|p| p.1 == w && p.2 == h);
    let mut presets = |ui: &mut egui::Ui| {
        for &(name, pw, ph) in SHOT_FORMATS {
            if ui.selectable_label(w == pw && h == ph, format!("{}  —  {pw} × {ph}", shot_format_name(name))).clicked() {
                w = pw;
                h = ph;
            }
        }
    };
    if compact {
        let text = current.map(|p| format!("{}  —  {} × {}", shot_format_name(p.0), p.1, p.2)).unwrap_or_else(|| tr!("Personnalisé", "Custom").into());
        egui::ComboBox::from_id_salt("shot_format_preset").selected_text(text).width(280.0).show_ui(ui, |ui| {
            presets(ui);
            ui.separator();
            let _ = ui.selectable_label(current.is_none(), tr!("Personnalisé (largeur × hauteur ci-dessous)", "Custom (width × height below)"));
        });
    } else {
        presets(ui);
        ui.separator();
    }
    ui.horizontal(|ui| {
        let aspect = w as f64 / h.max(1) as f64;
        let rw = ui.add(crate::calc::drag(&mut w).range(16..=MAX_W).suffix(" px").speed(4.0)).on_hover_text(tr!("Largeur", "Width"));
        let link = if *lock { "🔗" } else { "⛓" };
        ui.toggle_value(lock, link).on_hover_text(tr!("Garder les proportions", "Keep proportions"));
        let rh = ui.add(crate::calc::drag(&mut h).range(16..=MAX_H).suffix(" px").speed(4.0)).on_hover_text(tr!("Hauteur", "Height"));
        if *lock && rw.changed() {
            h = ((w as f64 / aspect).round() as u32).clamp(16, MAX_H);
        } else if *lock && rh.changed() {
            w = ((h as f64 * aspect).round() as u32).clamp(16, MAX_W);
        }
    });
    size_info(ui, w, h);
    ([w, h] != format).then_some([w, h])
}

/// Taille d'un export : facteur du format de prise (proportions toujours celles de la caméra).
fn scale_ui(ui: &mut egui::Ui, id: &str, cam: (u32, u32), scale: &mut f32) -> bool {
    let mut changed = false;
    let (w, h) = scaled(cam, *scale);
    ui.label(tr!("Taille", "Size"));
    let preset = SCALES.iter().any(|f| (f - *scale).abs() < 1e-4);
    let text = if preset { format!("{}  —  {w} × {h}", scale_label(*scale)) } else { trf!("Personnalisée  —  {w} × {h}", "Custom  —  {w} × {h}") };
    egui::ComboBox::from_id_salt((id, "scale")).selected_text(text).width(250.0).show_ui(ui, |ui| {
        for &f in SCALES {
            let (fw, fh) = scaled(cam, f);
            if ui.selectable_label((f - *scale).abs() < 1e-4, format!("{}  —  {fw} × {fh}", scale_label(f))).clicked() {
                *scale = f;
                changed = true;
            }
        }
    });
    ui.end_row();
    ui.label(tr!("Largeur", "Width"));
    ui.horizontal(|ui| {
        let mut nw = w;
        if ui
            .add(crate::calc::drag(&mut nw).range(16..=MAX_W).suffix(" px").speed(4.0))
            .on_hover_text(tr!("La hauteur suit les proportions de la caméra de prise", "The height follows the shot camera's proportions"))
            .changed()
        {
            *scale = nw as f32 / cam.0 as f32;
            changed = true;
        }
        ui.label(format!("× {} px", scaled(cam, *scale).1));
    });
    ui.end_row();
    ui.label("");
    size_info(ui, w, h);
    ui.end_row();
    changed
}

// ------------------------------------------------------------------ fenêtre

impl StudioApp {
    /// Format de prise de vue de la caméra (cadre de la vue « Prise »), enregistré dans le projet.
    pub(super) fn shot_format(&self) -> (u32, u32) {
        let [w, h] = self.scene.camera.format;
        (w.clamp(16, MAX_W), h.clamp(16, MAX_H))
    }

    /// Réglage du format de prise (barre d'outils et fenêtre d'export) ; annulable.
    pub(super) fn shot_format_editor(&mut self, ui: &mut egui::Ui, compact: bool) {
        let mut lock = self.config.export.shot_lock;
        if let Some(f) = shot_format_ui(ui, self.scene.camera.format, &mut lock, compact) {
            self.begin_edit();
            self.scene.camera.format = f;
            self.export.preview.key = None;
        }
        // fin d'un glisser : l'annulation regroupe tout le geste
        if self.editing && !ui.ctx().input(|i| i.pointer.any_down()) {
            self.editing = false;
        }
        if lock != self.config.export.shot_lock {
            self.config.export.shot_lock = lock;
            self.config.save();
        }
    }

    /// Taille de la vidéo exportée.
    fn video_size(&self) -> (u32, u32) {
        scaled(self.shot_format(), self.config.export.video.scale)
    }

    /// Taille de la photo.
    fn image_size(&self) -> (u32, u32) {
        scaled(self.shot_format(), self.config.export.image.scale)
    }

    /// Taille de l'image de l'onglet actif (mêmes proportions que la caméra de prise).
    fn out_size(&self) -> (u32, u32) {
        match self.config.export.tab {
            ExportTab::Video => self.video_size(),
            ExportTab::Image => self.image_size(),
        }
    }

    /// Ouvre la fenêtre sur un onglet.
    pub(super) fn open_export(&mut self, tab: ExportTab) {
        self.export.open = true;
        self.config.export.tab = tab;
    }

    fn export_rate(&self) -> FrameRate {
        self.config.export.video.rate.unwrap_or_else(|| FrameRate::from_fps(self.fps))
    }

    /// Images de la timeline exportées (incluses).
    fn export_range(&self) -> (i32, i32) {
        let v = &self.config.export.video;
        match v.range {
            RangeMode::Full => (self.start, self.end),
            RangeMode::InOut => self.export.marks.unwrap_or((self.start, self.end)),
            RangeMode::Custom => (v.custom.0, v.custom.1.max(v.custom.0)),
        }
    }

    /// Nombre d'images exportées et pas entre elles (en images de la timeline).
    fn export_frames(&self) -> (usize, f64) {
        let (a, b) = self.export_range();
        let span = (b - a + 1).max(1) as f64;
        let step = self.fps.max(0.1) as f64 / self.export_rate().fps();
        if (step - 1.0).abs() < 1e-6 {
            (span as usize, 1.0)
        } else {
            (((span / step).floor() as usize).max(1), step)
        }
    }

    fn ground_grid(&self) -> Lines {
        let mut lines = Lines::default();
        let n = 40;
        let h = n as f32 * 0.5;
        let (rot, o) = (self.grid_rotation(), self.grid_origin);
        for i in -n..=n {
            let g = i as f32 * 0.5;
            lines.line(o + rot * Vec3::new(g, 0.0, -h), o + rot * Vec3::new(g, 0.0, h), [0.3, 0.3, 0.33, 0.6], false);
            lines.line(o + rot * Vec3::new(-h, 0.0, g), o + rot * Vec3::new(h, 0.0, g), [0.3, 0.3, 0.33, 0.6], false);
        }
        lines
    }

    /// Problèmes qui empêchent l'export vidéo.
    fn video_errors(&self) -> Vec<String> {
        let v = &self.config.export.video;
        let (w, h) = self.video_size();
        let mut e = Vec::new();
        if let Some(c) = v.format.codec() {
            if !c.fits(w, h) {
                e.push(tr!("H.264 s'arrête à 3840 × 2160 (4K) : choisissez AV1 pour une vidéo plus grande.", "H.264 stops at 3840 × 2160 (4K): choose AV1 for a larger video.").into());
            }
            if v.encoder != Backend::Builtin {
                if self.export.ffmpeg_searching() {
                    e.push(tr!("Recherche de ffmpeg en cours…", "Looking for ffmpeg…").into());
                } else if !self.export.ffmpeg().is_some_and(|f| f.has(c, v.encoder)) {
                    e.push(trf!("Encodeur « {} » indisponible sur cette machine.", "Encoder “{}” is not available on this machine.", v.encoder.label(c)));
                }
            }
        }
        if v.format == VideoFormat::Gif && (w > 65535 || h > 65535) {
            e.push(tr!("GIF : 65 535 pixels de côté au plus.", "GIF: 65,535 pixels per side at most.").into());
        }
        if v.range == RangeMode::Custom && v.custom.1 < v.custom.0 {
            e.push(tr!("La fin de la plage doit être après son début.", "The end of the range must be after its start.").into());
        }
        if self.config.export.dir.trim().is_empty() {
            e.push(tr!("Choisissez un dossier de sortie.", "Choose an output folder.").into());
        }
        e
    }

    pub(super) fn export_ui(&mut self, ctx: &egui::Context) {
        if self.export.queue_running() {
            self.queue_modal(ctx);
        }
        if !self.export.open {
            return;
        }
        self.export.poll_ffmpeg_download();
        self.export.poll_ffmpeg();
        if self.export.ffmpeg_searching() || self.export.download.is_some() {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
        if self.export.pick_encoder {
            if let FfmpegState::Done(found) = &self.export.ffmpeg {
                // ffmpeg vient d'être installé : la carte graphique d'abord, sinon le processeur
                let v = &mut self.config.export.video;
                let codec = v.format.codec().unwrap_or(Codec::H264);
                if let Some(b) = found.as_ref().and_then(|f| Backend::FFMPEG.into_iter().find(|&b| f.has(codec, b))) {
                    v.encoder = b;
                }
                self.export.pick_encoder = false;
            }
        }
        let mut open = true;
        let before = self.config.export.clone();
        let busy = self.export.busy();
        egui::Window::new(tr!("Exporter", "Export"))
            .id(egui::Id::new("export_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let tab = &mut self.config.export.tab;
                    let message = &mut self.export.message;
                    ui.add_enabled_ui(!busy, |ui| {
                        for (t, label) in [(ExportTab::Video, tr!("🎬  Vidéo", "🎬  Video")), (ExportTab::Image, "🖼  Image")] {
                            let text = egui::RichText::new(label).size(15.0);
                            if ui.add(egui::Button::new(text).selected(*tab == t).min_size(Vec2::new(110.0, 28.0))).clicked() && *tab != t {
                                *tab = t;
                                message.clear();
                            }
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new(tr!("Rendu par la caméra de prise 🎥, jamais par la caméra de travail", "Rendered by the shot camera 🎥, never by the work camera")).weak());
                    });
                });
                ui.separator();
                // la fenêtre tient dans l'écran, résumé et bouton d'export compris
                let screen = ctx.screen_rect().size();
                let footer = if busy { 150.0 } else { 104.0 };
                let body_h = (screen.y - footer - 150.0).clamp(320.0, 640.0);
                let preview_w = (screen.x - 520.0).clamp(420.0, 760.0);
                ui.horizontal_top(|ui| {
                    let left_w = 430.0;
                    ui.allocate_ui_with_layout(Vec2::new(left_w, body_h), egui::Layout::top_down(egui::Align::Min), |ui| {
                        ui.set_width(left_w);
                        ui.set_max_width(left_w);
                        egui::ScrollArea::vertical()
                            .id_salt(("export_settings", self.config.export.tab))
                            .max_height(body_h)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.add_enabled_ui(!busy, |ui| match self.config.export.tab {
                                    ExportTab::Video => self.video_settings_ui(ui),
                                    ExportTab::Image => self.image_settings_ui(ui),
                                });
                            });
                    });
                    // séparateur de hauteur fixe (celui d'egui prendrait toute la hauteur de l'écran)
                    let (sep, _) = ui.allocate_exact_size(Vec2::new(12.0, body_h), Sense::hover());
                    ui.painter().vline(sep.center().x, sep.y_range(), ui.visuals().widgets.noninteractive.bg_stroke);
                    ui.allocate_ui_with_layout(Vec2::new(preview_w, body_h), egui::Layout::top_down(egui::Align::Min), |ui| {
                        self.preview_ui(ui, ctx, body_h);
                    });
                });
                ui.separator();
                self.footer_ui(ui);
            });
        if !open {
            self.export.open = false;
            self.export.preview = Preview::default();
            if !self.export.busy() {
                self.renderer.release_export_target();
            }
        }
        if self.config.export != before {
            let ptr_down = ctx.input(|i| i.pointer.any_down());
            if !ptr_down {
                self.config.save();
            }
        } else if self.export.open && ctx.input(|i| i.pointer.any_released()) {
            // un réglage glissé est enregistré au relâchement
            self.config.save();
        }
    }

    // -------------------------------------------------------------- onglet Vidéo

    /// Section « Caméra de prise » : son format fixe les proportions de toutes les images.
    fn shot_section(&mut self, ui: &mut egui::Ui) {
        let (w, h) = self.shot_format();
        let title = trf!("Caméra de prise  —  {w} × {h} ({})", "Shot camera  —  {w} × {h} ({})", ratio_label(w, h));
        egui::CollapsingHeader::new(egui::RichText::new(title).strong().size(14.0))
            .id_salt("export_section_shot")
            .default_open(false)
            .show(ui, |ui| {
                ui.add_space(2.0);
                self.shot_format_editor(ui, true);
                hint(
                    ui,
                    tr!("Format de prise de vue du projet : le cadre de la vue « Prise » et les proportions \
                     des vidéos et des photos. Aussi dans la barre d'outils (🎥 Prise).", "The project's shot format: the frame of the “Shot” view and the proportions of videos and photos. Also in the toolbar (🎥 Shot)."),
                );
                ui.add_space(4.0);
            });
    }

    fn video_settings_ui(&mut self, ui: &mut egui::Ui) {
        self.queue_section(ui);
        self.shot_section(ui);
        let cam = self.shot_format();
        let size = self.video_size();
        let fps_project = self.fps;
        let project_rate = FrameRate::from_fps(self.fps);
        let (count, _) = self.export_frames();
        let rate = self.export_rate();
        let marks = self.export.marks;
        let (t0, t1) = (self.start, self.end);
        let (r0, r1) = self.export_range();
        let ff = self.export.ffmpeg().cloned();
        let export = &mut self.export;
        let v = &mut self.config.export.video;

        section(ui, "Format", true, |ui| {
            egui::Grid::new("v_format").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label("Format");
                let old = v.format;
                egui::ComboBox::from_id_salt("v_format_combo").selected_text(v.format.label()).width(250.0).show_ui(ui, |ui| {
                    for f in VideoFormat::ALL {
                        ui.selectable_value(&mut v.format, f, f.label()).on_hover_text(f.hint());
                    }
                });
                if v.format != old {
                    if let Some(c) = v.format.codec() {
                        if !c.supports(v.mode) {
                            v.mode = RateMode::Vbr1;
                        }
                        if old.codec().is_none() || old.codec() != Some(c) {
                            v.recommend(size, rate.fps());
                        }
                    }
                }
                ui.end_row();
                if v.format == VideoFormat::Sequence {
                    ui.label(tr!("Images", "Frames"));
                    egui::ComboBox::from_id_salt("v_seq_format").selected_text(v.seq_format.label()).width(250.0).show_ui(ui, |ui| {
                        for f in ImageFormat::ALL {
                            ui.selectable_value(&mut v.seq_format, f, f.label()).on_hover_text(f.hint());
                        }
                    });
                    ui.end_row();
                    if v.seq_format.has_quality() {
                        ui.label(tr!("Qualité JPEG", "JPEG quality"));
                        ui.add(egui::Slider::new(&mut v.seq_quality, 1..=100));
                        ui.end_row();
                    }
                }
            });
            hint(ui, v.format.hint());
        });

        section(ui, tr!("Sortie", "Output"), true, |ui| {
            egui::Grid::new("v_out").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label(tr!("Dossier", "Folder"));
                ui.add(egui::TextEdit::singleline(&mut self.config.export.dir).desired_width(290.0));
                ui.end_row();
                ui.label(if v.format == VideoFormat::Sequence { tr!("Préfixe", "Prefix") } else { tr!("Nom du fichier", "File name") });
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut v.name).desired_width(200.0));
                    let ext = match v.format {
                        VideoFormat::Sequence => format!("_00000.{}", v.seq_format.ext()),
                        f => format!(".{}", f.ext()),
                    };
                    ui.label(egui::RichText::new(ext).weak());
                });
                ui.end_row();
            });
        });

        section(ui, tr!("Vidéo", "Video"), true, |ui| {
            egui::Grid::new("v_video").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                if scale_ui(ui, "v_size", cam, &mut v.scale) {
                    v.recommend(scaled(cam, v.scale), rate.fps());
                }
                ui.label(tr!("Images/s", "Frames/s"));
                let text = match v.rate {
                    None => trf!("Projet ({})", "Project ({})", project_rate.label()),
                    Some(r) => r.label(),
                };
                let old_rate = v.rate;
                egui::ComboBox::from_id_salt("v_rate").selected_text(text).width(140.0).show_ui(ui, |ui| {
                    ui.selectable_value(&mut v.rate, None, trf!("Projet ({})", "Project ({})", project_rate.label()));
                    ui.separator();
                    for r in FrameRate::PRESETS {
                        ui.selectable_value(&mut v.rate, Some(r), r.label());
                    }
                });
                if v.rate != old_rate {
                    v.recommend(size, v.rate.unwrap_or(project_rate).fps());
                }
                ui.end_row();
                if let Some(c) = v.format.codec() {
                    ui.label(tr!("Encodage", "Encoding"));
                    egui::ComboBox::from_id_salt("v_speed").selected_text(v.speed.label()).width(140.0).show_ui(ui, |ui| {
                        for s in Speed::ALL {
                            ui.selectable_value(&mut v.speed, s, s.label());
                        }
                    })
                    .response
                    .on_hover_text(match c {
                        Codec::H264 => tr!("Effort de compression : « Lente » donne une image un peu meilleure au même débit", "Compression effort: “Slow” gives a slightly better picture at the same bitrate"),
                        Codec::Av1 => tr!("Effort de compression : l'AV1 « Lente » peut être très long en 4K et 8K", "Compression effort: “Slow” AV1 can take very long in 4K and 8K"),
                    });
                    ui.end_row();
                    ui.label(tr!("Encodeur", "Encoder"));
                    egui::ComboBox::from_id_salt("v_encoder").selected_text(v.encoder.label(c)).width(250.0).show_ui(ui, |ui| {
                        ui.selectable_value(&mut v.encoder, Backend::Builtin, Backend::Builtin.label(c))
                            .on_hover_text(Backend::Builtin.hint());
                        for b in Backend::FFMPEG {
                            let ok = ff.as_ref().is_some_and(|f| f.has(c, b));
                            if ok || v.encoder == b {
                                ui.add_enabled_ui(ok, |ui| {
                                    ui.selectable_value(&mut v.encoder, b, b.label(c)).on_hover_text(b.hint());
                                });
                            }
                        }
                    });
                    ui.end_row();
                }
            });
            if v.format.codec().is_some() {
                export.ffmpeg_status_ui(ui);
                if v.encoder != Backend::Builtin && v.mode == RateMode::Vbr2 && !v.encoder.true_two_pass() {
                    hint(ui, tr!("VBR 2 passes sur la carte graphique : l'analyse se fait pendant l'encodage, sans second passage.", "2-pass VBR on the graphics card: the analysis happens during encoding, without a second run."));
                }
            }
            let size = scaled(cam, v.scale);
            if v.format.codec() == Some(Codec::H264) && !Codec::H264.fits(size.0, size.1) {
                ui.colored_label(Color32::from_rgb(240, 110, 90), tr!("H.264 : 3840 × 2160 au maximum.", "H.264: 3840 × 2160 at most."));
                if ui.button(tr!("Passer en AV1 (jusqu'à 8K)", "Switch to AV1 (up to 8K)")).clicked() {
                    v.format = VideoFormat::Av1;
                    if !Codec::Av1.supports(v.mode) {
                        v.mode = RateMode::Vbr1;
                    }
                    v.recommend(size, rate.fps());
                }
            }
            if v.format == VideoFormat::Gif && rate.fps() > 50.0 {
                hint(ui, tr!("GIF : 50 images/s au plus ; au-delà, les navigateurs ralentissent l'animation.", "GIF: 50 fps at most; above that, browsers slow the animation down."));
            }
            if (rate.fps() - fps_project as f64).abs() > 1e-3 {
                hint(
                    ui,
                    &trf!(
                        "Le projet est à {} images/s : l'animation est rééchantillonnée ({} images exportées), sa durée ne change pas.",
                        "The project runs at {} fps: the animation is resampled ({} frames exported), its duration does not change.",
                        project_rate.label(),
                        count
                    ),
                );
            }
        });

        if let Some(codec) = v.format.codec() {
            section(ui, tr!("Débit", "Bitrate"), true, |ui| {
                egui::Grid::new("v_rate_grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label(tr!("Encodage du débit", "Rate control"));
                    egui::ComboBox::from_id_salt("v_mode").selected_text(v.mode.label()).width(200.0).show_ui(ui, |ui| {
                        for m in RateMode::ALL {
                            let ok = codec.supports(m);
                            ui.add_enabled_ui(ok, |ui| {
                                ui.selectable_value(&mut v.mode, m, m.label())
                                    .on_hover_text(m.hint())
                                    .on_disabled_hover_text(tr!("L'encodeur AV1 ne fait pas de débit constant", "The AV1 encoder does not do constant bitrate"));
                            });
                        }
                    });
                    ui.end_row();
                    if v.mode.uses_bitrate() {
                        ui.label(tr!("Débit cible", "Target bitrate"));
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::Slider::new(&mut v.target_mbps, 0.5..=codec.max_mbps())
                                    .logarithmic(true)
                                    .suffix(" Mbit/s")
                                    .max_decimals(1),
                            );
                        });
                        ui.end_row();
                        let vbr = matches!(v.mode, RateMode::Vbr1 | RateMode::Vbr2);
                        if vbr && codec.supports_max_bitrate() {
                            ui.label(tr!("Débit maximal", "Maximum bitrate"));
                            ui.add(
                                egui::Slider::new(&mut v.max_mbps, v.target_mbps..=codec.max_mbps())
                                    .logarithmic(true)
                                    .suffix(" Mbit/s")
                                    .max_decimals(1),
                            )
                            .on_hover_text(tr!("Plafond des passages les plus complexes", "Ceiling for the most complex scenes"));
                            ui.end_row();
                        }
                        ui.label("");
                        if ui.small_button(tr!("Débits conseillés", "Recommended bitrates")).on_hover_text(tr!("Selon la taille, la fréquence et le codec", "Based on size, frame rate and codec")).clicked() {
                            v.recommend(scaled(cam, v.scale), rate.fps());
                        }
                        ui.end_row();
                    } else {
                        let (lo, hi, _) = codec.crf_range();
                        ui.label("CRF");
                        let crf = match codec {
                            Codec::H264 => &mut v.crf_h264,
                            Codec::Av1 => &mut v.crf_av1,
                        };
                        ui.add(egui::Slider::new(crf, lo..=hi)).on_hover_text(tr!("Plus bas = meilleure qualité, fichier plus lourd", "Lower = better quality, larger file"));
                        ui.end_row();
                        let c = *crf as f32 / hi as f32;
                        let word = if c <= 0.34 {
                            tr!("excellente (fichier lourd)", "excellent (large file)")
                        } else if c <= 0.45 {
                            tr!("très bonne", "very good")
                        } else if c <= 0.56 {
                            tr!("bonne", "good")
                        } else if c <= 0.7 {
                            tr!("moyenne", "average")
                        } else {
                            tr!("faible (fichier léger)", "low (small file)")
                        };
                        ui.label("");
                        ui.label(egui::RichText::new(trf!("Qualité {word}", "Quality: {word}")).weak());
                        ui.end_row();
                    }
                });
                if v.max_mbps < v.target_mbps {
                    v.max_mbps = v.target_mbps;
                }
                hint(ui, v.mode.hint());
                if v.mode.uses_bitrate() && codec == Codec::Av1 && v.mode != RateMode::Crf {
                    hint(ui, tr!("AV1 : le débit est une moyenne, sans plafond.", "AV1: the bitrate is an average, with no ceiling."));
                }
            });
        }

        section(ui, tr!("Plage", "Range"), true, |ui| {
            egui::Grid::new("v_range").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label(tr!("Exporter", "Export"));
                egui::ComboBox::from_id_salt("v_range_combo").selected_text(v.range.label()).width(200.0).show_ui(ui, |ui| {
                    for r in RangeMode::ALL {
                        ui.selectable_value(&mut v.range, r, r.label());
                    }
                });
                ui.end_row();
                match v.range {
                    RangeMode::Full => {
                        ui.label("");
                        ui.label(egui::RichText::new(trf!("Images {t0} à {t1} de la timeline", "Frames {t0} to {t1} of the timeline")).weak());
                        ui.end_row();
                    }
                    RangeMode::InOut => {
                        ui.label("");
                        match marks {
                            Some((a, b)) => ui.label(egui::RichText::new(trf!("Entrée {a}  ·  Sortie {b}", "In {a}  ·  Out {b}")).weak()),
                            None => ui.label(egui::RichText::new(tr!("Aucun repère : toute la timeline", "No marks: the whole timeline")).weak()),
                        };
                        ui.end_row();
                    }
                    RangeMode::Custom => {
                        ui.label(tr!("Images", "Frames"));
                        ui.horizontal(|ui| {
                            ui.add(crate::calc::drag(&mut v.custom.0).prefix(tr!("de ", "from ")));
                            let lo = v.custom.0;
                            ui.add(crate::calc::drag(&mut v.custom.1).prefix(tr!("à ", "to ")).range(lo..=i32::MAX));
                            if ui.small_button("= timeline").clicked() {
                                v.custom = (t0, t1);
                            }
                        });
                        ui.end_row();
                    }
                }
                ui.label(tr!("Durée", "Duration"));
                let secs = (r1 - r0 + 1).max(1) as f64 / fps_project.max(0.1) as f64;
                let plural = if count > 1 { "s" } else { "" };
                ui.label(trf!("{} s  ·  {count} image{plural} à {} i/s", "{} s  ·  {count} frame{plural} at {} fps", fr_num(secs, 2), rate.label()));
                ui.end_row();
            });
            if v.range == RangeMode::InOut && marks.is_none() {
                hint(ui, tr!("Placez l'entrée et la sortie avec les boutons { Entrée et Sortie } sous l'aperçu, ou en glissant sur sa barre de temps.", "Set the in and out points with the { In and Out } buttons under the preview, or by dragging on its time bar."));
            }
        });

        section(ui, "Options", true, |ui| {
            ui.checkbox(&mut v.grid, tr!("Afficher la grille au sol", "Show the ground grid"));
            if v.format == VideoFormat::Gif {
                ui.checkbox(&mut v.gif_loop, tr!("Lire en boucle", "Loop"));
            }
        });

        self.post_section(ui, false);
    }

    // -------------------------------------------------------------- onglet Image

    fn image_settings_ui(&mut self, ui: &mut egui::Ui) {
        self.shot_section(ui);
        let cam = self.shot_format();
        let frame = self.cur_frame();
        let i = &mut self.config.export.image;
        section(ui, "Format", true, |ui| {
            egui::Grid::new("i_format").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label("Format");
                egui::ComboBox::from_id_salt("i_format_combo").selected_text(i.format.label()).width(250.0).show_ui(ui, |ui| {
                    for f in ImageFormat::ALL {
                        ui.selectable_value(&mut i.format, f, f.label()).on_hover_text(f.hint());
                    }
                });
                ui.end_row();
                if i.format.has_quality() {
                    ui.label(tr!("Qualité", "Quality"));
                    ui.add(egui::Slider::new(&mut i.quality, 1..=100)).on_hover_text(tr!("90 et plus : pas de défaut visible", "90 and above: no visible artifacts"));
                    ui.end_row();
                }
            });
            hint(ui, i.format.hint());
            hint(
                ui,
                tr!("RAW : format des appareils photo (données brutes du capteur), sans équivalent pour une image 3D. \
                 Pour garder toute l'image sans perte, prenez TIFF ou PNG.", "RAW: a camera format (raw sensor data), with no equivalent for a 3D image. To keep the whole image lossless, choose TIFF or PNG."),
            );
        });

        section(ui, tr!("Sortie", "Output"), true, |ui| {
            egui::Grid::new("i_out").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label(tr!("Dossier", "Folder"));
                ui.add(egui::TextEdit::singleline(&mut self.config.export.dir).desired_width(290.0));
                ui.end_row();
                ui.label(tr!("Nom du fichier", "File name"));
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut i.name).desired_width(200.0));
                    ui.label(egui::RichText::new(format!(".{}", i.format.ext())).weak());
                });
                ui.end_row();
                ui.label("");
                let mut numbered = !i.overwrite;
                ui.checkbox(&mut numbered, tr!("Ne pas écraser : ajouter _001, _002…", "Don't overwrite: add _001, _002…"))
                    .on_hover_text(tr!("Décoché : une image du même nom est remplacée", "Unchecked: an image with the same name is replaced"));
                i.overwrite = !numbered;
                ui.end_row();
            });
        });

        section(ui, tr!("Taille", "Size"), true, |ui| {
            egui::Grid::new("i_size").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                scale_ui(ui, "i_size", cam, &mut i.scale);
            });
        });

        section(ui, "Image", true, |ui| {
            ui.label(trf!("Image {frame} de la timeline : choisissez-la avec les commandes sous l'aperçu.", "Frame {frame} of the timeline: pick it with the controls under the preview."));
            ui.checkbox(&mut i.grid, tr!("Afficher la grille au sol", "Show the ground grid"));
        });

        self.post_section(ui, true);
    }

    /// Post-traitement (réglages du projet) et qualité du rendu des exports.
    fn post_section(&mut self, ui: &mut egui::Ui, open: bool) {
        section(ui, tr!("Post-traitement", "Post-processing"), open, |ui| {
            post_fx_controls(ui, &mut self.post_fx);
            hint(ui, tr!("Réglages du projet : la vue, les photos et les vidéos les partagent. L'aperçu les montre aussitôt.", "Project settings: the viewport, photos and videos share them. The preview shows them right away."));
            ui.add_space(6.0);
            ui.label(egui::RichText::new(tr!("Qualité du rendu", "Render quality")).strong());
            let mut q = self.config.quality;
            let mut final_exports = !self.config.exports_like_view;
            ui.checkbox(&mut final_exports, tr!("« Rendu final » (sinon : qualité de la vue)", "“Final render” (otherwise: viewport quality)"))
                .on_hover_text(tr!("Ombres, textures et effets au plus haut pour l'export, quelle que soit la qualité de la vue", "Shadows, textures and effects at their highest for the export, whatever the viewport quality"));
            ui.horizontal(|ui| {
                ui.label(tr!("Suréchantillonnage", "Supersampling"));
                for n in 1..=4u32 {
                    ui.selectable_value(&mut q.supersample, n, format!("{n}×"));
                }
            })
            .response
            .on_hover_text(tr!("Rendu N fois plus grand puis réduit : contours et détails plus fins, export plus lent", "Rendered N times larger then scaled down: finer edges and details, slower export"));
            if q != self.config.quality || final_exports == self.config.exports_like_view {
                self.config.quality = q;
                self.config.exports_like_view = !final_exports;
                self.config.save();
            }
        });
    }

    // -------------------------------------------------------------- aperçu

    fn preview_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, body_h: f32) {
        let busy = self.export.busy();
        let video = self.config.export.tab == ExportTab::Video;
        let (ow, oh) = self.out_size();
        let aspect = ow as f32 / oh as f32;
        let controls_h = 84.0;
        ui.set_width(ui.available_width());
        let area = Vec2::new(ui.available_width(), (body_h - controls_h).max(160.0));
        let (rect, _) = ui.allocate_exact_size(area, Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, Color32::from_gray(16));
        let mut img = rect.shrink(8.0);
        if img.width() / img.height() > aspect {
            img = Rect::from_center_size(img.center(), Vec2::new(img.height() * aspect, img.height()));
        } else {
            img = Rect::from_center_size(img.center(), Vec2::new(img.width(), img.width() / aspect));
        }
        let ppp = ctx.pixels_per_point();
        // rendu de l'aperçu à la taille affichée (pas plus que l'image exportée ni que 1280 px)
        let s = (img.width() * ppp / ow as f32).min(img.height() * ppp / oh as f32).min(1.0).min(1280.0 / ow as f32);
        self.export.preview.want = (((ow as f32 * s) as u32).max(16), ((oh as f32 * s) as u32).max(16));
        if !busy {
            self.update_preview(ctx);
        }
        match &self.export.preview.tex {
            Some(tex) => {
                painter.image(tex.id(), img, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
            }
            None => {
                painter.rect_filled(img, 0.0, Color32::from_gray(30));
            }
        }
        painter.rect_stroke(img, 0.0, Stroke::new(1.0_f32, Color32::from_gray(70)), egui::StrokeKind::Outside);
        let info = if busy {
            let what = match self.export.current {
                Some(k) => trf!("Export en cours : image {} / {}", "Exporting: frame {} / {}", k + 1, self.export.count),
                None => tr!("Encodage en cours…", "Encoding…").into(),
            };
            match self.eta() {
                Eta::Total(rest) => trf!("{what}  ·  fin dans {}", "{what}  ·  done in {}", fmt_dur(rest)),
                _ => what,
            }
        } else {
            trf!("Aperçu  ·  {ow} × {oh}  ·  image {}", "Preview  ·  {ow} × {oh}  ·  frame {}", self.cur_frame())
        };
        let galley_pos = img.min + Vec2::new(6.0, 6.0);
        let text_rect = painter.text(galley_pos, egui::Align2::LEFT_TOP, &info, egui::FontId::proportional(12.0), Color32::TRANSPARENT);
        painter.rect_filled(text_rect.expand(4.0), 3.0, Color32::from_black_alpha(150));
        painter.text(galley_pos, egui::Align2::LEFT_TOP, info, egui::FontId::proportional(12.0), Color32::from_gray(230));

        ui.add_space(6.0);
        ui.add_enabled_ui(!busy, |ui| {
            self.scrubber_ui(ui, video);
            ui.add_space(4.0);
            self.transport_ui(ui, video);
        });
    }

    /// Refait l'aperçu quand l'image, le cadrage ou le post-traitement change (et deux fois
    /// par seconde, pour suivre les autres modifications de la scène).
    fn update_preview(&mut self, ctx: &egui::Context) {
        let (w, h) = self.export.preview.want;
        let view = self.real_shot(self.frame);
        let grid = match self.config.export.tab {
            ExportTab::Video => self.config.export.video.grid,
            ExportTab::Image => self.config.export.image.grid,
        };
        let vb = [
            view.eye.x.to_bits(),
            view.eye.y.to_bits(),
            view.eye.z.to_bits(),
            view.target.x.to_bits(),
            view.target.y.to_bits(),
            view.target.z.to_bits(),
            view.fov_deg.to_bits(),
        ];
        let key: PreviewKey = (self.frame.to_bits(), (w, h), vb, self.post_fx, grid);
        let p = &self.export.preview;
        let fresh = p.at.is_some_and(|t| t.elapsed() < Duration::from_millis(500));
        if p.key == Some(key) && fresh {
            ctx.request_repaint_after(Duration::from_millis(500));
            return;
        }
        let items = self.draw_items(false);
        self.set_focus(&view);
        let lines = if grid { self.ground_grid() } else { Lines::default() };
        let rgba = self.renderer.render_image(view.view_proj(w as f32 / h as f32), view.eye, &items, &lines, w, h, 1);
        self.set_preview(ctx, &rgba, w, h);
        self.export.preview.key = Some(key);
    }

    fn set_preview(&mut self, ctx: &egui::Context, rgba: &[u8], w: u32, h: u32) {
        let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba);
        let p = &mut self.export.preview;
        match &mut p.tex {
            Some(t) => t.set(img, egui::TextureOptions::LINEAR),
            None => p.tex = Some(ctx.load_texture("export_preview", img, egui::TextureOptions::LINEAR)),
        }
        p.at = Some(Instant::now());
    }

    /// Barre de temps de l'aperçu : tête de lecture, plage exportée, repères d'entrée et de sortie.
    fn scrubber_ui(&mut self, ui: &mut egui::Ui, video: bool) {
        let (t0, t1) = (self.start, self.end);
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::click_and_drag());
        let p = ui.painter_at(rect);
        let pad = 10.0;
        let span = (t1 - t0).max(1) as f32;
        let x0 = rect.min.x + pad;
        let ww = (rect.width() - 2.0 * pad).max(1.0);
        let x_of = |f: f32| x0 + (f - t0 as f32) / span * ww;
        let f_of = |x: f32| (t0 as f32 + (x - x0) / ww * span).round().clamp(t0 as f32, t1 as f32) as i32;
        p.rect_filled(rect, 4.0, Color32::from_gray(26));
        let bar = Rect::from_min_max(Pos2::new(x0, rect.center().y - 3.0), Pos2::new(x0 + ww, rect.center().y + 3.0));
        p.rect_filled(bar, 2.0, Color32::from_gray(58));
        let accent = ui.visuals().selection.bg_fill;
        if video {
            let (a, b) = self.export_range();
            let band = Rect::from_min_max(
                Pos2::new(x_of(a.max(t0) as f32), bar.min.y - 1.0),
                Pos2::new(x_of(b.min(t1) as f32), bar.max.y + 1.0),
            );
            if band.width() > 0.0 {
                p.rect_filled(band, 2.0, accent.gamma_multiply(0.85));
            }
        }
        let mark_col = Color32::from_rgb(255, 214, 90);
        if let Some((a, b)) = self.export.marks.filter(|_| video) {
            for (f, dir) in [(a, 1.0), (b, -1.0)] {
                let x = x_of(f as f32);
                let (top, bot) = (rect.min.y + 4.0, rect.max.y - 4.0);
                let st = Stroke::new(2.0_f32, mark_col);
                p.line_segment([Pos2::new(x, top), Pos2::new(x, bot)], st);
                p.line_segment([Pos2::new(x, top), Pos2::new(x + 5.0 * dir, top)], st);
                p.line_segment([Pos2::new(x, bot), Pos2::new(x + 5.0 * dir, bot)], st);
            }
        }
        // graduations
        let step = [1, 2, 5, 10, 20, 50, 100, 200, 500, 1000, 5000].into_iter().find(|s| ww / span * *s as f32 > 45.0).unwrap_or(10000);
        let mut f = (t0 as f32 / step as f32).ceil() as i32 * step;
        while f <= t1 {
            let x = x_of(f as f32);
            p.line_segment([Pos2::new(x, rect.max.y - 5.0), Pos2::new(x, rect.max.y - 1.0)], Stroke::new(1.0_f32, Color32::from_gray(100)));
            f += step;
        }
        // tête de lecture
        let x = x_of(self.frame);
        p.line_segment([Pos2::new(x, rect.min.y + 1.0), Pos2::new(x, rect.max.y - 1.0)], Stroke::new(2.0_f32, Color32::WHITE));
        p.add(egui::Shape::convex_polygon(
            vec![Pos2::new(x - 5.0, rect.min.y + 1.0), Pos2::new(x + 5.0, rect.min.y + 1.0), Pos2::new(x, rect.min.y + 7.0)],
            Color32::WHITE,
            Stroke::NONE,
        ));

        let near_mark = |pos: Pos2| -> Option<Drag> {
            let (a, b) = self.export.marks?;
            if !video {
                return None;
            }
            if (pos.x - x_of(b as f32)).abs() < 6.0 {
                Some(Drag::Out)
            } else if (pos.x - x_of(a as f32)).abs() < 6.0 {
                Some(Drag::In)
            } else {
                None
            }
        };
        if let Some(pos) = resp.hover_pos() {
            if near_mark(pos).is_some() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
        }
        let pressed = resp.drag_started() || resp.clicked() || resp.is_pointer_button_down_on();
        if pressed && self.export.drag.is_none() {
            if let Some(pos) = resp.interact_pointer_pos() {
                self.export.drag = Some(near_mark(pos).unwrap_or(Drag::Playhead));
            }
        }
        if let (Some(d), Some(pos)) = (self.export.drag, resp.interact_pointer_pos().or(resp.hover_pos()).filter(|_| pressed || resp.dragged())) {
            let f = f_of(pos.x);
            match d {
                Drag::Playhead => {
                    self.playing = false;
                    if f != self.cur_frame() || self.frame.fract() != 0.0 {
                        self.set_frame(f as f32);
                    }
                }
                Drag::In => {
                    if let Some(m) = &mut self.export.marks {
                        m.0 = f.min(m.1);
                    }
                }
                Drag::Out => {
                    if let Some(m) = &mut self.export.marks {
                        m.1 = f.max(m.0);
                    }
                }
            }
        }
        if !resp.is_pointer_button_down_on() && !resp.dragged() {
            self.export.drag = None;
        }
        resp.on_hover_text(tr!("Cliquer ou glisser : se déplacer dans le temps.\nRepères jaunes : entrée et sortie, à glisser.", "Click or drag: move through time.\nYellow marks: in and out points, draggable."));
    }

    /// Début, image précédente, lecture / pause, image suivante, fin ; repères d'entrée et de sortie.
    fn transport_ui(&mut self, ui: &mut egui::Ui, video: bool) {
        let (a, b) = if video { self.export_range() } else { (self.start, self.end) };
        ui.horizontal(|ui| {
            let big = |t: &str| egui::Button::new(egui::RichText::new(t).size(16.0)).min_size(Vec2::new(34.0, 28.0));
            if ui.add(big("⏮")).on_hover_text(if video { tr!("Début de la plage exportée", "Start of the exported range") } else { tr!("Début", "Start") }).clicked() {
                self.playing = false;
                self.set_frame(a as f32);
            }
            if ui.add(big("◀")).on_hover_text(tr!("Image précédente", "Previous frame")).clicked() {
                self.playing = false;
                self.set_frame((self.cur_frame() - 1).max(self.start) as f32);
            }
            let (icon, tip) = if self.playing { ("⏸", "Pause") } else { ("▶", tr!("Lecture", "Play")) };
            if ui.add(big(icon)).on_hover_text(tip).clicked() {
                self.toggle_play();
            }
            if ui.add(big("▶|")).on_hover_text(tr!("Image suivante", "Next frame")).clicked() {
                self.playing = false;
                self.set_frame((self.cur_frame() + 1).min(self.end) as f32);
            }
            if ui.add(big("⏭")).on_hover_text(if video { tr!("Fin de la plage exportée", "End of the exported range") } else { tr!("Fin", "End") }).clicked() {
                self.playing = false;
                self.set_frame(b as f32);
            }
            ui.separator();
            ui.label(egui::RichText::new(timecode(self.frame, self.fps)).monospace().size(14.0));
            ui.label(egui::RichText::new(trf!("image {}", "frame {}", self.cur_frame())).weak());
            if video {
                ui.separator();
                let cur = self.cur_frame();
                if ui.button(tr!("{  Entrée", "{  In")).on_hover_text(tr!("Placer l'entrée sur l'image courante", "Set the in point on the current frame")).clicked() {
                    self.mark_in(cur);
                }
                if ui.button(tr!("Sortie  }", "Out  }")).on_hover_text(tr!("Placer la sortie sur l'image courante", "Set the out point on the current frame")).clicked() {
                    self.mark_out(cur);
                }
                if self.export.marks.is_some() && ui.small_button("✖").on_hover_text(tr!("Effacer les repères", "Clear the marks")).clicked() {
                    self.clear_in_out();
                }
            }
        });
    }

    // -------------------------------------------------------------- bas de fenêtre

    fn footer_ui(&mut self, ui: &mut egui::Ui) {
        let busy = self.export.busy();
        let video = self.config.export.tab == ExportTab::Video;
        let dir = self.config.export.dir.trim().to_string();
        let errors = if video { self.video_errors() } else { Vec::new() };
        // résumé
        let summary = if video {
            let v = &self.config.export.video;
            let (count, _) = self.export_frames();
            let rate = self.export_rate();
            let secs = count as f64 / rate.fps();
            let (file, what) = match v.format {
                VideoFormat::Sequence => {
                    let base = export::clean_name(&v.name, &[], "image");
                    (format!("{base}_00000.{} …", v.seq_format.ext()), trf!("{} ({count} fichiers)", "{} ({count} files)", v.seq_format.label()))
                }
                f => {
                    let what = match f.codec() {
                        Some(c) => format!("{} · {}", f.label(), v.encoder.short(c)),
                        None => f.label().to_string(),
                    };
                    (format!("{}.{}", export::clean_name(&v.name, &[f.ext()], "film"), f.ext()), what)
                }
            };
            let (w, h) = self.video_size();
            let mut parts = vec![what, format!("{w} × {h}"), trf!("{} i/s", "{} fps", rate.label())];
            if let Some(c) = v.format.codec() {
                if v.mode.uses_bitrate() {
                    parts.push(format!("{} {} Mbit/s", v.mode.label().split(' ').next().unwrap_or(""), fr_num(v.target_mbps as f64, 1)));
                    parts.push(format!("≈ {}", fmt_bytes(v.target_mbps as f64 * 1e6 / 8.0 * secs)));
                } else {
                    parts.push(format!("CRF {}", v.crf(c)));
                }
            }
            parts.push(format!("{} s", fr_num(secs, 2)));
            (file, parts.join("  ·  "))
        } else {
            let i = &self.config.export.image;
            let (w, h) = self.image_size();
            let base = export::clean_name(&i.name, &[i.format.ext()], "photo");
            (format!("{base}.{}", i.format.ext()), trf!("{}  ·  {w} × {h}  ·  image {}", "{}  ·  {w} × {h}  ·  frame {}", i.format.label(), self.cur_frame()))
        };
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tr!("Fichier :", "File:")).strong());
            ui.label(egui::RichText::new(PathBuf::from(&dir).join(&summary.0).display().to_string()).monospace());
        });
        ui.label(egui::RichText::new(summary.1).weak());
        for e in &errors {
            ui.colored_label(Color32::from_rgb(240, 110, 90), e);
        }

        if busy {
            self.progress_ui(ui);
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if busy {
                if ui.add(egui::Button::new(tr!("■  Arrêter", "■  Stop")).min_size(Vec2::new(120.0, 30.0))).clicked() {
                    self.stop_export();
                }
            } else {
                let label = if video { tr!("▶  Exporter", "▶  Export") } else { tr!("📸  Enregistrer l'image", "📸  Save image") };
                let btn = egui::Button::new(egui::RichText::new(label).strong().size(15.0).color(Color32::WHITE))
                    .fill(ui.visuals().selection.bg_fill)
                    .min_size(Vec2::new(190.0, 32.0));
                if ui.add_enabled(errors.is_empty(), btn).clicked() {
                    if video {
                        self.start_export();
                    } else {
                        self.take_photo();
                        self.export.message = self.status.clone();
                        self.export.error = self.status.starts_with(tr!("Erreur", "Error"));
                    }
                }
                if video {
                    let add = egui::Button::new(tr!("➕ File d'attente", "➕ Queue")).min_size(Vec2::new(0.0, 32.0));
                    let tip = "Garde ce plan (caméra, animation, éclairage) avec ces réglages d'export : \
                               modifiez ensuite le projet et ajoutez-en d'autres, puis exportez toute la file";
                    if ui.add_enabled(errors.is_empty(), add).on_hover_text(tip).clicked() {
                        self.queue_add();
                    }
                    let waiting = self.export.queue.iter().filter(|q| q.status == QueueStatus::Waiting).count();
                    if waiting > 0 {
                        let run = egui::Button::new(egui::RichText::new(trf!("⏭  Exporter la file ({waiting})", "⏭  Export the queue ({waiting})")).strong())
                            .min_size(Vec2::new(0.0, 32.0));
                        if ui.add(run).on_hover_text(tr!("Exporte les vidéos en attente l'une après l'autre", "Exports the queued videos one after another")).clicked() {
                            self.queue_start();
                        }
                    }
                }
            }
            if ui.button(tr!("📂 Ouvrir le dossier", "📂 Open folder")).clicked() {
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::process::Command::new("explorer").arg(&dir).spawn();
            }
            if !self.export.message.is_empty() {
                let col = if self.export.error { Color32::from_rgb(240, 110, 90) } else { ui.visuals().text_color() };
                ui.add(egui::Label::new(egui::RichText::new(&self.export.message).color(col)).truncate());
            }
        });
    }

    fn progress_ui(&mut self, ui: &mut egui::Ui) {
        let total = self.export.count.max(1);
        let elapsed = self.export.started.map(|t| t.elapsed().as_secs_f32()).unwrap_or(0.0);
        let rendered = self.export.current.unwrap_or(total);
        ui.add(egui::ProgressBar::new(rendered as f32 / total as f32).text(trf!("Rendu : image {rendered} / {total}", "Rendering: frame {rendered} / {total}")));
        if let Some(ex) = &self.export.exporter {
            let done = ex.progress.done.load(Ordering::Relaxed);
            let stage = ex.progress.stage.load(Ordering::Relaxed);
            let rate = if elapsed > 0.5 { done as f32 / elapsed } else { 0.0 };
            let what = match (self.export.format, stage) {
                (Some(VideoFormat::Gif), _) => trf!("GIF : image {done} / {total}", "GIF: frame {done} / {total}"),
                (Some(VideoFormat::Sequence), _) => trf!("Écriture : image {done} / {total}", "Writing: frame {done} / {total}"),
                (_, 1) => trf!("Mise de côté pour les 2 passes : {done} / {total}", "Staging for the 2 passes: {done} / {total}"),
                (_, 2) => trf!("Passe 1 / 2 (analyse) : {done} / {total}", "Pass 1 / 2 (analysis): {done} / {total}"),
                (_, 3) => trf!("Passe 2 / 2 (encodage) : {done} / {total}", "Pass 2 / 2 (encoding): {done} / {total}"),
                _ => trf!("Encodage : {done} / {total} ({} images/s)", "Encoding: {done} / {total} ({} fps)", fr_num(rate as f64, 1)),
            };
            ui.add(egui::ProgressBar::new(done as f32 / total as f32).text(what));
        }
        let text = match self.eta() {
            Eta::Total(rest) => {
                trf!("⏱ Estimation : {} au total  ·  écoulé {}", "⏱ Estimate: {} in total  ·  elapsed {}", fmt_dur(elapsed + rest), fmt_dur(elapsed))
            }
            Eta::RenderOnly(rest) => trf!(
                "⏱ Rendu terminé dans {}, puis 2 passes d'encodage (durée estimée dès la 1re passe)  ·  écoulé {}",
                "⏱ Rendering done in {}, then 2 encoding passes (estimated once the 1st pass starts)  ·  elapsed {}",
                fmt_dur(rest),
                fmt_dur(elapsed)
            ),
            Eta::Unknown => trf!("⏱ Estimation en cours…  ·  écoulé {}", "⏱ Estimating…  ·  elapsed {}", fmt_dur(elapsed)),
        };
        ui.label(egui::RichText::new(text).strong());
        ui.ctx().request_repaint_after(Duration::from_millis(100));
    }

    /// Clé des vitesses d'encodage mémorisées pour une passe (1 ou 2) de l'export en cours.
    fn speed_key(&self, pass: u8) -> String {
        format!("{}:{pass}", self.export.speed_key)
    }

    /// Durée prévue d'une passe d'après les vitesses mesurées lors d'exports précédents.
    fn known_pass_secs(&self, pass: u8) -> Option<f32> {
        let mpx = self.config.export.encode_speeds.get(&self.speed_key(pass)).copied().filter(|v| *v > 0.0)?;
        Some((self.export.count as f64 * self.export.pixels / 1e6 / mpx as f64) as f32)
    }

    /// Mémorise la vitesse d'une passe qui vient de se terminer (moyennée avec la précédente).
    fn learn_pass_speed(&mut self, pass: u8, secs: f32) {
        if secs < 1.0 || self.export.count < 10 {
            return;
        }
        let mpx = (self.export.count as f64 * self.export.pixels / 1e6 / secs as f64) as f32;
        let key = self.speed_key(pass);
        let v = match self.config.export.encode_speeds.get(&key) {
            Some(old) if *old > 0.0 => (old + mpx) / 2.0,
            _ => mpx,
        };
        self.config.export.encode_speeds.insert(key, v);
        self.config.save();
    }

    /// Suit les changements d'étape du fil d'écriture (durée de chaque passe).
    fn track_stage(&mut self) {
        let Some(ex) = &self.export.exporter else { return };
        let stage = ex.progress.stage.load(Ordering::Relaxed);
        if stage == self.export.stage {
            return;
        }
        let now = Instant::now();
        if self.export.stage == 2 && stage == 3 {
            let secs = self.export.stage_at.map(|t| (now - t).as_secs_f32());
            self.export.pass1_secs = secs;
            if let Some(s) = secs {
                self.learn_pass_speed(1, s);
            }
        }
        self.export.stage = stage;
        self.export.stage_at = Some(now);
    }

    /// Temps restant de l'export en cours, d'après la vitesse depuis le début de chaque étape.
    fn eta(&self) -> Eta {
        let total = self.export.count.max(1);
        let elapsed = self.export.started.map(|t| t.elapsed().as_secs_f32()).unwrap_or(0.0);
        let Some(ex) = &self.export.exporter else { return Eta::Unknown };
        let done = ex.progress.done.load(Ordering::Relaxed).min(total);
        // reste d'une étape commencée depuis `secs` et arrivée à `done` images
        let rest = |secs: f32, done: usize| (done >= 2 && secs >= 1.0).then(|| secs / done as f32 * (total - done) as f32);
        if !self.export.two_pass {
            // rendu et écriture vont de pair : la vitesse de bout de chaîne fait foi
            return rest(elapsed, done).map_or(Eta::Unknown, Eta::Total);
        }
        let in_stage = self.export.stage_at.map(|t| t.elapsed().as_secs_f32()).unwrap_or(elapsed);
        match self.export.stage {
            // rendu (images mises de côté), puis les 2 passes
            0 | 1 => {
                let Some(render) = rest(elapsed, done) else { return Eta::Unknown };
                match self.known_pass_secs(1) {
                    Some(p1) => Eta::Total(render + p1 + self.known_pass_secs(2).unwrap_or(p1)),
                    None => Eta::RenderOnly(render),
                }
            }
            2 => {
                let p1 = rest(in_stage, done).or_else(|| self.known_pass_secs(1).map(|s| (s - in_stage).max(0.0)));
                let Some(p1) = p1 else { return Eta::Unknown };
                // seconde passe : vitesse connue, sinon aussi longue que la première
                let p2 = self.known_pass_secs(2).unwrap_or(in_stage + p1);
                Eta::Total(p1 + p2)
            }
            _ => {
                let p2 = rest(in_stage, done).or_else(|| {
                    let s = self.known_pass_secs(2).or(self.export.pass1_secs)?;
                    Some((s - in_stage).max(0.0))
                });
                p2.map_or(Eta::Unknown, Eta::Total)
            }
        }
    }

    // -------------------------------------------------------------- file d'attente

    fn capture_state(&self) -> ProjectState {
        ProjectState {
            scene: self.scene.snapshot(),
            lighting: self.lighting.clone(),
            post_fx: self.post_fx,
            particles: self.particles.clone(),
            grid_yaw: self.grid_yaw,
            grid_origin: self.grid_origin,
            fps: self.fps,
            start: self.start,
            end: self.end,
            marks: self.export.marks,
        }
    }

    fn apply_state(&mut self, s: &ProjectState) {
        self.scene.replace(&s.scene);
        self.lighting = s.lighting.clone();
        self.post_fx = s.post_fx;
        self.particles = s.particles.clone();
        self.grid_yaw = s.grid_yaw;
        self.grid_origin = s.grid_origin;
        self.fps = s.fps;
        self.start = s.start;
        self.end = s.end;
        self.export.marks = s.marks;
        self.scene.evaluate(self.frame);
        // panorama, particules et post-traitement envoyés au rendu tout de suite : le premier
        // export de la file est rendu dans cette même image
        self.sync_lighting();
        self.force_render = true;
    }

    /// Met l'export réglé en file d'attente, avec l'état actuel du projet.
    fn queue_add(&mut self) {
        if let Some(e) = self.video_errors().first() {
            self.export.message = e.clone();
            self.export.error = true;
            return;
        }
        let mut video = self.config.export.video.clone();
        let dir = self.config.export.dir.trim().to_string();
        let default = if video.format == VideoFormat::Sequence { "image" } else { "film" };
        let ext = video.format.ext();
        let clean = |v: &VideoSettings| export::clean_name(&v.name, &[ext], default);
        // deux exports de la file n'écrivent jamais le même fichier : film, film_2, film_3...
        let base = clean(&video);
        let taken = |n: &str| self.export.queue.iter().any(|q| q.dir == dir && q.video.format == video.format && clean(&q.video) == n);
        let mut name = base.clone();
        let mut k = 2;
        while taken(&name) {
            name = format!("{base}_{k}");
            k += 1;
        }
        let file = match video.format {
            VideoFormat::Sequence => format!("{name}_00000.{} …", video.seq_format.ext()),
            f => format!("{name}.{}", f.ext()),
        };
        // résumé, avec les réglages tels qu'ils sont maintenant
        let (count, _) = self.export_frames();
        let (w, h) = self.video_size();
        let rate = self.export_rate();
        let (a, b) = self.export_range();
        let what = match video.format.codec() {
            Some(c) => format!("{} · {}", video.format.label(), video.encoder.short(c)),
            None => video.format.label().to_string(),
        };
        let details = trf!("{what}  ·  {w} × {h}  ·  {} i/s  ·  images {a} à {b} ({count})", "{what}  ·  {w} × {h}  ·  {} fps  ·  frames {a} to {b} ({count})", rate.label());
        video.name = name;
        let state = self.capture_state();
        self.export.queue.push(QueueItem { file, details, state, video, dir, status: QueueStatus::Waiting });
        let waiting = self.export.queue.iter().filter(|q| q.status == QueueStatus::Waiting).count();
        self.export.message = trf!("Ajouté à la file d'attente ({waiting} en attente). Vous pouvez modifier le projet et en ajouter d'autres.", "Added to the queue ({waiting} waiting). You can change the project and add more.");
        self.export.error = false;
    }

    /// Exporte les exports en attente, l'un après l'autre.
    fn queue_start(&mut self) {
        if self.export.busy() || self.export.queue_run.is_some() {
            return;
        }
        if !self.export.queue.iter().any(|q| q.status == QueueStatus::Waiting) {
            return;
        }
        self.export.queue_run = Some(QueueRun {
            current: None,
            restore: self.capture_state(),
            video: self.config.export.video.clone(),
            dir: self.config.export.dir.clone(),
            started: Instant::now(),
            stop: false,
        });
        self.queue_next();
    }

    /// Lance le prochain export en attente, ou termine la file.
    fn queue_next(&mut self) {
        loop {
            let Some(run) = &self.export.queue_run else { return };
            if run.stop {
                break;
            }
            let Some(i) = self.export.queue.iter().position(|q| q.status == QueueStatus::Waiting) else { break };
            if let Some(run) = &mut self.export.queue_run {
                run.current = Some(i);
            }
            let q = &mut self.export.queue[i];
            q.status = QueueStatus::Running;
            let (state, video, dir) = (q.state.clone(), q.video.clone(), q.dir.clone());
            self.apply_state(&state);
            self.config.export.video = video;
            self.config.export.dir = dir;
            self.config.export.tab = ExportTab::Video;
            self.start_export();
            if self.export.busy() {
                return;
            }
            // lancement impossible (dossier, réglages) : on passe au suivant
            self.export.queue[i].status = QueueStatus::Failed(self.export.message.clone());
        }
        self.queue_finish();
    }

    /// À chaque image : l'export en cours de la file est fini, on passe au suivant.
    fn queue_tick(&mut self) {
        let Some(run) = &self.export.queue_run else { return };
        if self.export.busy() {
            return;
        }
        if let Some(q) = run.current.and_then(|i| self.export.queue.get_mut(i)) {
            if q.status == QueueStatus::Running {
                q.status = if self.export.error {
                    QueueStatus::Failed(self.export.message.clone())
                } else {
                    QueueStatus::Done(self.export.output.display().to_string())
                };
            }
        }
        self.queue_next();
    }

    /// Fin de la file : le projet et les réglages de l'utilisateur reviennent.
    fn queue_finish(&mut self) {
        let Some(run) = self.export.queue_run.take() else { return };
        self.apply_state(&run.restore);
        self.config.export.video = run.video;
        self.config.export.dir = run.dir;
        self.config.save();
        let q = &self.export.queue;
        let done = q.iter().filter(|q| matches!(q.status, QueueStatus::Done(_))).count();
        let failed = q.iter().filter(|q| matches!(q.status, QueueStatus::Failed(_))).count();
        let mut msg = if run.stop {
            trf!("File d'attente arrêtée après {} : {done} export(s) terminé(s)", "Queue stopped after {}: {done} export(s) finished", fmt_dur(run.started.elapsed().as_secs_f32()))
        } else {
            trf!("✔ File d'attente terminée en {} : {done} export(s)", "✔ Queue finished in {}: {done} export(s)", fmt_dur(run.started.elapsed().as_secs_f32()))
        };
        if failed > 0 {
            msg += &trf!(", {failed} en échec", ", {failed} failed");
        }
        self.export.message = msg;
        self.export.error = failed > 0;
        self.status = self.export.message.clone();
    }

    /// Arrête l'export en cours et la file.
    fn queue_stop(&mut self) {
        if let Some(run) = &mut self.export.queue_run {
            run.stop = true;
            if let Some(q) = run.current.and_then(|i| self.export.queue.get_mut(i)) {
                if q.status == QueueStatus::Running {
                    q.status = QueueStatus::Stopped;
                }
            }
        }
        self.stop_export();
    }

    /// Fermeture de l'application pendant la file : arrêt et retour au projet de l'utilisateur.
    pub(super) fn queue_abort(&mut self) {
        if self.export.queue_run.is_some() {
            self.queue_stop();
            self.queue_finish();
        }
    }

    /// Liste de la file ; `editable` : boutons pour retirer un export.
    fn queue_list_ui(&mut self, ui: &mut egui::Ui, editable: bool) {
        let mut remove = None;
        let accent = ui.visuals().selection.bg_fill;
        for (i, q) in self.export.queue.iter().enumerate() {
            ui.horizontal(|ui| {
                let (icon, col) = match &q.status {
                    QueueStatus::Waiting => ("⏳", ui.visuals().weak_text_color()),
                    QueueStatus::Running => ("▶", accent),
                    QueueStatus::Done(_) => ("✔", Color32::from_rgb(110, 200, 120)),
                    QueueStatus::Failed(_) => ("✖", Color32::from_rgb(240, 110, 90)),
                    QueueStatus::Stopped => ("■", ui.visuals().weak_text_color()),
                };
                ui.label(egui::RichText::new(icon).color(col).size(15.0));
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(format!("{}.  {}", i + 1, q.file)).strong());
                        if editable && q.status != QueueStatus::Running && ui.small_button("✖").on_hover_text(tr!("Retirer de la file", "Remove from the queue")).clicked() {
                            remove = Some(i);
                        }
                    });
                    ui.add(egui::Label::new(egui::RichText::new(&q.details).small().weak()).wrap());
                    match &q.status {
                        QueueStatus::Done(p) => {
                            ui.add(egui::Label::new(egui::RichText::new(trf!("Écrit : {p}", "Written: {p}")).small().weak()).wrap());
                        }
                        QueueStatus::Failed(e) => {
                            ui.add(egui::Label::new(egui::RichText::new(e).small().color(Color32::from_rgb(240, 110, 90))).wrap());
                        }
                        QueueStatus::Stopped => {
                            ui.label(egui::RichText::new(tr!("Arrêté", "Stopped")).small().weak());
                        }
                        _ => {}
                    }
                });
            });
            ui.add_space(2.0);
        }
        if let Some(i) = remove {
            self.export.queue.remove(i);
        }
    }

    /// Section « File d'attente » de l'onglet Vidéo (si elle n'est pas vide).
    fn queue_section(&mut self, ui: &mut egui::Ui) {
        if self.export.queue.is_empty() {
            return;
        }
        section(ui, tr!("File d'attente", "Queue"), true, |ui| {
            hint(
                ui,
                tr!("Chaque export garde le projet tel qu'il était à son ajout (caméra, animation, éclairage, \
                 post-traitement) ; le décor (carte) reste celui du moment de l'export.", "Each export keeps the project as it was when added (camera, animation, lighting, post-processing); the scenery (map) stays the one at export time."),
            );
            ui.add_space(2.0);
            self.queue_list_ui(ui, true);
            let finished = self.export.queue.iter().any(|q| !matches!(q.status, QueueStatus::Waiting | QueueStatus::Running));
            ui.horizontal(|ui| {
                if finished && ui.small_button(tr!("Retirer les terminés", "Remove finished")).clicked() {
                    self.export.queue.retain(|q| q.status == QueueStatus::Waiting);
                }
                if ui.small_button(tr!("Tout retirer", "Remove all")).clicked() {
                    self.export.queue.clear();
                }
            });
        });
    }

    /// Fenêtre bloquante pendant la file : avancement et arrêt.
    fn queue_modal(&mut self, ctx: &egui::Context) {
        egui::Modal::new(egui::Id::new("export_queue_modal")).show(ctx, |ui| {
            ui.set_width(620.0);
            let n = self.export.queue.len();
            let k = self.export.queue_run.as_ref().and_then(|r| r.current).map_or(0, |i| i + 1);
            ui.heading(trf!("File d'attente : export {k} / {n}", "Queue: export {k} / {n}"));
            ui.label(egui::RichText::new(tr!("Le projet est remis tel quel à la fin de la file.", "The project is restored as it was at the end of the queue.")).weak());
            ui.separator();
            egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| self.queue_list_ui(ui, false));
            ui.separator();
            if self.export.busy() {
                self.progress_ui(ui);
            }
            ui.add_space(4.0);
            if ui.add(egui::Button::new(tr!("■  Arrêter la file", "■  Stop the queue")).min_size(Vec2::new(150.0, 30.0))).clicked() {
                self.queue_stop();
            }
        });
        ctx.request_repaint_after(Duration::from_millis(100));
    }

    // -------------------------------------------------------------- export

    pub(super) fn start_export(&mut self) {
        let errors = self.video_errors();
        if let Some(e) = errors.first() {
            self.export.message = e.clone();
            self.export.error = true;
            return;
        }
        let dir = PathBuf::from(self.config.export.dir.trim());
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.export.message = trf!("Dossier invalide : {e}", "Invalid folder: {e}");
            self.export.error = true;
            return;
        }
        let v = self.config.export.video.clone();
        let rate = self.export_rate();
        let (count, step) = self.export_frames();
        let (a, _) = self.export_range();
        let (mut w, mut h) = self.video_size();
        let job = match v.format {
            VideoFormat::H264 | VideoFormat::Av1 => {
                // 4:2:0 : dimensions paires
                w &= !1;
                h &= !1;
                let codec = v.format.codec().unwrap();
                let name = export::clean_name(&v.name, &["mp4"], "film");
                let params = VideoParams {
                    codec,
                    width: w,
                    height: h,
                    rate,
                    mode: v.mode,
                    target_bps: (v.target_mbps * 1e6) as u32,
                    max_bps: (v.max_mbps.max(v.target_mbps) * 1e6) as u32,
                    crf: v.crf(codec),
                    speed: v.speed,
                };
                let path = dir.join(format!("{name}.mp4"));
                match (v.encoder, self.export.ffmpeg()) {
                    (Backend::Builtin, _) => Job::Video { path, params },
                    (backend, Some(ff)) => Job::Ffmpeg { path, params, exe: ff.exe.clone(), backend },
                    (_, None) => {
                        self.export.message = tr!("ffmpeg introuvable", "ffmpeg not found").into();
                        self.export.error = true;
                        return;
                    }
                }
            }
            VideoFormat::Gif => {
                let name = export::clean_name(&v.name, &["gif"], "film");
                Job::Gif { path: dir.join(format!("{name}.gif")), width: w, height: h, rate, repeat: v.gif_loop }
            }
            VideoFormat::Sequence => {
                let base = export::clean_name(&v.name, &[], "image");
                Job::Sequence { dir: dir.clone(), base, format: v.seq_format, quality: v.seq_quality, width: w, height: h }
            }
        };
        self.export.output = match &job {
            Job::Video { path, .. } | Job::Ffmpeg { path, .. } | Job::Gif { path, .. } => path.clone(),
            Job::Sequence { dir, .. } => dir.clone(),
        };
        match Exporter::start(job) {
            Ok(ex) => self.export.exporter = Some(ex),
            Err(e) => {
                self.export.message = trf!("Erreur : {e:#}", "Error: {e:#}");
                self.export.error = true;
                return;
            }
        }
        self.config.save();
        self.playing = false;
        self.export.message.clear();
        self.export.error = false;
        self.export.format = Some(v.format);
        self.export.two_pass = v.format.codec().is_some() && v.mode == RateMode::Vbr2 && v.encoder.true_two_pass();
        self.export.speed_key = match v.encoder {
            Backend::Builtin => format!("{:?}:{:?}", v.format.codec(), v.speed),
            b => format!("{:?}:{:?}:{b:?}", v.format.codec(), v.speed),
        };
        self.export.pixels = w as f64 * h as f64;
        self.export.stage = 0;
        self.export.pass1_secs = None;
        self.export.started = Some(Instant::now());
        self.export.stage_at = self.export.started;
        self.export.count = count;
        self.export.first = a as f64;
        self.export.step = step;
        self.export.current = Some(0);
        self.export.grid = v.grid;
        // qualité des exports avant de créer la cible (l'anticrénelage en dépend)
        let q = self.export_quality();
        q.apply(&mut self.renderer, self.lighting.shadows);
        self.export.supersample = q.export_supersample();
        self.export.size = Some((w, h));
    }

    fn stop_export(&mut self) {
        self.export.current = None;
        self.export.size = None;
        self.export.pending = None;
        self.renderer.release_export_target();
        if let Some(mut ex) = self.export.exporter.take() {
            // le fil termine ce qu'il a reçu : vidéo partielle, ou images déjà écrites
            ex.close_input();
            self.export.message = match self.export.format {
                Some(VideoFormat::Sequence) => tr!("Arrêté : les images déjà rendues sont écrites.", "Stopped: the frames already rendered are written.").into(),
                _ => tr!("Arrêté : le fichier contiendra les images déjà rendues.", "Stopped: the file will contain the frames already rendered.").into(),
            };
            self.export.error = false;
            std::thread::spawn(move || {
                let _ = ex.finish();
            });
        }
    }

    /// Avance l'export d'une image par mise à jour (l'interface reste réactive).
    pub(super) fn export_step(&mut self, ctx: &egui::Context) {
        self.queue_tick();
        self.track_stage();
        // 1) image en attente d'une place dans la file du fil d'écriture
        if let Some(img) = self.export.pending.take() {
            let Some(ex) = &self.export.exporter else { return };
            match ex.try_push(img) {
                Ok(None) => {}
                Ok(Some(img)) => {
                    self.export.pending = Some(img);
                    ctx.request_repaint_after(Duration::from_millis(10));
                    return;
                }
                Err(e) => {
                    self.fail_export(e);
                    return;
                }
            }
        }
        // 2) toutes les images sont envoyées : attendre la fin de l'écriture
        if self.export.current.is_none() {
            if self.export.exporter.as_ref().is_some_and(|e| e.is_finished()) {
                let ex = self.export.exporter.take().unwrap();
                let secs = self.export.started.map(|t| t.elapsed().as_secs_f32()).unwrap_or(0.0);
                let r = ex.finish();
                if r.is_ok() && self.export.two_pass && self.export.stage == 3 {
                    if let Some(t) = self.export.stage_at {
                        self.learn_pass_speed(2, t.elapsed().as_secs_f32());
                    }
                }
                self.export.error = r.is_err();
                self.export.message = match (r, self.export.format) {
                    (Ok(p), Some(VideoFormat::Sequence)) => {
                        trf!("✔ {} images écrites en {secs:.0} s dans {}", "✔ {} images written in {secs:.0} s to {}", self.export.count, p.display())
                    }
                    (Ok(p), _) => trf!("✔ Écrit en {secs:.0} s : {}", "✔ Written in {secs:.0} s: {}", p.display()),
                    (Err(e), _) => trf!("Erreur d'écriture : {e:#}", "Error while writing: {e:#}"),
                };
                self.status = self.export.message.clone();
            } else if self.export.exporter.is_some() {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
            return;
        }
        // 3) rendu de l'image courante
        let Some(k) = self.export.current else { return };
        let Some((w, h)) = self.export.size else {
            self.export.current = None;
            return;
        };
        let f = (self.export.first + k as f64 * self.export.step) as f32;
        self.scene.evaluate(f);
        self.apply_lighting_at(f);
        let view = self.real_shot(f);
        self.set_focus(&view);
        let items = self.draw_items(false);
        let lines = if self.export.grid { self.ground_grid() } else { Lines::default() };
        let s = self.export.supersample.max(1);
        let rgba = self.renderer.render_image(view.view_proj(w as f32 / h as f32), view.eye, &items, &lines, w, h, s);
        drop(items);
        // l'aperçu montre l'image rendue (quelques fois par seconde)
        if self.export.open && self.export.preview.at.is_none_or(|t| t.elapsed() > Duration::from_millis(250)) {
            let (pw, ph) = self.export.preview.want;
            let (small, sw, sh) = shrink(&rgba, w, h, pw.max(64), ph.max(64));
            self.set_preview(ctx, &small, sw, sh);
            self.export.preview.key = None;
        }
        self.export.pending = Some(rgba);
        if k + 1 >= self.export.count {
            self.export.current = None;
            self.export.size = None;
            self.renderer.release_export_target();
            // envoyer la dernière image puis fermer l'entrée
            if let (Some(ex), Some(img)) = (&self.export.exporter, self.export.pending.take()) {
                if let Err(e) = ex.push_blocking(img) {
                    self.fail_export(e);
                    return;
                }
            }
            if let Some(ex) = self.export.exporter.as_mut() {
                ex.close_input();
            }
            self.export.message = if self.export.two_pass { tr!("Encodage en deux passes…", "Two-pass encoding…").into() } else { tr!("Finalisation…", "Finishing…").into() };
        } else {
            self.export.current = Some(k + 1);
        }
        self.scene.evaluate(self.frame);
        ctx.request_repaint();
    }

    fn fail_export(&mut self, e: anyhow::Error) {
        // l'erreur du fil d'écriture est plus parlante que « l'export s'est arrêté »
        let msg = match self.export.exporter.take().map(|ex| ex.finish()) {
            Some(Err(inner)) => trf!("Erreur : {inner:#}", "Error: {inner:#}"),
            _ => trf!("Erreur : {e:#}", "Error: {e:#}"),
        };
        self.export.message = msg;
        self.export.error = true;
        self.export.current = None;
        self.export.size = None;
        self.export.pending = None;
        self.renderer.release_export_target();
    }

    /// Photo : l'image courante vue par la caméra de prise, avec les réglages de l'onglet Image.
    pub(super) fn take_photo(&mut self) {
        if self.export.busy() {
            self.status = tr!("Un export est en cours : photo impossible pour l'instant", "An export is running: no photo for now").into();
            return;
        }
        let i = self.config.export.image.clone();
        let (w, h) = self.image_size();
        let dir = PathBuf::from(self.config.export.dir.trim());
        let r = (|| -> anyhow::Result<PathBuf> {
            std::fs::create_dir_all(&dir)?;
            let base = export::clean_name(&i.name, &[i.format.ext()], "photo");
            let path = export::free_path(&dir, &base, i.format.ext(), i.overwrite)?;
            let view = self.real_shot(self.frame);
            // qualité des exports le temps de la photo (la vue reprend la sienne à l'image suivante)
            let q = self.export_quality();
            q.apply(&mut self.renderer, self.lighting.shadows);
            let s = q.export_supersample();
            let items = self.draw_items(false);
            self.set_focus(&view);
            let lines = if i.grid { self.ground_grid() } else { Lines::default() };
            let rgba = self.renderer.render_image(view.view_proj(w as f32 / h as f32), view.eye, &items, &lines, w, h, s);
            // la cible d'une grande photo occupe beaucoup de mémoire graphique
            self.renderer.release_export_target();
            self.export.preview.key = None;
            export::write_image(&path, w, h, &rgba, i.format, i.quality)?;
            Ok(path)
        })();
        self.status = match r {
            Ok(p) => trf!("📸 Photo {w}×{h} enregistrée : {}", "📸 {w}×{h} photo saved: {}", p.display()),
            Err(e) => trf!("Erreur de photo : {e:#}", "Error taking photo: {e:#}"),
        };
    }
}
