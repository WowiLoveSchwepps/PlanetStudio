//! Application : panneaux, viewport, gizmos, timeline.

mod asset_preview;
mod decor_ui;
mod export_ui;
mod help_ui;
mod markers;
mod shots_ui;
mod update_ui;

use crate::anim::{CamKey, Interp, Key, Track};
use crate::camera::{self, DroneSettings, OrbitCamera, ViewParams};
use crate::camrec::{self, CamRec};
use export_ui::{ExportState, ExportTab};
use crate::clips::{BakeOptions, BlendMode, BlendOptions, BodyPart, DecodedClip};
use crate::camo::{self, TextureChoice};
use crate::decal;
use crate::skins::{self, SkinChoice};
use crate::anim::{AmbKey, AmbState, LightKey, SunKey, SunState};
use crate::quality::{ParticleQuality, Preset, Quality as RenderQuality, ShadowQuality, TerrainQuality, TextureQuality};
use crate::keymap::{Action, Keymap};
use crate::environment::{preset_state, Ambiance, Lighting, SUN_PRESETS};
use crate::loader::{Loader, Msg};
use crate::render::{DrawItem, Lines, Renderer};
use crate::scene::{
    euler_deg_from_quat, quat_from_euler_deg, ActorFile, Attachment, History, ProjectFile, Scene,
};
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2 as EVec2};
use eframe::egui_wgpu;
use forgelight::{GameData, Model, Transform};
use glam::{Mat4, Quat, Vec2, Vec3};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

// ------------------------------------------------------------------ config

#[derive(Default, Serialize, Deserialize)]
struct Config {
    game_path: Option<String>,
    last_project: Option<String>,
    /// qualité graphique de la vue
    #[serde(default)]
    quality: RenderQuality,
    /// vrai : les exports gardent la qualité de la vue au lieu de « Rendu final »
    #[serde(default)]
    exports_like_view: bool,
    /// raccourcis clavier (les actions absentes gardent leur touche par défaut)
    #[serde(default)]
    keymap: Keymap,
    /// vol libre de la caméra : vitesse, sensibilité, inertie
    #[serde(default)]
    drone: DroneSettings,
    /// enregistrement automatique du projet
    #[serde(default)]
    autosave: AutoSave,
    /// réglages de la fenêtre « Exporter »
    #[serde(default)]
    export: export_ui::ExportSettings,
    /// réglages des plans de caméra prédéfinis (menu « 🎞 Plans »)
    #[serde(default)]
    shots: crate::shots::ShotSettings,
    /// mise à jour depuis les GitHub Releases
    #[serde(default)]
    update: update_ui::UpdateSettings,
    /// langue de l'interface (absente : celle du système)
    #[serde(default)]
    lang: Option<crate::i18n::Lang>,
}

/// Enregistrement automatique : toutes les `minutes` et à la fermeture de l'application.
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
struct AutoSave {
    enabled: bool,
    minutes: u32,
}

impl Default for AutoSave {
    fn default() -> Self {
        Self { enabled: true, minutes: 2 }
    }
}

fn config_path() -> PathBuf {
    forgelight::app_data_dir().join("config.json")
}

impl Config {
    fn load() -> Self {
        std::fs::read_to_string(config_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }
    fn save(&self) {
        let p = config_path();
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(p, serde_json::to_string_pretty(self).unwrap_or_default());
    }
}

/// Dossier du jeu (contenant `Resources/Assets`) ou directement le dossier des archives `.pack2`.
/// Le dossier est toujours choisi par l'utilisateur : aucune recherche automatique sur le disque.
fn is_game_dir(p: &std::path::Path) -> bool {
    p.join("Resources").join("Assets").join("data_x64_0.pack2").exists() || p.join("data_x64_0.pack2").exists()
}

/// Guide d'installation de la version 2019 de PlanetSide 2 (section du README).
const GAME_GUIDE_URL: &str = "https://github.com/WowiLoveSchwepps/PlanetStudio#installer-planetside-2-version-2019";
const GAME_GUIDE_URL_EN: &str =
    "https://github.com/WowiLoveSchwepps/PlanetStudio/blob/main/README.en.md#installing-planetside-2-2019-version";

/// Invitation au serveur Discord (menu Aide). Vide : l'entrée n'est pas affichée.
const DISCORD_URL: &str = "https://discord.gg/PmNbcgfnD";

fn projects_dir() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from);
    home.map(|h| h.join("Documents").join("PlanetStudio")).unwrap_or_else(|| PathBuf::from("."))
}

/// Premier « sans_titre_N.psproj » libre : fichier d'un projet jamais enregistré.
fn untitled_path() -> PathBuf {
    let dir = projects_dir();
    (1..).map(|n| dir.join(format!("sans_titre_{n}.psproj"))).find(|p| !p.exists()).unwrap()
}

/// Écrit le projet dans un fichier temporaire puis le renomme : une coupure en pleine écriture
/// ne peut pas abîmer le fichier existant.
fn write_project(path: &std::path::Path, json: &str) -> anyhow::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

// ------------------------------------------------------------------ types UI

#[derive(Clone, Copy, PartialEq, Eq)]
enum Category {
    Characters,
    Weapons,
    Vehicles,
    /// objets des bases que le serveur fait apparaître (absents des cartes)
    Gameplay,
    Other,
    All,
}

/// Mots des noms d'objets de base : terminaux, boucliers et portes IFF, tubes de réapparition,
/// points de capture, générateurs… Le jeu ne les place pas dans les `.zone` (c'est le serveur
/// qui les fait apparaître) : on les pose à la main.
const GAMEPLAY_WORDS: &[&str] = &[
    "globalcontrolunit",
    "terminal",
    "shield",
    "iffdoor",
    "forcefield",
    "spawntube",
    "spawncontrolunit",
    "spawnpad",
    "capturepoint",
    "capture.",
    "gravitytube",
    "jumppad",
    "generator",
    "ammodepot",
    "gameplayobjects",
    "dpo_placeables",
    "hologram",
];

impl Category {
    const ALL: [Category; 6] =
        [Category::Characters, Category::Weapons, Category::Vehicles, Category::Gameplay, Category::Other, Category::All];
    fn label(self) -> &'static str {
        match self {
            Category::Characters => tr!("Personnages", "Characters"),
            Category::Weapons => tr!("Armes", "Weapons"),
            Category::Vehicles => tr!("Véhicules", "Vehicles"),
            Category::Gameplay => tr!("Objets de base", "Base objects"),
            Category::Other => tr!("Décor / autres", "Scenery / other"),
            Category::All => tr!("Tout", "All"),
        }
    }
    fn matches(self, name: &str) -> bool {
        let l = name.to_ascii_lowercase();
        let ch = ["armor_", "head_", "helmet_", "fparmor", "character", "hair_", "facialhair"]
            .iter()
            .any(|p| l.starts_with(p));
        let wp = l.starts_with("weapon");
        let vh = l.starts_with("vehicle");
        let gp = !ch && !wp && !vh && GAMEPLAY_WORDS.iter().any(|w| l.contains(w));
        match self {
            Category::Characters => ch,
            Category::Weapons => wp,
            Category::Vehicles => vh,
            Category::Gameplay => gp,
            Category::Other => !ch && !wp && !vh && !gp,
            Category::All => true,
        }
    }
}

/// Filtre de faction des objets de base : `None` = toutes, sinon `TR`, `NC`, `VS` ou `""`
/// (neutres : sans suffixe de faction).
fn faction_matches(filter: Option<&str>, name: &str) -> bool {
    let Some(f) = filter else { return true };
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    let tag = stem.split('_').find(|t| ["TR", "NC", "VS"].iter().any(|x| t.eq_ignore_ascii_case(x)));
    match tag {
        Some(t) => t.eq_ignore_ascii_case(f),
        None => f.is_empty(),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GizmoMode {
    Rotate,
    Translate,
}

#[derive(Clone, Copy, PartialEq)]
enum DragKind {
    /// clic droit tenu : la caméra regarde autour d'elle (vol libre)
    Look,
    Orbit,
    Pan,
    RotateAxis(Vec3),
    RotateFree,
    TranslateAxis(Vec3),
    TranslateFree,
    /// personnage saisi par le corps et glissé sur le sol : il reste sous le curseur, sur le plan
    /// horizontal `height` ; `offset` = position de sa racine moins le point saisi
    MoveGround { height: f32, offset: Vec3 },
}

/// Ajout d'acteur en attente de chargement.
#[derive(Default)]
struct PendingAdd {
    attach: Option<PendingAttach>,
    file: Option<ActorFile>,
    select: bool,
    /// acteurs à charger ensuite et à lier à celui-ci (tête, casque...)
    children: Vec<(String, ChildKind)>,
    /// position imposée (troupe)
    place: Option<Transform>,
    /// animation à écrire dès l'arrivée
    dance: Option<DanceJob>,
    /// l'étape d'annulation est déjà posée (changement de skin : retrait + ajout = une étape)
    in_edit: bool,
    /// reprend le camouflage et la décalcomanie de l'acteur sur lequel il est fusionné
    inherit_look: bool,
}

/// Danse à appliquer à un ou plusieurs acteurs (dès leur chargement pour une troupe).
#[derive(Clone)]
enum DanceJob {
    /// danse du cheval, générée par IK
    Horse { start: i32, bpm: f32, secs: f32 },
    /// animation FBX importée (Mixamo), transférée sur le squelette de chaque acteur
    Fbx { start: i32, name: String, anim: Arc<forgelight::fbx::FbxAnimation>, loops: u32 },
}

impl DanceJob {
    /// Écrit la danse dans la timeline de l'acteur. Renvoie la dernière image, ou une erreur
    /// lisible (squelette non humain, os Mixamo introuvables…).
    fn bake(&self, a: &mut crate::scene::Actor, fps: f32) -> Result<i32, String> {
        match self {
            DanceJob::Horse { start, bpm, secs } => crate::dance::bake_horse_dance(a, *start, fps, *bpm, *secs)
                .ok_or_else(|| trf!("{} : pas de squelette humain complet", "{}: no complete human skeleton", a.name)),
            DanceJob::Fbx { start, name, anim, loops } => {
                let clip = crate::retarget::retarget(anim, &a.model.skeleton, name).map_err(|e| format!("{} : {e:#}", a.name))?;
                let mut last = *start;
                for _ in 0..(*loops).max(1) {
                    let opts = BakeOptions {
                        fps,
                        speed: 1.0,
                        root_start: None,
                        blend: BlendOptions { mode: BlendMode::Replace, weight: 1.0, mask: None },
                        fade_in: 0,
                        fade_out: 0,
                    };
                    last = clip.bake(a, last, &opts).0;
                }
                Ok(last)
            }
        }
    }

    fn label(&self) -> String {
        match self {
            DanceJob::Horse { .. } => tr!("la danse du cheval", "the horse dance").into(),
            DanceJob::Fbx { name, .. } => trf!("« {name} »", "“{name}”"),
        }
    }
}

#[derive(Clone)]
enum ChildKind {
    Merge,
    /// l'acteur entier suit cet os du parent (armes d'un MAX)
    Bone(String),
}

#[derive(Clone)]
enum PendingAttach {
    Merge(u64),
    Bone(u64, String),
}

/// Os copié : sa transformation de repos (pour adapter la pose à un autre gabarit), sa pose à
/// l'image copiée et toute sa piste d'animation.
#[derive(Clone)]
struct BoneClip {
    rest: Transform,
    pose: Transform,
    track: Track,
}

/// Presse-papiers de pose : un personnage entier, os repérés par leur nom (en minuscules).
struct PoseClipboard {
    /// nom du personnage d'origine
    from: String,
    /// image à laquelle la pose a été copiée
    frame: i32,
    bones: HashMap<String, BoneClip>,
    /// position / orientation monde du personnage au moment de la copie
    root_world: Mat4,
}

impl PoseClipboard {
    /// Clés d'animation présentes dans la copie.
    fn key_count(&self) -> usize {
        self.bones.values().map(|b| b.track.keys.len()).sum()
    }
}

/// Transformation d'un os source adaptée au gabarit de l'os cible : même rotation, et le
/// déplacement par rapport au repos (bassin qui monte et descend…) ajouté à la longueur d'os de
/// la cible. Entre deux squelettes identiques, c'est une copie exacte.
fn adapt_bone(src: Transform, src_rest: Transform, dst_rest: Transform) -> Transform {
    Transform { translation: dst_rest.translation + (src.translation - src_rest.translation), ..src }
}

/// Texte mis dans le presse-papiers du système à la copie d'une pose : sans lui, Windows
/// n'envoie pas Ctrl+V à l'application quand le presse-papiers est vide.
const POSE_CLIP_TEXT: &str = "PlanetStudio : pose copiée";

#[derive(Clone, Copy, PartialEq)]
enum PoseOp {
    Copy,
    Paste,
    PasteMirror,
    PasteAnim,
    PastePosition,
}

#[derive(PartialEq)]
enum Dialog {
    Save,
    Open,
}

pub struct StudioApp {
    rs: egui_wgpu::RenderState,
    renderer: Renderer,
    loader: Loader,
    config: Config,
    game: Option<Arc<GameData>>,
    game_path: String,
    game_error: Option<String>,
    /// fenêtre « Dossier de PlanetSide 2 » ouverte (choix en cours, ou changement demandé)
    game_setup: bool,
    /// mises à jour depuis les GitHub Releases
    updater: crate::update::Updater,
    /// bandeau « nouvelle version » masqué pour cette session (« Plus tard »)
    update_hidden: bool,
    /// relancer PlanetStudio à la fermeture (nouvelle version installée)
    restart_after_exit: bool,

    // navigateur
    search: String,
    category: Category,
    /// filtre de faction de la catégorie « Objets de base » (voir [`faction_matches`])
    faction_filter: Option<&'static str>,
    show_dme: bool,
    adr_names: Vec<String>,
    dme_names: Vec<String>,
    filtered: Vec<String>,
    filter_dirty: bool,
    soldier_faction: usize,
    soldier_sex: usize,
    soldier_class: usize,
    soldier_head: usize,
    /// pièce d'armure posée sur le corps de base (skins)
    soldier_armor: SkinChoice,
    soldier_helmet: SkinChoice,
    skin_filter: String,
    /// listes d'armures et de casques pour (faction, sexe, classe)
    skin_lists: Option<((usize, usize, usize), Vec<String>, Vec<String>)>,
    /// mêmes listes pour le soldat sélectionné (inspecteur)
    insp_skin_lists: Option<((&'static str, &'static str, &'static str), Vec<String>, Vec<String>)>,

    // chargements
    pending: HashMap<u64, PendingAdd>,
    next_request: u64,
    models: HashMap<String, Arc<Model>>,
    /// bulle d'aperçu au survol des listes d'assets
    asset_preview: asset_preview::AssetPreview,

    // scène
    scene: Scene,
    history: History,
    editing: bool,
    sel_actor: Option<u64>,
    sel_bone: Option<usize>,
    /// groupes (personnage + pièces fusionnées) dépliés dans la liste de la scène
    expanded_groups: HashSet<u64>,
    bone_filter: String,
    clipboard_pose: Option<PoseClipboard>,

    // temps
    frame: f32,
    fps: f32,
    start: i32,
    end: i32,
    playing: bool,
    looping: bool,
    /// enregistrement en direct armé : piloter la caméra de prise lance la lecture et l'enregistre
    cam_rec_armed: bool,
    /// prise de caméra en cours
    cam_rec: Option<CamRec>,
    interp: Interp,
    last_tick: Instant,
    /// l'image précédente n'avait reçu aucune entrée (souris, clavier, molette)
    idle_prev: bool,
    /// un chargement a modifié la scène : le prochain rendu 3D est obligatoire
    force_render: bool,
    /// rien n'a pu changer depuis la dernière image : on réutilise le rendu 3D précédent
    /// (évite de redessiner la scène à chaque déplacement de la fenêtre)
    skip_3d: bool,
    /// tailles (pixels) de la vue et du moniteur lors du dernier rendu 3D
    last_render_size: Option<((u32, u32), Option<(u32, u32)>)>,
    key_drag: Option<(i32, i32)>,
    /// clé de soleil en cours de déplacement : (image d'origine, image actuelle)
    sun_drag: Option<(i32, i32)>,
    /// clé d'ambiance en cours de déplacement
    amb_drag: Option<(i32, i32)>,
    /// zoom de la timeline : plage d'images visible (None = tout, de Début à Fin)
    tl_view: Option<(f32, f32)>,
    /// glisser dans la barre de zoom : (mode, décalage de prise en images)
    tl_bar_drag: Option<(TlBarDrag, f32)>,
    /// marqueurs de la timeline (enregistrés dans le projet)
    markers: Vec<crate::scene::Marker>,
    /// repère (entrée, sortie, marqueur) glissé dans la timeline
    tl_mark_drag: Option<markers::MarkDrag>,
    /// marqueur sous le clic droit qui a ouvert le menu de la timeline
    tl_menu_marker: Option<usize>,

    // vue
    /// caméra de travail : libre, jamais rendue
    cam: OrbitCamera,
    /// vrai : on regarde (et on pilote) la caméra de prise
    shot_view: bool,
    /// incrustation de la caméra de prise dans la vue de travail
    show_monitor: bool,
    show_skeleton: bool,
    show_all_skeletons: bool,
    show_grid: bool,
    /// rotation de la grille autour de l'axe vertical (degrés), enregistrée avec le projet
    grid_yaw: f32,
    /// origine de la grille (pieds du personnage sur lequel on l'a posée), enregistrée avec le projet
    grid_origin: Vec3,
    /// ambiance et réglages d'éclairage (enregistrés avec le projet)
    lighting: Lighting,
    /// ambiance actuellement chargée dans le renderer
    lighting_built: Ambiance,
    /// repère du soleil dans la vue de travail
    show_sun: bool,
    /// textures de camouflage du jeu, filtre de la liste, textures illisibles
    camo_catalog: Vec<String>,
    camo_filter: String,
    camo_failed: HashSet<String>,
    /// décalcomanies du jeu et filtre de la liste
    decal_catalog: Vec<String>,
    decal_filter: String,
    gizmo: GizmoMode,
    drag: Option<DragKind>,
    last_mouse: Option<Vec2>,
    /// touches de déplacement enfoncées (position physique : ZQSD en AZERTY = WASD en QWERTY)
    fly_keys: HashSet<egui::Key>,
    /// la caméra a bougé au clavier à l'image précédente
    fly_moving: bool,
    /// clé de caméra touchée par la série d'édition en cours : (image, elle existait avant)
    shot_key_touch: Option<(i32, bool)>,
    /// vitesse actuelle du vol (lissée : le drone accélère et freine en douceur)
    fly_vel: Vec3,
    /// visée au clic droit en cours : position du curseur (caché) au moment de l'appui
    look_anchor: Option<Pos2>,
    /// le système envoie les mouvements bruts de la souris (visée sans limite d'écran)
    raw_mouse: bool,
    /// dernier changement de vitesse de vol, pour l'afficher un instant dans la vue
    speed_toast: Option<Instant>,
    hover_axis: Option<DragKind>,

    status: String,
    project_path: String,
    /// fichier du projet courant (enregistré ou ouvert) ; None : projet sans titre
    project_file: Option<PathBuf>,
    /// contenu du dernier enregistrement (ou de l'ouverture) : il y a des modifications s'il diffère
    saved_json: Option<String>,
    /// projet ouvert dont les acteurs se chargent encore : on ne l'enregistre pas (il serait incomplet)
    opening: bool,
    /// projet à rouvrir au démarrage, dès que le jeu sera chargé (le dernier ouvert ou enregistré)
    restore_project: Option<PathBuf>,
    last_autosave: Instant,
    dialog: Option<Dialog>,
    export: ExportState,
    quality_open: bool,
    /// fenêtre « Caméra prédéfinie » ouverte
    shots_open: bool,
    /// image de départ du plan de la caméra prédéfinie
    shots_start: i32,
    /// aperçu du plan de la caméra prédéfinie (fenêtre ouverte), pas encore écrit
    shot_preview: Option<shots_ui::ShotPreview>,
    help: help_ui::HelpState,

    // animations du jeu
    left_tab: LeftTab,
    network: Option<Arc<forgelight::morpheme::Network>>,
    network_name: String,
    network_loading: bool,
    mrn_names: Vec<String>,
    clips: Vec<forgelight::morpheme::ClipInfo>,
    clip_filtered: Vec<usize>,
    clip_search: String,
    clip_category: String,
    clip_additive: bool,
    clip_dirty: bool,
    clip_selected: Option<ClipRef>,
    clip_speed: f32,
    clip_loops: u32,
    clip_root_motion: bool,
    clip_mode: BlendMode,
    clip_weight: f32,
    clip_fade_in: i32,
    clip_fade_out: i32,
    clip_part: BodyPart,
    dance_bpm: f32,
    troupe_size: usize,
    dance_secs: f32,
    /// danse choisie : un FBX importé (par son chemin), sinon la danse du cheval
    dance_fbx: Option<PathBuf>,
    /// répétitions de la danse FBX
    dance_loops: u32,
    clip_preview: bool,
    preview: Option<(ClipRef, Arc<DecodedClip>, Instant)>,
    /// animations FBX importées (Mixamo…), transférées sur le squelette à l'application
    fbx_clips: Vec<FbxClip>,
    fbx_path: String,
    /// fichiers .fbx trouvés dans Téléchargements et Documents/PlanetStudio (rafraîchis toutes les 3 s)
    fbx_found: (Vec<PathBuf>, Option<Instant>),

    // décor d'une carte
    decor: decor_ui::DecorState,
    /// particules d'ambiance (feuilles, sable, neige)
    particles: crate::particles::Particles,
    /// post-traitement du projet (étalonnage, profondeur de champ, netteté) et sa fenêtre
    post_fx: crate::post::PostFx,
    post_fx_open: bool,
}

/// Clip choisi dans l'onglet Animations : du jeu (indice dans `clips`) ou importé (indice dans `fbx_clips`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ClipRef {
    Game(usize),
    Fbx(usize),
}

struct FbxClip {
    name: String,
    path: PathBuf,
    anim: Arc<forgelight::fbx::FbxAnimation>,
}

/// Dossiers où chercher des .fbx à proposer.
fn fbx_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from);
    let mut v: Vec<PathBuf> = home.iter().map(|h| h.join("Downloads")).collect();
    v.push(projects_dir());
    v
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LeftTab {
    Models,
    Anims,
}

const FACTIONS: [&str; 4] = ["TR", "NC", "VS", "NSO"];

/// Zoom maximal de la timeline : nombre minimal d'images visibles.
const TL_MIN_SPAN: f32 = 4.0;

/// Ce qu'on attrape dans la barre de zoom de la timeline.
#[derive(Clone, Copy, PartialEq)]
enum TlBarDrag {
    Left,
    Right,
    Pan,
}

/// Les fichiers du jeu nomment les robots NSO « Common » (Armor_Common_Male_Skins_*,
/// Helmet_Common_Male_*) : on ajoute ces mots-clés pour la recherche.
fn search_text(name: &str) -> String {
    let l = name.to_ascii_lowercase();
    if (l.starts_with("armor_common_male") || l.starts_with("helmet_common_male")) && l.contains("max") {
        // le MAX NSO s'appelle Defector dans le jeu, « Common_MAX » dans ses fichiers
        format!("{l} nso robot defector")
    } else if l.starts_with("armor_common_male") || l.starts_with("helmet_common_male") || l.contains("_bot_") {
        format!("{l} nso robot")
    } else {
        l
    }
}
/// Nom affiché d'un sexe ou d'une classe de `SEXES` / `CLASSES` (écrits en français).
fn soldier_word(fr: &'static str) -> &'static str {
    if !crate::i18n::en() {
        return fr;
    }
    match fr {
        "Homme" => "Male",
        "Femme" => "Female",
        "Assaut lourd" => "Heavy Assault",
        "Assaut léger" => "Light Assault",
        "Médecin" => "Combat Medic",
        "Ingénieur" => "Engineer",
        "Infiltré" => "Infiltrator",
        w => w,
    }
}

const SEXES: [(&str, &str); 2] = [("Male", "Homme"), ("Female", "Femme")];
const CLASSES: [(&str, &str); 7] = [
    ("HeavyAssault", "Assaut lourd"),
    ("LightAssault", "Assaut léger"),
    ("CombatMedic", "Médecin"),
    ("Engineer", "Ingénieur"),
    ("Stealth", "Infiltré"),
    ("Max", "MAX"),
    ("", "Base"),
];

/// La classe d'indice `class` est-elle le MAX (exosquelette, hommes seulement, sans visage) ?
fn is_max(class: usize) -> bool {
    CLASSES[class].0 == "Max"
}
const HEADS: [&str; 4] = ["Caucasian", "African", "Asian", "Hispanic"];

/// Entrée d'une liste déroulante à filtre : la liste reste ouverte quand on clique dans le
/// champ de recherche (`CloseOnClickOutside`), donc on la ferme à la main quand on choisit.
fn pick_value<T: PartialEq>(ui: &mut egui::Ui, current: &mut T, value: T, text: impl Into<egui::WidgetText>) -> egui::Response {
    let r = ui.selectable_value(current, value, text);
    if r.clicked() {
        ui.memory_mut(|m| m.close_popup());
    }
    r
}

/// Liste déroulante d'un cosmétique du créateur de soldat (armure ou casque), avec filtre.
fn skin_combo(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash,
    choice: &mut SkinChoice,
    list: &[String],
    filter: &mut String,
    default_label: Option<&str>,
    none_label: Option<&str>,
    preview: &mut asset_preview::AssetPreview,
    preview_tex: Option<egui::TextureId>,
) {
    let text = choice.label(default_label.unwrap_or(""), none_label.or(default_label).unwrap_or(""));
    let resp = egui::ComboBox::from_id_salt(id).selected_text(text).width(220.0).height(400.0).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show_ui(ui, |ui| {
        if let Some(d) = default_label {
            pick_value(ui, choice, SkinChoice::Default, d);
        }
        if let Some(n) = none_label {
            pick_value(ui, choice, SkinChoice::None, n);
        }
        ui.separator();
        ui.add(egui::TextEdit::singleline(filter).hint_text(tr!("Filtrer (ex : auraxium, PS, look010)…", "Filter (e.g. auraxium, PS, look010)…")));
        let words: Vec<String> = filter.to_ascii_lowercase().split_whitespace().map(String::from).collect();
        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
            for n in list {
                let l = n.to_ascii_lowercase();
                if words.iter().all(|w| l.contains(w.as_str())) {
                    let r = pick_value(ui, choice, SkinChoice::Named(n.clone()), skins::display_name(n));
                    preview.attach(r, n, preview_tex);
                }
            }
        });
    });
    if let SkinChoice::Named(n) = choice {
        resp.response.on_hover_text(n.as_str());
    }
}

fn mat_rotation(m: &Mat4) -> Quat {
    let (_, r, _) = m.to_scale_rotation_translation();
    r.normalize()
}

fn dist_to_segment(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_squared().max(1e-6)).clamp(0.0, 1.0);
    p.distance(a + ab * t)
}

fn ray_aabb(o: Vec3, d: Vec3, lo: Vec3, hi: Vec3) -> Option<f32> {
    let inv = d.recip();
    let t1 = (lo - o) * inv;
    let t2 = (hi - o) * inv;
    let tmin = t1.min(t2).max_element();
    let tmax = t1.max(t2).min_element();
    (tmax >= tmin.max(0.0)).then_some(tmin.max(0.0))
}

impl StudioApp {
    pub fn new(cc: &eframe::CreationContext) -> Self {
        // agrandie une fois la fenêtre affichée, pour que Windows calcule la bonne taille
        cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
        let rs = cc.wgpu_render_state.clone().expect("le moteur wgpu est requis");
        let renderer = Renderer::new(&rs);
        let loader = Loader::new(cc.egui_ctx.clone());
        let config = Config::load();
        crate::i18n::set(config.lang.unwrap_or_else(crate::i18n::Lang::from_system));
        let game_path = config.game_path.clone().unwrap_or_default().trim().to_string();
        let game_ok = !game_path.is_empty() && is_game_dir(std::path::Path::new(&game_path));
        let game_error = (!game_path.is_empty() && !game_ok).then(|| {
            trf!("Le dossier enregistré ne contient plus les fichiers du jeu :\n{game_path}", "The saved folder no longer contains the game files:\n{game_path}")
        });
        let restore_project =
            config.last_project.as_deref().map(|p| PathBuf::from(p.trim())).filter(|p| p.is_file());

        let mut visuals = egui::Visuals::dark();
        visuals.selection.bg_fill = Color32::from_rgb(200, 120, 30);
        cc.egui_ctx.set_visuals(visuals);

        let mut app = Self {
            rs,
            renderer,
            loader,
            config,
            game: None,
            game_path: game_path.clone(),
            game_error,
            game_setup: false,
            updater: crate::update::Updater::new(),
            update_hidden: false,
            restart_after_exit: false,
            search: String::new(),
            category: Category::Characters,
            faction_filter: None,
            show_dme: false,
            adr_names: Vec::new(),
            dme_names: Vec::new(),
            filtered: Vec::new(),
            filter_dirty: true,
            soldier_faction: 0,
            soldier_sex: 0,
            soldier_class: 0,
            soldier_head: 0,
            soldier_armor: SkinChoice::Default,
            soldier_helmet: SkinChoice::Default,
            skin_filter: String::new(),
            skin_lists: None,
            insp_skin_lists: None,
            pending: HashMap::new(),
            next_request: 1,
            models: HashMap::new(),
            asset_preview: Default::default(),
            scene: Scene::default(),
            history: History::default(),
            editing: false,
            sel_actor: None,
            sel_bone: None,
            expanded_groups: HashSet::new(),
            bone_filter: String::new(),
            clipboard_pose: None,
            frame: 0.0,
            fps: 30.0,
            start: 0,
            end: 120,
            playing: false,
            looping: true,
            cam_rec_armed: false,
            cam_rec: None,
            interp: Interp::Smooth,
            last_tick: Instant::now(),
            idle_prev: false,
            force_render: true,
            skip_3d: false,
            last_render_size: None,
            key_drag: None,
            sun_drag: None,
            amb_drag: None,
            tl_view: None,
            tl_bar_drag: None,
            markers: Vec::new(),
            tl_mark_drag: None,
            tl_menu_marker: None,
            cam: OrbitCamera::default(),
            shot_view: false,
            show_monitor: true,
            show_skeleton: true,
            show_all_skeletons: false,
            show_grid: true,
            grid_yaw: 0.0,
            grid_origin: Vec3::ZERO,
            lighting: Lighting::default(),
            lighting_built: Ambiance::Studio,
            show_sun: true,
            camo_catalog: Vec::new(),
            camo_filter: String::new(),
            camo_failed: HashSet::new(),
            decal_catalog: Vec::new(),
            decal_filter: String::new(),
            gizmo: GizmoMode::Rotate,
            drag: None,
            last_mouse: None,
            fly_keys: HashSet::new(),
            fly_moving: false,
            shot_key_touch: None,
            fly_vel: Vec3::ZERO,
            look_anchor: None,
            raw_mouse: false,
            speed_toast: None,
            hover_axis: None,
            status: String::new(),
            project_path: projects_dir().join("scene.psproj").display().to_string(),
            project_file: None,
            saved_json: None,
            opening: false,
            restore_project,
            last_autosave: Instant::now(),
            dialog: None,
            export: ExportState::default(),
            quality_open: false,
            shots_open: false,
            shots_start: 0,
            shot_preview: None,
            help: Default::default(),
            left_tab: LeftTab::Models,
            network: None,
            network_name: "ThirdPerson.mrn".into(),
            network_loading: false,
            mrn_names: Vec::new(),
            clips: Vec::new(),
            clip_filtered: Vec::new(),
            clip_search: String::new(),
            clip_category: String::new(),
            clip_additive: false,
            clip_dirty: true,
            clip_selected: None,
            clip_speed: 1.0,
            clip_loops: 1,
            clip_root_motion: true,
            clip_mode: BlendMode::Replace,
            clip_weight: 1.0,
            clip_fade_in: 0,
            clip_fade_out: 0,
            clip_part: BodyPart::All,
            dance_bpm: 132.0,
            troupe_size: 7,
            dance_secs: 16.0,
            dance_fbx: None,
            dance_loops: 1,
            clip_preview: true,
            preview: None,
            fbx_clips: Vec::new(),
            fbx_path: String::new(),
            fbx_found: (Vec::new(), None),
            decor: Default::default(),
            particles: Default::default(),
            post_fx: Default::default(),
            post_fx_open: false,
        };
        if game_ok {
            app.open_game();
        }
        app.update_on_start(&cc.egui_ctx);
        app
    }

    fn open_game(&mut self) {
        let root = PathBuf::from(self.game_path.trim());
        if self.game_path.trim().is_empty() {
            self.game_error = Some(tr!("Choisis d'abord le dossier du jeu.", "Choose the game folder first.").into());
            return;
        }
        if !is_game_dir(&root) {
            self.game_error = Some(trf!(
                "Fichiers du jeu introuvables dans ce dossier.\nIl doit contenir Resources\\Assets\\data_x64_0.pack2 :\n{}",
                "Game files not found in this folder.\nIt must contain Resources\\Assets\\data_x64_0.pack2:\n{}",
                root.display()
            ));
            return;
        }
        self.game_error = None;
        self.status = tr!("Ouverture des archives du jeu...", "Opening the game archives...").into();
        self.loader.open_game(PathBuf::from(self.game_path.trim()));
    }

    // -------------------------------------------------------------- chargement

    fn request_model(&mut self, asset: &str, pending: PendingAdd) {
        let Some(game) = self.game.clone() else { return };
        let id = self.next_request;
        self.next_request += 1;
        self.pending.insert(id, pending);
        if let Some(m) = self.models.get(asset).cloned().or_else(|| self.preview_model(asset)) {
            self.finish_add(id, asset, m);
            return;
        }
        let skip: Vec<String> = Vec::new();
        self.status = trf!("Chargement de {asset}...", "Loading {asset}...");
        self.loader.load_model(game, asset.to_string(), id, skip);
    }

    fn finish_add(&mut self, request: u64, asset: &str, model: Arc<Model>) {
        let Some(p) = self.pending.remove(&request) else { return };
        if !self.renderer.has_model(asset) {
            self.renderer.upload_model(asset, &model);
        }
        self.models.insert(asset.to_string(), model.clone());
        if p.file.is_none() && !p.in_edit {
            // ajout par l'utilisateur : une étape d'annulation à lui (Ctrl+Z retire l'acteur)
            self.history.push(self.scene.snapshot());
        }
        let id = self.scene.add(asset, model);
        if let Some(f) = &p.file {
            let a = self.scene.actor_mut(id).unwrap();
            f.apply_to(a);
            self.scene.next_id = self.scene.next_id.max(f.id + 1);
            // ordre d'origine du projet (les chargements arrivent dans le désordre)
            self.scene.actors.sort_by_key(|a| a.id);
        } else if let Some(att) = p.attach {
            let look = match &att {
                PendingAttach::Merge(parent) if p.inherit_look => {
                    self.scene.actor(*parent).map(|o| (o.camo.clone(), o.decal.clone()))
                }
                _ => None,
            };
            let a = self.scene.actor_mut(id).unwrap();
            a.attachment = Some(match att {
                PendingAttach::Merge(parent) => Attachment::BoneMerge { parent },
                PendingAttach::Bone(parent, bone) => Attachment::Bone { parent, bone },
            });
            if let Some((camo, decal)) = look {
                a.camo = camo;
                a.decal = decal;
            }
        } else if let Some(place) = p.place {
            self.scene.actor_mut(id).unwrap().anim.root_static = place;
        } else {
            // place le nouvel acteur à côté des autres
            let n = self.scene.actors.iter().filter(|a| a.attachment.is_none()).count();
            if n > 1 {
                let a = self.scene.actor_mut(id).unwrap();
                a.anim.root_static.translation.x = (n as f32 - 1.0) * 1.2;
            }
        }
        let mut dance_err = None;
        if let Some(job) = p.dance {
            let fps = self.fps;
            match self.scene.actor_mut(id).map(|a| job.bake(a, fps)) {
                Some(Ok(last)) => self.end = self.end.max(last),
                Some(Err(e)) => dance_err = Some(e),
                None => {}
            }
        }
        let id = p.file.as_ref().map(|f| f.id).unwrap_or(id);
        if p.select {
            self.sel_actor = Some(id);
            self.sel_bone = None;
        }
        self.scene.evaluate(self.frame);
        self.status = match dance_err {
            Some(e) => trf!("{asset} ajouté à la scène, sans danse : {e}", "{asset} added to the scene, without dance: {e}"),
            None => trf!("{asset} ajouté à la scène", "{asset} added to the scene"),
        };
        for (child, kind) in p.children {
            let attach = match kind {
                ChildKind::Merge => PendingAttach::Merge(id),
                ChildKind::Bone(bone) => PendingAttach::Bone(id, bone),
            };
            self.request_model(&child, PendingAdd { attach: Some(attach), file: None, select: false, children: Vec::new(), ..Default::default() });
        }
    }

    /// Skins du soldat auquel appartient l'acteur sélectionné (corps, ou pièce fusionnée sur
    /// lui) : remplace sa pièce d'armure et son casque. Renvoie faux si ce n'est pas un soldat.
    fn skins_ui(&mut self, ui: &mut egui::Ui, aid: u64) -> bool {
        let root = self.scene.merge_root(aid);
        let Some(kind) = self.scene.actor(root).and_then(|r| skins::soldier_kind(&r.asset)) else { return false };
        if self.adr_names.is_empty() {
            return false;
        }
        if self.insp_skin_lists.as_ref().is_none_or(|(k, _, _)| *k != kind) {
            let (f, s, c) = kind;
            self.insp_skin_lists = Some((kind, skins::armors(&self.adr_names, f, s, c), skins::helmets(&self.adr_names, f, s, c)));
        }
        let Some((_, armors, helmets)) = self.insp_skin_lists.take() else { return false };
        // pièces fusionnées directement sur le corps
        let pieces: Vec<(u64, String)> = self
            .scene
            .actors
            .iter()
            .filter(|o| o.attachment == Some(Attachment::BoneMerge { parent: root }))
            .map(|o| (o.id, o.asset.clone()))
            .collect();
        let cur_armor = pieces.iter().find(|(_, n)| skins::is_armor_piece(n)).cloned();
        let cur_helmet = pieces.iter().find(|(_, n)| skins::is_helmet(n)).cloned();
        let as_choice = |c: &Option<(u64, String)>| c.as_ref().map_or(SkinChoice::None, |(_, n)| SkinChoice::Named(n.clone()));
        let (mut armor, mut helmet) = (as_choice(&cur_armor), as_choice(&cur_helmet));
        let (armor0, helmet0) = (armor.clone(), helmet.clone());

        ui.label(egui::RichText::new("Skins").strong());
        egui::Grid::new(("skins_insp", root)).num_columns(2).show(ui, |ui| {
            ui.label(tr!("Armure", "Armor"));
            let none = if kind.0 == "NSO" { tr!("Aucune (corps nu)", "None (bare body)") } else { tr!("Aucune (tenue de base)", "None (base outfit)") };
            let tex = self.renderer.preview_tex;
            skin_combo(ui, ("insp_armure", root), &mut armor, &armors, &mut self.skin_filter, None, Some(none), &mut self.asset_preview, tex);
            ui.end_row();
            ui.label(tr!("Casque", "Helmet"));
            skin_combo(ui, ("insp_casque", root), &mut helmet, &helmets, &mut self.skin_filter, None, Some(tr!("Aucun casque", "No helmet")), &mut self.asset_preview, tex);
            ui.end_row();
        });
        ui.horizontal(|ui| {
            if ui.button(tr!("🎲 Au hasard", "🎲 Random")).on_hover_text(tr!("Armure et casque tirés au hasard", "Random armor and helmet")).clicked() {
                if !armors.is_empty() {
                    armor = SkinChoice::Named(armors[skins::random_index(armors.len())].clone());
                }
                if !helmets.is_empty() {
                    helmet = SkinChoice::Named(helmets[skins::random_index(helmets.len() + 7) % helmets.len()].clone());
                }
            }
            ui.label(egui::RichText::new(trf!("{} armures, {} casques", "{} armors, {} helmets", armors.len(), helmets.len())).small().weak());
        });
        self.insp_skin_lists = Some((kind, armors, helmets));

        let mut remove = Vec::new();
        let mut add = Vec::new();
        for (new, old, cur) in [(&armor, &armor0, &cur_armor), (&helmet, &helmet0, &cur_helmet)] {
            if new != old {
                remove.extend(cur.as_ref().map(|(id, _)| *id));
                if let SkinChoice::Named(n) = new {
                    add.push(n.clone());
                }
            }
        }
        if !remove.is_empty() || !add.is_empty() {
            self.swap_pieces(root, &remove, &add);
        }
        true
    }

    /// Retire des pièces fusionnées sur `root` et en fusionne d'autres, en une étape d'annulation.
    /// Les nouvelles pièces reprennent le camouflage et la décalcomanie du corps.
    fn swap_pieces(&mut self, root: u64, remove: &[u64], add: &[String]) {
        self.history.push(self.scene.snapshot());
        for &id in remove {
            // ce qui était accroché à la pièce (rare) part avec elle
            for d in std::iter::once(id).chain(self.scene.descendants(id)).collect::<Vec<_>>() {
                self.scene.remove(d);
                self.renderer.forget_actor(d);
                self.expanded_groups.remove(&d);
            }
        }
        if self.sel_actor.is_some_and(|s| self.scene.actor(s).is_none()) {
            self.sel_actor = Some(root);
            self.sel_bone = None;
        }
        self.scene.evaluate(self.frame);
        for n in add {
            self.request_model(
                n,
                PendingAdd { attach: Some(PendingAttach::Merge(root)), in_edit: true, inherit_look: true, ..Default::default() },
            );
        }
        if add.is_empty() {
            self.status = tr!("Pièce retirée", "Piece removed").into();
        }
    }

    /// Réglages de camouflage de l'acteur sélectionné. Un changement s'applique aussi aux acteurs
    /// fusionnés sur lui (tête, casque…), ou à tous les personnages avec le bouton dédié.
    fn camo_ui(&mut self, ui: &mut egui::Ui, aid: u64) {
        let Some(a) = self.scene.actor(aid) else { return };
        let mut c = a.camo.clone();
        let family = self.merged_family(aid);
        let accepts = family
            .iter()
            .filter_map(|id| self.scene.actor(*id))
            .any(|o| o.model.meshes.iter().any(|m| m.accepts_camo()));
        let default_name = a.model.meshes.iter().find_map(|m| m.camo.clone());
        let mut apply_all = false;
        ui.label(egui::RichText::new("Camouflage").strong());
        if !accepts {
            ui.label(egui::RichText::new(tr!("Ce modèle n'a pas d'emplacement de camouflage.", "This model has no camo slot.")).small().weak());
        }
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt(("camo", aid)).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).selected_text(c.label()).width(240.0).show_ui(ui, |ui| {
                pick_value(ui, &mut c.choice, TextureChoice::None, tr!("Aucun (couleurs d'origine)", "None (original colors)"));
                let def_label = match &default_name {
                    Some(t) => trf!("Par défaut de l'armure ({})", "Armor default ({})", camo::display_name(t)),
                    None => tr!("Par défaut de l'armure", "Armor default").into(),
                };
                pick_value(ui, &mut c.choice, TextureChoice::Default, def_label);
                ui.separator();
                ui.add(egui::TextEdit::singleline(&mut self.camo_filter).hint_text(tr!("Filtrer les camouflages…", "Filter camos…")));
                let f = self.camo_filter.to_ascii_lowercase();
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    for t in self.camo_catalog.iter().filter(|t| f.is_empty() || t.to_ascii_lowercase().contains(&f)) {
                        pick_value(ui, &mut c.choice, TextureChoice::Texture(t.clone()), camo::display_name(t))
                            .on_hover_text(t);
                    }
                });
            });
        });
        ui.add_enabled_ui(c.choice != TextureChoice::None, |ui| {
            ui.horizontal(|ui| {
                ui.label(tr!("Échelle", "Scale"));
                ui.add(crate::calc::drag(&mut c.scale).range(0.5..=32.0).speed(0.05));
                ui.label(tr!("Intensité", "Intensity"));
                ui.add(egui::Slider::new(&mut c.strength, 0.0..=1.0));
            });
        });
        if ui.button(tr!("Appliquer à tous les personnages", "Apply to all characters")).clicked() {
            apply_all = true;
        }
        if c == a.camo && !apply_all {
            return;
        }
        let targets: Vec<u64> = if apply_all { self.scene.actors.iter().map(|o| o.id).collect() } else { family };
        for id in targets {
            if let Some(o) = self.scene.actor_mut(id) {
                o.camo = c.clone();
            }
        }
    }

    /// Décalcomanie de l'acteur sélectionné (même propagation que le camouflage).
    fn decal_ui(&mut self, ui: &mut egui::Ui, aid: u64) {
        let Some(a) = self.scene.actor(aid) else { return };
        let mut d = a.decal.clone();
        let family = self.merged_family(aid);
        let members: Vec<&crate::scene::Actor> = family.iter().filter_map(|id| self.scene.actor(*id)).collect();
        let accepts = members.iter().any(|o| o.model.meshes.iter().any(|m| m.accepts_decal()));
        let default_name = members
            .iter()
            .flat_map(|o| o.model.meshes.iter())
            .find_map(|m| m.decal.clone().filter(|t| !decal::is_blank(t)));
        let mut apply_all = false;
        ui.label(egui::RichText::new(tr!("Décalcomanie", "Decal")).strong());
        if !accepts {
            ui.label(egui::RichText::new(tr!("Ce modèle n'a pas d'emplacement de décalcomanie.", "This model has no decal slot.")).small().weak());
        }
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt(("decal", aid)).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).selected_text(d.label()).width(240.0).show_ui(ui, |ui| {
                pick_value(ui, &mut d.choice, TextureChoice::None, tr!("Aucune", "None"));
                let def_label = match &default_name {
                    Some(t) => trf!("Par défaut de l'armure ({})", "Armor default ({})", decal::display_name(t)),
                    None => tr!("Par défaut de l'armure (aucune)", "Armor default (none)").into(),
                };
                pick_value(ui, &mut d.choice, TextureChoice::Default, def_label);
                ui.separator();
                ui.add(egui::TextEdit::singleline(&mut self.decal_filter).hint_text(tr!("Filtrer les décalcomanies…", "Filter decals…")));
                let f = self.decal_filter.to_ascii_lowercase();
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    for t in self.decal_catalog.iter().filter(|t| f.is_empty() || t.to_ascii_lowercase().contains(&f)) {
                        pick_value(ui, &mut d.choice, TextureChoice::Texture(t.clone()), decal::display_name(t))
                            .on_hover_text(t);
                    }
                });
            });
        });
        ui.add_enabled_ui(d.choice != TextureChoice::None, |ui| {
            ui.horizontal(|ui| {
                ui.label(tr!("Opacité", "Opacity"));
                ui.add(egui::Slider::new(&mut d.opacity, 0.0..=1.0));
            });
        });
        if ui.button(tr!("Appliquer à tous les personnages", "Apply to all characters")).clicked() {
            apply_all = true;
        }
        if d == a.decal && !apply_all {
            return;
        }
        let targets: Vec<u64> = if apply_all { self.scene.actors.iter().map(|o| o.id).collect() } else { family };
        for id in targets {
            if let Some(o) = self.scene.actor_mut(id) {
                o.decal = d.clone();
            }
        }
    }

    /// L'acteur et ceux qui lui sont fusionnés (récursivement).
    fn merged_family(&self, root: u64) -> Vec<u64> {
        let mut ids = vec![root];
        let mut i = 0;
        while i < ids.len() {
            let p = ids[i];
            for o in &self.scene.actors {
                if o.attachment == Some(Attachment::BoneMerge { parent: p }) && !ids.contains(&o.id) {
                    ids.push(o.id);
                }
            }
            i += 1;
        }
        ids
    }

    /// Charge les textures de camouflage et de décalcomanie choisies et pas encore en mémoire.
    fn sync_camo_textures(&mut self) {
        let Some(game) = self.game.clone() else { return };
        let mut wanted: Vec<String> = Vec::new();
        for a in &self.scene.actors {
            for choice in [&a.camo.choice, &a.decal.choice] {
                if let TextureChoice::Texture(t) = choice {
                    if !self.renderer.has_texture(t) && !self.camo_failed.contains(t) && !wanted.contains(t) {
                        wanted.push(t.clone());
                    }
                }
            }
        }
        for t in wanted {
            match game.load_texture_gpu(&t) {
                Ok(img) => self.renderer.upload_texture(&t, &img),
                Err(e) => {
                    self.status = trf!("Texture {t} illisible : {e:#}", "Unreadable texture {t}: {e:#}");
                    self.camo_failed.insert(t);
                }
            }
        }
    }

    /// Soleil à une image : clés s'il est animé, sinon soleil fixe des réglages.
    fn sun_at(&self, frame: f32) -> SunState {
        self.scene.sun.sample(frame).unwrap_or_else(|| self.lighting.sun_state())
    }

    /// Ambiance (rotation, intensité) à une image : clés si elle est animée, sinon réglages fixes.
    fn amb_at(&self, frame: f32) -> AmbState {
        self.scene.ambiance.sample(frame).unwrap_or_else(|| self.lighting.amb_state())
    }

    /// Envoie au rendu le soleil et l'ambiance d'une image.
    fn apply_lighting_at(&mut self, frame: f32) {
        let l = self.lighting.with_sun(self.sun_at(frame));
        self.renderer.light_dir = l.light_dir();
        self.renderer.sun_color = l.sun_radiance();
        let a = self.amb_at(frame);
        self.renderer.env_rotation_deg = a.rotation;
        self.renderer.env_intensity = a.intensity;
        self.renderer.clear_color = a.background.map(|c| c as f64);
        // particules d'ambiance : elles suivent le temps de la timeline (lecture et export identiques)
        self.renderer.particle_time = frame / self.fps.max(1.0);
        // sillages dans l'eau du décor : relus dans la timeline, comme les particules
        let planes = self.renderer.water_planes();
        self.renderer.wakes = crate::wake::build(&self.scene, frame, self.fps, &planes);
    }

    /// Modifie l'ambiance : clé à l'image courante si elle est animée, sinon réglages fixes.
    fn set_amb(&mut self, st: AmbState) {
        if self.scene.ambiance.is_empty() {
            self.lighting.set_amb_state(st);
        } else {
            self.begin_edit();
            let frame = self.cur_frame();
            self.scene.ambiance.set_key(AmbKey::new(frame, st, self.interp));
        }
    }

    /// Pose une clé d'ambiance à l'image courante (l'ambiance devient animée).
    fn key_amb(&mut self) {
        self.begin_edit();
        let frame = self.cur_frame();
        let st = self.amb_at(self.frame);
        self.scene.ambiance.set_key(AmbKey::new(frame, st, self.interp));
        self.editing = false;
        self.status = trf!("Clé d'ambiance à l'image {frame}", "Ambiance key at frame {frame}");
    }

    /// Supprime une clé d'ambiance ; après la dernière, l'ambiance garde ses valeurs.
    fn delete_amb_key(&mut self, frame: i32) {
        self.begin_edit();
        if let Some(k) = self.scene.ambiance.remove_key(frame) {
            if self.scene.ambiance.is_empty() {
                self.lighting.set_amb_state(k.state());
            }
        }
        self.editing = false;
    }

    /// Modifie le soleil : clé à l'image courante s'il est animé, sinon soleil fixe.
    fn set_sun(&mut self, st: SunState) {
        if self.scene.sun.is_empty() {
            self.lighting.set_sun_state(st);
        } else {
            self.begin_edit();
            let frame = self.cur_frame();
            self.scene.sun.set_key(SunKey::new(frame, st, self.interp));
        }
    }

    /// Pose une clé de soleil à l'image courante (le soleil devient animé).
    fn key_sun(&mut self) {
        self.begin_edit();
        let frame = self.cur_frame();
        let st = self.sun_at(self.frame);
        self.scene.sun.set_key(SunKey::new(frame, st, self.interp));
        self.editing = false;
        self.status = trf!("Clé de soleil à l'image {frame}", "Sun key at frame {frame}");
    }

    /// Supprime une clé de soleil ; après la dernière, le soleil reste où elle le mettait.
    fn delete_sun_key(&mut self, frame: i32) {
        self.begin_edit();
        if let Some(k) = self.scene.sun.remove_key(frame) {
            if self.scene.sun.is_empty() {
                self.lighting.set_sun_state(k.state());
            }
        }
        self.editing = false;
    }

    /// Qualité des photos et vidéos.
    fn export_quality(&self) -> RenderQuality {
        if self.config.exports_like_view {
            self.config.quality
        } else {
            self.config.quality.for_export()
        }
    }

    /// Qualité en vigueur : celle des exports pendant un export, sinon celle de la vue.
    fn active_quality(&self) -> RenderQuality {
        if self.export.busy() {
            self.export_quality()
        } else {
            self.config.quality
        }
    }

    /// Fenêtre « Qualité graphique ».
    fn quality_ui(&mut self, ctx: &egui::Context) {
        let mut open = self.quality_open;
        let mut q = self.config.quality;
        let mut like_view = self.config.exports_like_view;
        let sample_counts = self.renderer.sample_counts().to_vec();
        egui::Window::new(tr!("Qualité graphique", "Graphics quality")).id(egui::Id::new("quality_window")).open(&mut open).resizable(false).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(tr!("Préréglage", "Preset"));
                for p in Preset::ALL {
                    if ui.selectable_label(q.matching_preset() == Some(p), p.label()).clicked() {
                        q = q.with_preset(p);
                    }
                }
                if q.matching_preset().is_none() {
                    ui.label(egui::RichText::new(tr!("(personnalisé)", "(custom)")).weak());
                }
            });
            ui.separator();
            // titre de section, sur toute la ligne de la grille
            let section = |ui: &mut egui::Ui, title: &str| {
                ui.label(egui::RichText::new(title).strong());
                ui.end_row();
            };
            egui::Grid::new("quality").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                section(ui, tr!("Affichage", "View"));
                ui.label(tr!("Résolution de la vue", "Viewport resolution"));
                let mut pct = (q.render_scale * 100.0).round();
                if ui
                    .add(egui::Slider::new(&mut pct, 50.0..=200.0).suffix(" %"))
                    .on_hover_text(tr!("Au-delà de 100 %, l'image est calculée plus grande puis réduite : contours plus fins, rendu plus lent", "Above 100%, the image is rendered larger then scaled down: finer edges, slower rendering"))
                    .changed()
                {
                    q.render_scale = pct / 100.0;
                }
                ui.end_row();
                ui.label(tr!("Luminosité", "Brightness"));
                ui.add(egui::Slider::new(&mut q.brightness, 0.5..=2.0));
                ui.end_row();

                section(ui, tr!("Anticrénelage", "Anti-aliasing"));
                ui.label("MSAA");
                let msaa_label = |n: u32| if n > 1 { format!("{n}× (MSAA)") } else { tr!("Désactivé", "Off").to_owned() };
                // valeur effective : la carte peut refuser 2× ou 8×
                let effective = sample_counts.iter().copied().filter(|&n| n <= q.msaa.max(1)).max().unwrap_or(1);
                egui::ComboBox::from_id_salt("msaa").selected_text(msaa_label(effective)).show_ui(ui, |ui| {
                    for &n in &sample_counts {
                        ui.selectable_value(&mut q.msaa, n, msaa_label(n));
                    }
                })
                .response
                .on_hover_text(tr!("Lisse les contours des modèles et, avec la flore haute, les bords des feuillages", "Smooths model edges and, with high flora, foliage edges"));
                ui.end_row();
                ui.label("");
                ui.vertical(|ui| {
                    ui.checkbox(&mut q.fxaa, tr!("FXAA (lissage de l'image finale)", "FXAA (final image smoothing)"))
                        .on_hover_text(tr!("Rattrape les contours que le MSAA ne voit pas ; seul, il remplace un MSAA trop coûteux (image un peu plus douce)", "Catches the edges MSAA misses; on its own, it replaces a too costly MSAA (slightly softer image)"));
                    ui.checkbox(&mut q.specular_aa, tr!("Reflets lissés", "Smoothed highlights"))
                        .on_hover_text(tr!("Évite le scintillement des reflets du soleil sur le relief fin, les courbures et l'eau au loin", "Avoids flickering sun highlights on fine relief, curves and distant water"));
                });
                ui.end_row();

                section(ui, tr!("Rendu", "Rendering"));
                ui.label("Textures");
                egui::ComboBox::from_id_salt("texq").selected_text(q.textures.label()).show_ui(ui, |ui| {
                    for v in TextureQuality::ALL {
                        ui.selectable_value(&mut q.textures, v, v.label());
                    }
                });
                ui.end_row();
                ui.label("");
                ui.vertical(|ui| {
                    ui.add_enabled(q.textures == TextureQuality::Ultra, egui::Checkbox::new(&mut q.texture_hq, tr!("Filtrage bicubique des gros plans", "Bicubic filtering for close-ups")))
                        .on_hover_text(tr!("Là où un texel couvre plusieurs pixels : plus de losanges ni de marches sur la couleur et le relief (textures « Ultra »)", "Where a texel covers several pixels: no more diamonds or steps on color and relief (“Ultra” textures)"));
                    ui.horizontal(|ui| {
                        ui.label(tr!("Netteté des textures", "Texture sharpness"));
                        ui.add(egui::Slider::new(&mut q.texture_sharpness, 0.0..=1.0))
                            .on_hover_text(tr!("Lit des niveaux de texture plus fins dans les images suréchantillonnées (exports, vue au-delà de 100 %)", "Reads finer texture levels in supersampled images (exports, viewport above 100%)"));
                    });
                });
                ui.end_row();
                ui.label(tr!("Ombres", "Shadows"));
                egui::ComboBox::from_id_salt("shadowq").selected_text(q.shadows.label()).show_ui(ui, |ui| {
                    for v in ShadowQuality::ALL {
                        ui.selectable_value(&mut q.shadows, v, v.label());
                    }
                });
                ui.end_row();
                ui.label(tr!("Éclairage", "Lighting"));
                ui.vertical(|ui| {
                    ui.checkbox(&mut q.normal_maps, tr!("Relief des textures (normal maps)", "Texture relief (normal maps)"));
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut q.emissive, tr!("Lumières des textures (émissif)", "Texture lights (emissive)"));
                        ui.add_enabled(q.emissive, egui::Slider::new(&mut q.emissive_strength, 0.0..=3.0))
                            .on_hover_text(tr!("Intensité des néons, LED et voyants (1 = défaut). Au-delà, leur halo de bloom s'élargit", "Intensity of neons, LEDs and indicator lights (1 = default). Above that, their bloom halo widens"));
                    });
                });
                ui.end_row();

                section(ui, tr!("Effets", "Effects"));
                ui.label("Bloom");
                ui.add(egui::Slider::new(&mut q.bloom, 0.0..=3.0))
                    .on_hover_text(tr!("Halo autour des zones très lumineuses (LED, emblèmes, reflets vifs). 0 = désactivé", "Halo around very bright areas (LEDs, emblems, sharp highlights). 0 = off"));
                ui.end_row();
                ui.label(tr!("Occlusion ambiante", "Ambient occlusion"));
                ui.horizontal(|ui| {
                    ui.add(egui::Slider::new(&mut q.ao, 0.0..=1.0))
                        .on_hover_text(tr!("Assombrit les creux : aisselles, plis, dessous des plaques. 0 = désactivée", "Darkens crevices: armpits, folds, under plates. 0 = off"));
                    ui.add_enabled(
                        q.ao > 0.0,
                        crate::calc::drag(&mut q.ao_radius).range(0.05..=1.0).speed(0.01).prefix(tr!("rayon ", "radius ")).suffix(" m"),
                    );
                });
                ui.end_row();
                ui.label(tr!("Particules", "Particles"));
                egui::ComboBox::from_id_salt("partq").selected_text(q.particles.label()).show_ui(ui, |ui| {
                    for v in ParticleQuality::ALL {
                        ui.selectable_value(&mut q.particles, v, v.label());
                    }
                })
                .response
                .on_hover_text(tr!("Nombre de particules d'ambiance (neige, sable, feuilles), en plus de la densité du décor", "Number of ambient particles (snow, sand, leaves), on top of the scenery's density"));
                ui.end_row();

                section(ui, tr!("Décor", "Scenery"));
                ui.label("Terrain");
                egui::ComboBox::from_id_salt("terrainq").selected_text(q.terrain.label()).show_ui(ui, |ui| {
                    for v in TerrainQuality::ALL {
                        ui.selectable_value(&mut q.terrain, v, v.label());
                    }
                })
                .response
                .on_hover_text(tr!("Distance jusqu'où le détail du sol et de la roche est visible", "Distance up to which ground and rock detail is visible"));
                ui.end_row();
                ui.label(tr!("Flore", "Flora"));
                egui::ComboBox::from_id_salt("floraq")
                    .selected_text(if q.flora_hq { tr!("Haute", "High") } else { tr!("Basse", "Low") })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut q.flora_hq, false, tr!("Basse (découpe nette)", "Low (hard cutout)"));
                        ui.selectable_value(&mut q.flora_hq, true, tr!("Haute (bords lissés, feuillage garni au loin)", "High (smoothed edges, fuller foliage in the distance)"));
                    })
                    .response
                    .on_hover_text(tr!("Les bords lissés demandent le MSAA", "Smoothed edges need MSAA"));
                ui.end_row();
                ui.label(tr!("Eau", "Water"));
                egui::ComboBox::from_id_salt("waterq")
                    .selected_text(if q.water_hq { tr!("Haute", "High") } else { tr!("Basse", "Low") })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut q.water_hq, false, tr!("Basse (rides fixes)", "Low (static ripples)"));
                        ui.selectable_value(&mut q.water_hq, true, tr!("Haute (houle animée, reflet du ciel)", "High (animated swell, sky reflection)"));
                    });
                ui.end_row();
            });
            ui.separator();
            ui.label(egui::RichText::new(tr!("Photos et vidéos", "Photos and videos")).strong());
            let mut final_exports = !like_view;
            ui.checkbox(&mut final_exports, tr!("Exporter en « Rendu final » (sinon : qualité de la vue)", "Export as “Final render” (otherwise: viewport quality)"));
            like_view = !final_exports;
            ui.horizontal(|ui| {
                ui.label(tr!("Suréchantillonnage", "Supersampling"));
                for n in 1..=4u32 {
                    ui.selectable_value(&mut q.supersample, n, format!("{n}×"));
                }
            })
            .response
            .on_hover_text(tr!("Rendu N fois plus grand puis réduit : contours et détails plus fins, export plus lent. Les grandes images sont rendues par tuiles : 4K en 3× ou 4×, 8K et plus sont possibles", "Rendered N times larger then scaled down: finer edges and details, slower export. Large images are rendered in tiles: 4K at 3× or 4×, 8K and more are possible"));
            ui.label(
                egui::RichText::new(tr!(
                    "Les ombres du soleil se règlent aussi par projet (Affichage > Soleil) ; « Désactivées » ici les coupe partout.",
                    "Sun shadows can also be set per project (View > Sun); “Off” here turns them off everywhere."
                ))
                .small()
                .weak(),
            );
        });
        self.quality_open = open;
        if q != self.config.quality || like_view != self.config.exports_like_view {
            self.config.quality = q;
            self.config.exports_like_view = like_view;
            self.config.save();
        }
    }

    /// Fenêtre « Post-traitement » : réglages du projet, appliqués à la vue, aux photos et aux vidéos.
    fn post_fx_ui(&mut self, ctx: &egui::Context) {
        // réglages : voir post_fx_controls
        let mut open = self.post_fx_open;
        let mut fx = self.post_fx;
        egui::Window::new(tr!("Post-traitement", "Post-processing")).id(egui::Id::new("post_window")).open(&mut open).resizable(false).show(ctx, |ui| {
            post_fx_controls(ui, &mut fx);
            ui.label(
                egui::RichText::new(tr!("Enregistré dans le projet ; la vue, les photos et les vidéos l'utilisent.", "Saved in the project; the viewport, photos and videos use it."))
                    .small()
                    .weak(),
            );
        });
        self.post_fx_open = open;
        if fx != self.post_fx {
            self.post_fx = fx;
            self.renderer.post_settings.fx = fx;
        }
    }

    /// Recharge le panorama si l'ambiance a changé (menu, projet ouvert, jeu chargé après coup).
    fn sync_lighting(&mut self) {
        if self.lighting.ambiance != self.lighting_built {
            if let Some(game) = self.game.clone() {
                match crate::environment::build(&game, self.lighting.ambiance) {
                    Ok(env) => self.renderer.set_environment(env.as_ref()),
                    Err(e) => {
                        self.status = trf!("Ambiance {} indisponible : {e:#}", "Ambiance {} unavailable: {e:#}", self.lighting.ambiance.label());
                        self.renderer.set_environment(None);
                        self.lighting.ambiance = Ambiance::Studio;
                    }
                }
                self.lighting_built = self.lighting.ambiance;
            }
        }
        self.renderer.ground_shadow = self.lighting.ground_shadow;
        self.renderer.decor_shades_actors = self.lighting.decor_shades_actors;
        self.sync_particles();
        self.apply_lighting_at(self.frame);
        // qualité : celle de la vue, ou « Rendu final » pendant un export
        let q = self.active_quality();
        q.apply(&mut self.renderer, self.lighting.shadows);
        self.renderer.post_settings.fx = self.post_fx;
    }

    fn poll_loader(&mut self) {
        while let Ok(msg) = self.loader.rx.try_recv() {
            self.force_render = true;
            match msg {
                Msg::Game(Ok(gd)) => {
                    self.adr_names = gd.index.names_with_ext("adr");
                    self.dme_names = gd.index.names_with_ext("dme");
                    self.mrn_names = gd.index.names_with_ext("mrn");
                    self.camo_catalog = camo::catalog(&gd);
                    self.decal_catalog = decal::catalog(&gd);
                    self.filter_dirty = true;
                    self.status = trf!(
                        "Jeu chargé : {} fichiers, {} acteurs (.adr), {} modèles (.dme)",
                        "Game loaded: {} files, {} actors (.adr), {} models (.dme)",
                        gd.index.len(),
                        self.adr_names.len(),
                        self.dme_names.len()
                    );
                    self.config.game_path = Some(self.game_path.trim().to_string());
                    self.config.save();
                    self.game = Some(gd);
                    self.game_setup = false;
                    // démarrage : rouvre le dernier projet, si l'on n'a encore rien commencé
                    if let Some(p) = self.restore_project.take() {
                        if self.project_file.is_none() && self.scene.actors.is_empty() && self.pending.is_empty() {
                            self.project_path = p.display().to_string();
                            self.open_project();
                        }
                    }
                }
                Msg::Game(Err(e)) => {
                    self.game_error = Some(e.clone());
                    self.status = trf!("Erreur : {e}", "Error: {e}");
                }
                Msg::Network { name, result } => {
                    self.network_loading = false;
                    match result {
                        Ok(net) => {
                            self.clips = net.main_clips().into_iter().cloned().collect();
                            self.clips.sort_by(|a, b| a.name.cmp(&b.name));
                            self.status = trf!("{name} : {} animations", "{name}: {} animations", self.clips.len());
                            self.network = Some(net);
                            self.clip_dirty = true;
                            if matches!(self.clip_selected, Some(ClipRef::Game(_))) {
                                self.clip_selected = None;
                            }
                        }
                        Err(e) => self.status = trf!("Impossible de charger {name} : {e}", "Could not load {name}: {e}"),
                    }
                }
                Msg::Decor { request, result } => self.on_decor_loaded(request, result),
                Msg::Model { request, result, .. } if self.asset_preview.owns(request) => self.take_preview_model(request, result),
                Msg::Model { request, asset, result } => match result {
                    Ok((model, textures)) => {
                        for (name, img) in &textures {
                            if !self.renderer.has_texture(name) {
                                self.renderer.upload_texture(name, img);
                            }
                        }
                        for w in &model.warnings {
                            eprintln!("{asset} : {w}");
                        }
                        self.finish_add(request, &asset, model);
                    }
                    Err(e) => {
                        self.pending.remove(&request);
                        self.status = trf!("Impossible de charger {asset} : {e}", "Could not load {asset}: {e}");
                    }
                },
            }
        }
    }

    fn refilter(&mut self) {
        let q: Vec<String> = self.search.to_ascii_lowercase().split_whitespace().map(String::from).collect();
        let src = if self.show_dme { &self.dme_names } else { &self.adr_names };
        self.filtered = src
            .iter()
            // une recherche explicite porte sur toutes les catégories
            .filter(|n| !q.is_empty() || self.category.matches(n))
            .filter(|n| self.category != Category::Gameplay || faction_matches(self.faction_filter, n))
            .filter(|n| {
                let l = search_text(n);
                q.iter().all(|w| l.contains(w.as_str()))
            })
            .cloned()
            .collect();
        self.filter_dirty = false;
    }


    fn find_name(&self, candidates: &[String]) -> Option<String> {
        let game = self.game.as_ref()?;
        candidates.iter().find(|c| game.index.contains(c)).cloned()
    }

    /// Pièces d'un soldat : (acteur principal, pièces fusionnées). `armor` et `helmet` choisissent
    /// les cosmétiques (voir [`skins`]).
    fn soldier_parts(
        &self,
        faction: usize,
        sex: usize,
        class: usize,
        head: usize,
        armor: &SkinChoice,
        helmet: &SkinChoice,
    ) -> Result<(String, Vec<(String, ChildKind)>), String> {
        let named = |c: &SkinChoice| match c {
            SkinChoice::Named(n) => self.find_name(std::slice::from_ref(n)),
            _ => None,
        };
        let f = FACTIONS[faction];
        let (c, _) = CLASSES[class];
        if c == "Max" {
            // exosquelette : corps Look001 (robot NSO : MAX_Base), amélioration ou skin par-dessus,
            // casque, et une arme anti-infanterie à chaque bras (pas d'arme pour le Defector NSO)
            let body = skins::max_body(f);
            if self.find_name(std::slice::from_ref(&body)).is_none() {
                return Err(trf!("Corps de MAX introuvable : {body}", "MAX body not found: {body}"));
            }
            let armor = match armor {
                SkinChoice::Default if f == "NSO" => self.find_name(&["Armor_Common_Male_Skins_MAX_001.adr".to_string()]),
                c => named(c),
            };
            let helmet = match helmet {
                SkinChoice::Default if f == "NSO" => self.find_name(&["Helmet_Common_Male_All_MAX_001.adr".to_string()]),
                SkinChoice::Default => self.find_name(&[format!("Helmet_{f}_Male_Max_Look001.adr")]),
                c => named(c),
            };
            let mut children: Vec<_> = armor.into_iter().chain(helmet).map(|n| (n, ChildKind::Merge)).collect();
            for (w, bone) in skins::max_weapons(f).into_iter().zip(["L_WEAPON", "R_WEAPON"]) {
                if let Some(w) = self.find_name(&[w]) {
                    children.push((w, ChildKind::Bone(bone.into())));
                }
            }
            return Ok((body, children));
        }
        if f == "NSO" {
            // robot NSO : corps + armure de classe + tête-caméra
            let body = "Armor_Common_Male_Skins_Base.adr".to_string();
            if self.find_name(std::slice::from_ref(&body)).is_none() {
                return Err(trf!("Corps NSO introuvable : {body}", "NSO body not found: {body}"));
            }
            let class_armor = match c {
                "HeavyAssault" => Some("HeavyAssault"),
                "LightAssault" => Some("LightAssault"),
                "CombatMedic" => Some("Medic"),
                "Engineer" => Some("Engineer"),
                "Stealth" => Some("Infiltrator"),
                _ => None,
            };
            let mut children = Vec::new();
            let armor = match armor {
                SkinChoice::Default => class_armor.and_then(|k| self.find_name(&[format!("Armor_Common_Male_Skins_{k}_001.adr")])),
                c => named(c),
            };
            let helmet = match helmet {
                SkinChoice::Default => self.find_name(&["Helmet_Common_Male_All_Look001.adr".to_string()]),
                c => named(c),
            };
            children.extend(armor.into_iter().chain(helmet).map(|n| (n, ChildKind::Merge)));
            return Ok((body, children));
        }
        let (s, _) = SEXES[sex];
        let armor_c = match c {
            "" => vec![format!("Armor_{f}_{s}_Base.adr")],
            "Stealth" => vec![format!("Armor_{f}_{s}_StealthBase.adr"), format!("Armor_{f}_{s}_Infiltrator_Base.adr")],
            _ => vec![format!("Armor_{f}_{s}_{c}_Base.adr")],
        };
        let armor_main = self.find_name(&armor_c).ok_or_else(|| trf!("Armure introuvable : {}", "Armor not found: {}", armor_c.join(" / ")))?;
        let mut children = Vec::new();
        if let Some(h) = self.find_name(&[format!("Head_{s}_{}.adr", HEADS[head])]) {
            children.push((h, ChildKind::Merge));
        }
        // pièce d'armure par-dessus la combinaison de base (rien par défaut)
        if let Some(a) = named(armor) {
            children.push((a, ChildKind::Merge));
        }
        match helmet {
            SkinChoice::Default => {}
            SkinChoice::None => return Ok((armor_main, children)),
            c => {
                if let Some(h) = named(c) {
                    children.push((h, ChildKind::Merge));
                }
                return Ok((armor_main, children));
            }
        }
        // Les casques infiltrés s'appellent « Stealth » dans le jeu (Helmet_TR_Female_Stealth_Look001),
        // comme l'armure ; « Infiltrator » reste en secours.
        let helmet_classes: Vec<&str> = match c {
            "Stealth" => vec!["Stealth", "Infiltrator"],
            "" => vec!["HeavyAssault"],
            c => vec![c],
        };
        // d'abord le casque de base exact (« ClassLook001 » contient aussi « look001 »)
        let exact: Vec<String> = helmet_classes.iter().map(|k| format!("Helmet_{f}_{s}_{k}_Look001.adr")).collect();
        let helmet = self.find_name(&exact).or_else(|| {
            helmet_classes.iter().find_map(|k| {
                let prefix = format!("helmet_{f}_{s}_{k}_").to_ascii_lowercase();
                self.adr_names
                    .iter()
                    .find(|n| {
                        let l = n.to_ascii_lowercase();
                        l.starts_with(&prefix) && l.contains("_look001")
                    })
                    .or_else(|| self.adr_names.iter().find(|n| n.to_ascii_lowercase().starts_with(&prefix)))
                    .cloned()
            })
        });
        if let Some(h) = helmet {
            children.push((h, ChildKind::Merge));
        }
        Ok((armor_main, children))
    }

    /// Recalcule les armures et casques proposés quand la faction, le sexe ou la classe change, et
    /// oublie un choix qui n'y figure plus.
    fn refresh_skin_lists(&mut self) {
        let key = (self.soldier_faction, self.soldier_sex, self.soldier_class);
        if self.skin_lists.as_ref().is_some_and(|(k, _, _)| *k == key) {
            return;
        }
        // le MAX n'existe qu'en homme
        let s = if is_max(key.2) { "Male" } else { SEXES[key.1].0 };
        let (f, c) = (FACTIONS[key.0], CLASSES[key.2].0);
        let armors = skins::armors(&self.adr_names, f, s, c);
        let helmets = skins::helmets(&self.adr_names, f, s, c);
        for (choice, list) in [(&mut self.soldier_armor, &armors), (&mut self.soldier_helmet, &helmets)] {
            if matches!(choice, SkinChoice::Named(n) if !list.contains(n)) {
                *choice = SkinChoice::Default;
            }
        }
        // les listes restent vides tant que l'index du jeu n'est pas chargé : on recalculera
        if !self.adr_names.is_empty() {
            self.skin_lists = Some((key, armors, helmets));
        }
    }

    /// Crée un soldat complet d'après les choix du panneau.
    fn add_soldier(&mut self) {
        match self.soldier_parts(
            self.soldier_faction,
            self.soldier_sex,
            self.soldier_class,
            self.soldier_head,
            &self.soldier_armor,
            &self.soldier_helmet,
        ) {
            Ok((main, children)) => {
                self.request_model(&main, PendingAdd { select: true, children, ..Default::default() })
            }
            Err(e) => self.status = e,
        }
    }

    /// Troupe de danseurs : factions et classes variées, en formation en V.
    fn create_troupe(&mut self, n: usize, job: DanceJob) {
        let mut created = 0;
        for i in 0..n {
            let faction = i % FACTIONS.len();
            let class = i % 5;
            let sex = (i / FACTIONS.len()) % 2;
            let head = i % HEADS.len();
            let Ok((main, children)) =
                self.soldier_parts(faction, sex, class, head, &SkinChoice::Default, &SkinChoice::Default)
            else {
                continue;
            };
            // 0 devant au centre, puis alternativement à gauche/droite, en retrait
            let rank = i.div_ceil(2) as f32;
            let side = if i % 2 == 1 { 1.0 } else { -1.0 };
            let place = Transform { translation: Vec3::new(side * rank * 1.25, 0.0, -rank * 1.1), ..Transform::IDENTITY };
            self.request_model(&main, PendingAdd { select: i == 0, children, place: Some(place), dance: Some(job.clone()), ..Default::default() });
            created += 1;
        }
        self.status = trf!("Troupe de {created} danseurs en cours de chargement ({})...", "Loading a troupe of {created} dancers ({})...", job.label());
    }

    /// Danse choisie dans l'onglet Animations, à partir de l'image courante.
    fn dance_job(&mut self) -> DanceJob {
        self.frame = self.frame.round();
        let start = self.cur_frame();
        match self.dance_fbx.as_ref().and_then(|p| self.fbx_clips.iter().find(|c| &c.path == p)) {
            Some(c) => DanceJob::Fbx { start, name: c.name.clone(), anim: c.anim.clone(), loops: self.dance_loops },
            None => DanceJob::Horse { start, bpm: self.dance_bpm, secs: self.dance_secs },
        }
    }

    /// Fait danser les acteurs donnés (la sélection, ou tous les personnages non attachés).
    fn dance_actors(&mut self, ids: Vec<u64>) {
        self.playing = false;
        self.preview = None;
        let job = self.dance_job();
        let (start, fps) = (self.cur_frame(), self.fps);
        self.begin_edit();
        let mut n = 0;
        let mut last = start;
        let mut errors = Vec::new();
        for id in ids {
            match self.scene.actor_mut(id).map(|a| job.bake(a, fps)) {
                Some(Ok(l)) => {
                    last = last.max(l);
                    n += 1;
                }
                Some(Err(e)) => errors.push(e),
                None => {}
            }
        }
        self.editing = false;
        self.end = self.end.max(last);
        self.scene.evaluate(self.frame);
        self.status = match (n, errors.first()) {
            (0, Some(e)) => e.clone(),
            (0, None) => tr!("Aucun personnage à faire danser", "No character to make dance").into(),
            _ => trf!(
                "{n} personnage(s) dansent {}, images {start} à {last}{}",
                "{n} character(s) dance {}, frames {start} to {last}{}",
                job.label(),
                if errors.is_empty() {
                    String::new()
                } else {
                    trf!(" ({} ignoré(s) : {})", " ({} skipped: {})", errors.len(), errors[0])
                }
            ),
        };
    }

    // -------------------------------------------------------------- édition

    /// Mémorise l'état pour "annuler" au début d'une modification.
    fn begin_edit(&mut self) {
        if !self.editing {
            self.history.push(self.scene.snapshot());
            self.editing = true;
            self.shot_key_touch = None;
        }
    }

    fn cur_frame(&self) -> i32 {
        self.frame.round() as i32
    }

    fn set_frame(&mut self, f: f32) {
        self.frame = f;
        self.scene.evaluate(self.frame);
    }

    /// Matrice monde du parent de la sélection.
    fn parent_world(a: &crate::scene::Actor, bone: Option<usize>) -> Mat4 {
        match bone {
            Some(b) => match a.model.skeleton.bones[b].parent {
                Some(p) => a.world[p],
                None => a.root_world,
            },
            None => a.root_world * a.root_pose.to_mat4().inverse(),
        }
    }

    /// Rotation monde `rw` appliquée à la sélection (os ou racine).
    fn apply_world_rotation(&mut self, rw: Quat) {
        let frame = self.cur_frame();
        let interp = self.interp;
        let sel_bone = self.sel_bone;
        let Some(a) = self.sel_actor.and_then(|id| self.scene.actor_mut(id)) else { return };
        let pr = mat_rotation(&Self::parent_world(a, sel_bone));
        match sel_bone {
            Some(b) => {
                let mut t = a.pose[b];
                t.rotation = (pr.inverse() * rw * pr * t.rotation).normalize();
                a.key_bone(b, frame, t, interp);
            }
            None => {
                let mut t = a.root_pose;
                t.rotation = (pr.inverse() * rw * pr * t.rotation).normalize();
                a.key_root(frame, t, interp, false);
            }
        }
        self.scene.update_world();
    }

    fn apply_world_translation(&mut self, d: Vec3) {
        let frame = self.cur_frame();
        let interp = self.interp;
        let sel_bone = self.sel_bone;
        let Some(a) = self.sel_actor.and_then(|id| self.scene.actor_mut(id)) else { return };
        let local = Self::parent_world(a, sel_bone).inverse().transform_vector3(d);
        match sel_bone {
            Some(b) => {
                let mut t = a.pose[b];
                t.translation += local;
                a.key_bone(b, frame, t, interp);
            }
            None => {
                let mut t = a.root_pose;
                t.translation += local;
                a.key_root(frame, t, interp, false);
            }
        }
        self.scene.update_world();
    }

    /// Pivot et orientation de la sélection.
    fn selection_frame(&self) -> Option<(Vec3, Quat)> {
        let a = self.scene.actor(self.sel_actor?)?;
        let m = match self.sel_bone {
            Some(b) => *a.world.get(b)?,
            None => a.root_world,
        };
        Some((m.w_axis.truncate(), mat_rotation(&m)))
    }

    fn key_selection(&mut self) {
        let frame = self.cur_frame();
        let interp = self.interp;
        let sel_bone = self.sel_bone;
        if self.sel_actor.is_none() {
            return;
        }
        self.begin_edit();
        if let Some(a) = self.sel_actor.and_then(|id| self.scene.actor_mut(id)) {
            match sel_bone {
                Some(b) => {
                    let t = a.pose[b];
                    a.key_bone(b, frame, t, interp);
                }
                None => {
                    let t = a.root_pose;
                    a.key_root(frame, t, interp, true);
                }
            }
        }
        self.editing = false;
        self.status = trf!("Clé posée à l'image {frame}", "Key set at frame {frame}");
    }

    fn key_all_bones(&mut self) {
        let frame = self.cur_frame();
        let interp = self.interp;
        if self.sel_actor.is_none() {
            return;
        }
        self.begin_edit();
        if let Some(a) = self.sel_actor.and_then(|id| self.scene.actor_mut(id)) {
            for b in 0..a.pose.len() {
                let t = a.pose[b];
                a.key_bone(b, frame, t, interp);
            }
            let t = a.root_pose;
            a.key_root(frame, t, interp, true);
        }
        self.editing = false;
        self.status = trf!("Pose complète enregistrée à l'image {frame}", "Full pose saved at frame {frame}");
    }

    fn delete_key(&mut self) {
        let frame = self.cur_frame();
        let sel_bone = self.sel_bone;
        if self.sel_actor.is_none() {
            return;
        }
        self.begin_edit();
        if let Some(a) = self.sel_actor.and_then(|id| self.scene.actor_mut(id)) {
            match sel_bone {
                Some(b) => {
                    a.anim.bones[b].remove_key(frame);
                }
                None => {
                    // sans os sélectionné : toutes les clés de l'acteur à cette image
                    a.anim.root.remove_key(frame);
                    for t in &mut a.anim.bones {
                        t.remove_key(frame);
                    }
                }
            }
        }
        self.editing = false;
        self.scene.evaluate(self.frame);
    }

    /// Efface toute l'animation de l'acteur sélectionné (racine et tous les os).
    /// L'acteur reste à sa position de l'image courante ; annulable.
    fn clear_actor_keys(&mut self) {
        let Some(id) = self.sel_actor else { return };
        self.begin_edit();
        let mut n = 0;
        if let Some(a) = self.scene.actor_mut(id) {
            n = a.anim.root.keys.len() + a.anim.bones.iter().map(|t| t.keys.len()).sum::<usize>();
            if !a.anim.root.is_empty() {
                // garder l'acteur là où il est à l'image courante
                a.anim.root_static = a.root_pose;
            }
            a.anim.root.keys.clear();
            for t in &mut a.anim.bones {
                t.keys.clear();
            }
        }
        self.editing = false;
        self.scene.evaluate(self.frame);
        self.status = trf!("{n} clés effacées (Ctrl+Z pour annuler)", "{n} keys erased (Ctrl+Z to undo)");
    }

    /// Efface toutes les clés de la caméra de prise ; elle reste fixe à sa position actuelle.
    fn clear_camera_keys(&mut self) {
        if self.scene.camera.keys.is_empty() {
            return;
        }
        self.pause_shot_preview();
        let v = self.real_shot(self.frame);
        self.begin_edit();
        let n = self.scene.camera.keys.len();
        self.scene.camera.keys.clear();
        self.scene.camera.base = crate::anim::CamPose { eye: v.eye.to_array(), target: v.target.to_array(), fov_deg: v.fov_deg };
        self.editing = false;
        self.status = trf!("{n} clés de caméra effacées (Ctrl+Z pour annuler)", "{n} camera keys erased (Ctrl+Z to undo)");
    }

    /// Pose une clé sur la caméra de prise (elle devient animée), avec ce qu'on regarde :
    /// la vue de travail en mode Travail, la caméra de prise elle-même en mode Prise.
    fn key_camera(&mut self) {
        self.playing = false;
        self.frame = self.frame.round();
        self.begin_edit();
        self.pause_shot_preview();
        let v = if self.shot_view { self.real_shot(self.frame) } else { self.work_view() };
        let frame = self.cur_frame();
        self.scene.camera.set_key(CamKey {
            frame,
            eye: v.eye.to_array(),
            target: v.target.to_array(),
            fov_deg: v.fov_deg,
            interp: self.interp,
        });
        self.editing = false;
        let source = if self.shot_view { tr!("caméra de prise", "shot camera") } else { tr!("vue de travail", "work view") };
        // une animation dont toutes les clés sont identiques ne bouge pas : on le signale
        let same = |k: &CamKey| {
            Vec3::from(k.eye).distance(v.eye) < 0.01
                && Vec3::from(k.target).distance(v.target) < 0.01
                && (k.fov_deg - v.fov_deg).abs() < 0.01
        };
        let keys = &self.scene.camera.keys;
        // clé voisine (avant ou après) à la même position : seul l'angle changera entre les deux
        let i = keys.partition_point(|k| k.frame < frame);
        let still = [i.checked_sub(1), Some(i + 1)]
            .into_iter()
            .flatten()
            .filter_map(|j| keys.get(j))
            .find(|k| Vec3::from(k.eye).distance(v.eye) < 0.01);
        self.status = if keys.len() > 1 && keys.iter().all(same) {
            trf!(
                "Clé de caméra à l'image {frame} (depuis la {source}) — identique aux autres clés : \
                 la caméra ne bougera pas. Déplacez la vue puis reposez la clé.",
                "Camera key at frame {frame} (from the {source}) — identical to the other keys: \
                 the camera will not move. Move the view then set the key again."
            )
        } else if let Some(k) = still {
            trf!(
                "Clé de caméra à l'image {frame} (depuis la {source}) — même position que la clé de \
                 l'image {} : seul l'angle changera entre les deux. Pour un travelling, déplacez la \
                 caméra (vol) puis reposez la clé.",
                "Camera key at frame {frame} (from the {source}) — same position as the key at \
                 frame {}: only the angle will change between the two. For a dolly move, move the \
                 camera (fly) then set the key again.",
                k.frame
            )
        } else {
            trf!("Clé de caméra à l'image {frame} (depuis la {source})", "Camera key at frame {frame} (from the {source})")
        };
    }

    /// Caméra de prise sous forme orbitale, pour la piloter comme la vue de travail.
    fn shot_orbit(&self) -> OrbitCamera {
        let v = self.shot_ctrl();
        let mut c = OrbitCamera::default();
        c.set_from(v.eye, v.target, v.fov_deg);
        c
    }

    /// Déplace la caméra de prise (clé à l'image courante si elle est animée).
    fn set_shot(&mut self, eye: Vec3, target: Vec3, fov_deg: f32) {
        self.pause_shot_preview();
        if let Some(r) = &mut self.cam_rec {
            r.raw = ViewParams { eye, target, fov_deg };
            return;
        }
        let frame = self.cur_frame();
        let cam = &self.scene.camera;
        if !cam.keys.is_empty() {
            // une caméra animée prend une clé à l'image courante : on dit laquelle, et surtout
            // si le geste réécrit une clé déjà posée (piège classique : oublier de changer d'image)
            let existed = match self.shot_key_touch {
                Some((f, existed)) if f == frame => existed,
                _ => cam.keys.binary_search_by_key(&frame, |k| k.frame).is_ok(),
            };
            self.shot_key_touch = Some((frame, existed));
            self.status = if existed {
                trf!(
                    "Clé de caméra de l'image {frame} modifiée — pour une nouvelle clé, changez d'image \
                     avant de bouger la caméra (Ctrl+Z pour annuler)",
                    "Camera key at frame {frame} changed — for a new key, change frame \
                     before moving the camera (Ctrl+Z to undo)"
                )
            } else {
                trf!("Nouvelle clé de caméra à l'image {frame}", "New camera key at frame {frame}")
            };
        }
        self.scene.camera.set_pose(frame, eye, target, fov_deg, self.interp);
    }

    /// Pose que les commandes font bouger : la pose brute pendant une prise (jamais l'aperçu).
    fn shot_ctrl(&self) -> ViewParams {
        match &self.cam_rec {
            Some(r) => r.raw,
            None => self.real_shot(self.frame),
        }
    }

    /// Début d'un geste sur la caméra de prise. Enregistrement armé : lance la lecture et la
    /// prise en direct ; sinon, arrête la lecture et prépare une clé à l'image courante.
    fn begin_shot_control(&mut self) {
        if self.cam_rec.is_some() {
            return;
        }
        self.frame = self.frame.round();
        self.begin_edit();
        self.pause_shot_preview();
        if !self.cam_rec_armed {
            self.playing = false;
            return;
        }
        if self.cur_frame() >= self.end {
            self.playing = false;
            self.status = tr!("Enregistrement impossible : la tête de lecture est sur la dernière image.", "Cannot record: the playhead is on the last frame.").into();
            return;
        }
        let pose = self.real_shot(self.frame);
        let frame = self.cur_frame();
        self.cam_rec = Some(CamRec::start(&mut self.scene.camera, frame, pose));
        self.playing = true;
        self.last_tick = Instant::now();
        self.status = trf!("⏺ Enregistrement de la caméra depuis l'image {frame}…", "⏺ Recording the camera from frame {frame}…");
    }

    /// Une image de prise : stabilise, enregistre, et termine la prise quand on lâche les
    /// commandes (une fois la caméra posée) ou quand la lecture s'arrête.
    fn record_tick(&mut self, dt: f32) {
        let controlling = self.fly_moving || matches!(self.drag, Some(DragKind::Look | DragKind::Orbit | DragKind::Pan));
        let stabilize = self.config.drone.stabilize;
        let Some(rec) = &mut self.cam_rec else {
            return;
        };
        rec.smooth(dt, stabilize);
        rec.record(&mut self.scene.camera, self.frame);
        let done = !self.playing || self.frame >= self.end as f32 || (!controlling && rec.settled());
        if done {
            self.stop_recording();
        }
        self.force_render = true;
    }

    fn stop_recording(&mut self) {
        let Some(mut rec) = self.cam_rec.take() else {
            return;
        };
        // dernière clé à l'image atteinte
        let end = self.frame.min(self.end as f32).ceil();
        rec.record(&mut self.scene.camera, end);
        let (a, b) = (rec.first_key, rec.last_key());
        let fixed = camrec::smooth_jerks(&mut self.scene.camera, a, b, self.config.drone.stabilize);
        self.playing = false;
        self.set_frame(b as f32);
        self.editing = false;
        self.status = if fixed > 0 {
            trf!("Caméra enregistrée de l'image {a} à {b} ({fixed} à-coups adoucis)", "Camera recorded from frame {a} to {b} ({fixed} jolts smoothed)")
        } else {
            trf!("Caméra enregistrée de l'image {a} à {b}", "Camera recorded from frame {a} to {b}")
        };
    }

    fn set_shot_orbit(&mut self, c: &OrbitCamera) {
        self.set_shot(c.eye(), c.target, c.fov_deg);
    }

    /// Déplace la caméra pilotée (travail, ou prise en vue « Prise ») sans changer son regard.
    /// En vue Prise, l'appelant ouvre la série d'annulation (`begin_edit`).
    fn move_view(&mut self, d: Vec3) {
        if self.shot_view {
            let v = self.shot_ctrl();
            self.set_shot(v.eye + d, v.target + d, v.fov_deg);
        } else {
            self.cam.target += d;
        }
        self.force_render = true;
    }

    /// Clic droit enfoncé dans la vue : début de la visée libre. Le curseur est caché et reste
    /// sur place ; seuls les mouvements bruts de la souris font tourner la caméra.
    fn begin_look(&mut self, ctx: &egui::Context) {
        self.drag = Some(DragKind::Look);
        self.look_anchor = ctx.input(|i| i.pointer.latest_pos());
        if self.shot_view {
            self.begin_shot_control();
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::CursorGrab::Confined));
    }

    /// Une image de visée : applique le mouvement de la souris au regard.
    fn look_step(&mut self, ctx: &egui::Context) {
        let (raw, pointer, pos) = ctx.input(|i| {
            let mut raw: Option<Vec2> = None;
            for ev in &i.events {
                if let egui::Event::MouseMoved(d) = ev {
                    *raw.get_or_insert(Vec2::ZERO) += Vec2::new(d.x, d.y);
                }
            }
            (raw, Vec2::new(i.pointer.delta().x, i.pointer.delta().y), i.pointer.latest_pos())
        });
        self.raw_mouse |= raw.is_some();
        // mouvements bruts si le système les fournit (pas de bord d'écran), sinon le curseur
        let delta = if self.raw_mouse { raw.unwrap_or(Vec2::ZERO) } else { pointer };
        // curseur ramené à son point de départ : il ne survole pas les panneaux pendant la visée
        if self.raw_mouse {
            if let (Some(a), Some(p)) = (self.look_anchor, pos) {
                if a.distance(p) > 1.0 {
                    ctx.send_viewport_cmd(egui::ViewportCommand::CursorPosition(a));
                }
            }
        }
        if delta == Vec2::ZERO {
            return;
        }
        let d = self.config.drone;
        let rate = d.look_rate() * self.current_view().fov_deg.clamp(5.0, 120.0) / 60.0; // plus lent au téléobjectif
        let turn = Vec2::new(delta.x, if d.invert_y { -delta.y } else { delta.y }) * rate;
        if self.shot_view {
            let mut c = self.shot_orbit();
            c.look(turn);
            self.set_shot_orbit(&c);
        } else {
            self.cam.look(turn);
        }
        self.force_render = true;
    }

    /// Clic droit relâché (ou fenêtre quittée) : fin de la visée, le curseur réapparaît.
    fn end_look(&mut self, ctx: &egui::Context) {
        if self.drag == Some(DragKind::Look) {
            self.drag = None;
            if !self.fly_moving {
                self.editing = false;
            }
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::CursorGrab::None));
        ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(true));
        if let Some(a) = self.look_anchor.take() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CursorPosition(a));
        }
    }

    /// Place la caméra de prise exactement comme la vue de travail.
    fn shot_from_work(&mut self) {
        self.playing = false;
        self.frame = self.frame.round();
        self.begin_edit();
        let v = self.work_view();
        self.set_shot(v.eye, v.target, v.fov_deg);
        self.editing = false;
        self.status = if self.scene.camera.keys.is_empty() {
            tr!("Caméra de prise placée sur la vue de travail", "Shot camera placed on the work view").into()
        } else {
            trf!("Caméra de prise placée sur la vue de travail (clé à l'image {})", "Shot camera placed on the work view (key at frame {})", self.cur_frame())
        };
    }

    /// Amène la vue de travail sur la caméra de prise.
    fn work_from_shot(&mut self) {
        let v = self.shot_params(self.frame);
        self.cam.set_from(v.eye, v.target, v.fov_deg);
        self.shot_view = false;
    }

    /// Mise au point de la profondeur de champ pour une vue : sa cible, ou la distance du projet.
    fn set_focus(&mut self, view: &ViewParams) {
        let fx = &self.post_fx;
        self.renderer.post_settings.focus = if fx.focus_auto { (view.target - view.eye).length() } else { fx.focus };
    }

    fn reset_bone(&mut self, all: bool) {
        let frame = self.cur_frame();
        let interp = self.interp;
        let sel = self.sel_bone;
        if self.sel_actor.is_none() {
            return;
        }
        self.begin_edit();
        if let Some(a) = self.sel_actor.and_then(|id| self.scene.actor_mut(id)) {
            let n = a.pose.len();
            let bones: Vec<usize> = if all { (0..n).collect() } else { sel.into_iter().collect() };
            for b in bones {
                let rest = a.model.skeleton.bones[b].rest_local;
                a.key_bone(b, frame, rest, interp);
            }
        }
        self.editing = false;
        self.scene.update_world();
    }

    /// Personnage visé par le copier-coller : une pièce fusionnée (casque, armure…) renvoie à son
    /// personnage, car c'est lui qui porte la pose.
    fn pose_target(&self) -> Option<u64> {
        self.sel_actor.filter(|&id| self.scene.actor(id).is_some()).map(|id| self.scene.merge_root(id))
    }

    fn pose_op(&mut self, ctx: &egui::Context, op: PoseOp) {
        match op {
            PoseOp::Copy => self.copy_pose(ctx),
            PoseOp::Paste => self.paste_pose(false),
            PoseOp::PasteMirror => self.paste_pose(true),
            PoseOp::PasteAnim => self.paste_anim(),
            PoseOp::PastePosition => self.paste_position(),
        }
    }

    /// Place le personnage sélectionné à la position et l'orientation (monde) du personnage
    /// copié, à l'image courante. Son échelle ne change pas.
    fn paste_position(&mut self) {
        let Some(target) = self.pose_target() else {
            self.status = tr!("Sélectionnez le personnage à déplacer", "Select the character to move").into();
            return;
        };
        let Some(clip) = self.clipboard_pose.as_ref() else { return };
        let (from, src_world) = (clip.from.clone(), clip.root_world);
        let frame = self.cur_frame();
        let interp = self.interp;
        self.begin_edit();
        let mut name = String::new();
        if let Some(a) = self.scene.actor_mut(target) {
            name = a.name.clone();
            // repère du parent (monde, ou os auquel le personnage est accroché)
            let parent = Self::parent_world(a, None);
            let (_, r, p) = (parent.inverse() * src_world).to_scale_rotation_translation();
            let t = Transform { translation: p, rotation: r.normalize(), scale: a.root_pose.scale };
            a.key_root(frame, t, interp, false);
        }
        self.editing = false;
        self.scene.update_world();
        self.status = trf!("« {name} » placé à la position de « {from} » (image {frame})", "“{name}” placed at the position of “{from}” (frame {frame})");
    }

    fn copy_pose(&mut self, ctx: &egui::Context) {
        let Some(a) = self.pose_target().and_then(|id| self.scene.actor(id)) else { return };
        let bones = a
            .model
            .skeleton
            .bones
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let clip = BoneClip {
                    rest: b.rest_local,
                    pose: a.pose[i],
                    track: a.anim.bones.get(i).cloned().unwrap_or_default(),
                };
                (b.name.to_ascii_lowercase(), clip)
            })
            .collect();
        let clip = PoseClipboard { from: a.name.clone(), frame: self.cur_frame(), bones, root_world: a.root_world };
        self.status = trf!(
            "Pose de « {} » copiée (image {}). Sélectionnez un autre personnage puis Ctrl+V (Ctrl+Maj+V : en miroir).",
            "Pose of “{}” copied (frame {}). Select another character then Ctrl+V (Ctrl+Shift+V: mirrored).",
            clip.from, clip.frame
        );
        self.clipboard_pose = Some(clip);
        ctx.copy_text(POSE_CLIP_TEXT.into());
    }

    fn paste_pose(&mut self, mirror: bool) {
        let Some(target) = self.pose_target() else {
            self.status = tr!("Sélectionnez le personnage sur lequel coller la pose", "Select the character to paste the pose onto").into();
            return;
        };
        let Some(clip) = self.clipboard_pose.take() else { return };
        let frame = self.cur_frame();
        let interp = self.interp;
        self.begin_edit();
        let mut name = String::new();
        if let Some(a) = self.scene.actor_mut(target) {
            name = a.name.clone();
            // en miroir : on échange L_/R_ et on reflète la rotation *monde* de repos
            for (i, bone) in a.model.skeleton.bones.clone().iter().enumerate() {
                let src_name = if mirror {
                    if let Some(r) = bone.name.strip_prefix("L_") {
                        format!("R_{r}")
                    } else if let Some(r) = bone.name.strip_prefix("R_") {
                        format!("L_{r}")
                    } else {
                        bone.name.clone()
                    }
                } else {
                    bone.name.clone()
                };
                let Some(src) = clip.bones.get(&src_name.to_ascii_lowercase()) else { continue };
                let t = if mirror {
                    // delta par rapport au repos de l'os source, reflété (x -> -x)
                    let d = src.rest.rotation.inverse() * src.pose.rotation;
                    let dm = Quat::from_xyzw(d.x, -d.y, -d.z, d.w);
                    Transform { rotation: (bone.rest_local.rotation * dm).normalize(), ..bone.rest_local }
                } else {
                    adapt_bone(src.pose, src.rest, bone.rest_local)
                };
                a.key_bone(i, frame, t, interp);
            }
        }
        self.editing = false;
        self.scene.update_world();
        self.status = if mirror {
            trf!("Pose de « {} » collée en miroir sur « {name} » (image {frame})", "Pose of “{}” pasted mirrored onto “{name}” (frame {frame})", clip.from)
        } else {
            trf!("Pose de « {} » collée sur « {name} » (image {frame})", "Pose of “{}” pasted onto “{name}” (frame {frame})", clip.from)
        };
        self.clipboard_pose = Some(clip);
    }

    /// Remplace l'animation des os du personnage sélectionné par celle du personnage copié
    /// (toutes les clés, pas seulement l'image courante). La position du personnage ne change pas.
    fn paste_anim(&mut self) {
        let Some(target) = self.pose_target() else {
            self.status = tr!("Sélectionnez le personnage sur lequel coller l'animation", "Select the character to paste the animation onto").into();
            return;
        };
        let Some(clip) = self.clipboard_pose.take() else { return };
        self.begin_edit();
        let mut name = String::new();
        if let Some(a) = self.scene.actor_mut(target) {
            name = a.name.clone();
            for (i, bone) in a.model.skeleton.bones.iter().enumerate() {
                let Some(src) = clip.bones.get(&bone.name.to_ascii_lowercase()) else { continue };
                let Some(track) = a.anim.bones.get_mut(i) else { continue };
                track.keys = src
                    .track
                    .keys
                    .iter()
                    .map(|k| Key::new(k.frame, &adapt_bone(k.transform(), src.rest, bone.rest_local), k.interp))
                    .collect();
            }
        }
        self.editing = false;
        self.scene.evaluate(self.frame);
        self.status = trf!("Animation de « {} » collée sur « {name} » ({} clés)", "Animation of “{}” pasted onto “{name}” ({} keys)", clip.from, clip.key_count());
        self.clipboard_pose = Some(clip);
    }

    /// Oriente la grille comme la racine du personnage sélectionné.
    fn align_grid_to_selection(&mut self) {
        if let Some(a) = self.pose_target().and_then(|id| self.scene.actor(id)) {
            let x = a.root_world.x_axis;
            self.grid_yaw = (-x.z).atan2(x.x).to_degrees();
        }
    }

    /// Pose la grille aux pieds du personnage sélectionné (sur un toit, un balcon…).
    fn place_grid_at_selection(&mut self) {
        let Some(a) = self.pose_target().and_then(|id| self.scene.actor(id)) else {
            self.status = tr!("Sélectionnez d'abord un personnage pour y placer la grille", "First select a character to put the grid under").into();
            return;
        };
        self.grid_origin = a.root_world.w_axis.truncate();
        self.show_grid = true;
        let o = self.grid_origin;
        self.status = trf!("Grille placée sous « {} » ({:.2}, {:.2}, {:.2})", "Grid placed under “{}” ({:.2}, {:.2}, {:.2})", a.name, o.x, o.y, o.z);
    }

    /// Pose le personnage sélectionné au centre de la grille : ses pieds (la racine) vont sur
    /// l'origine de la grille, à l'image courante. Son orientation et son échelle ne changent pas.
    fn place_selection_on_grid(&mut self) {
        let Some(target) = self.pose_target() else {
            self.status = tr!("Sélectionnez d'abord un personnage à poser sur la grille", "First select a character to put on the grid").into();
            return;
        };
        let world = self.grid_origin;
        self.show_grid = true;
        let frame = self.cur_frame();
        let interp = self.interp;
        self.begin_edit();
        let mut name = String::new();
        if let Some(a) = self.scene.actor_mut(target) {
            name = a.name.clone();
            // repère du parent (monde, ou os auquel le personnage est accroché)
            let parent = Self::parent_world(a, None);
            let p = parent.inverse().transform_point3(world);
            let t = Transform { translation: p, ..a.root_pose };
            a.key_root(frame, t, interp, false);
        }
        self.editing = false;
        self.scene.update_world();
        self.status = trf!(
            "« {name} » posé au centre de la grille ({:.2}, {:.2}, {:.2}, image {frame})",
            "“{name}” put at the center of the grid ({:.2}, {:.2}, {:.2}, frame {frame})",
            world.x, world.y, world.z
        );
    }

    fn grid_rotation(&self) -> Quat {
        Quat::from_rotation_y(self.grid_yaw.to_radians())
    }

    fn delete_selected_actor(&mut self) {
        if let Some(id) = self.sel_actor.take() {
            self.history.push(self.scene.snapshot());
            self.scene.remove(id);
            self.renderer.forget_actor(id);
            self.sel_bone = None;
            self.scene.evaluate(self.frame);
        }
    }

    /// Annuler (`redo` faux) ou rétablir : la liste des acteurs peut changer (suppression,
    /// ajout), on libère ceux qui s'en vont et on lâche une sélection devenue orpheline.
    fn undo_redo(&mut self, redo: bool) {
        let before: Vec<u64> = self.scene.actors.iter().map(|a| a.id).collect();
        let done = if redo { self.history.redo(&mut self.scene) } else { self.history.undo(&mut self.scene) };
        if !done {
            return;
        }
        for id in before {
            if self.scene.actor(id).is_none() {
                self.renderer.forget_actor(id);
            }
        }
        if self.sel_actor.is_some_and(|id| self.scene.actor(id).is_none()) {
            self.sel_actor = None;
            self.sel_bone = None;
        }
        self.scene.evaluate(self.frame);
        self.status = if redo { tr!("Rétabli", "Redone") } else { tr!("Annulé", "Undone") }.into();
    }

    /// Supprime un acteur et tout ce qui lui est attaché (pièces fusionnées, objets accrochés à un os).
    fn delete_actor_tree(&mut self, id: u64) {
        let ids: Vec<u64> = std::iter::once(id).chain(self.scene.descendants(id)).collect();
        self.history.push(self.scene.snapshot());
        for &d in &ids {
            self.scene.remove(d);
            self.renderer.forget_actor(d);
            self.expanded_groups.remove(&d);
        }
        if self.sel_actor.is_some_and(|s| ids.contains(&s)) {
            self.sel_actor = None;
            self.sel_bone = None;
        }
        self.scene.evaluate(self.frame);
    }

    fn frame_selection(&mut self) {
        let target = if self.sel_bone.is_some() {
            self.selection_frame().map(|(p, _)| (p, 0.6))
        } else if let Some(a) = self.sel_actor.and_then(|id| self.scene.actor(id)) {
            let (lo, hi) = a.bounds_world();
            Some(((lo + hi) * 0.5, (hi - lo).length().max(0.5)))
        } else {
            None
        };
        if let Some((c, size)) = target {
            if self.shot_view && self.cam_rec.is_some() {
                // pendant une prise : recadrage en douceur, enregistré
                let mut cam = self.shot_orbit();
                cam.target = c;
                cam.distance = size * 1.6 * (22.5_f32.to_radians()).tan() / (cam.fov_deg.to_radians() * 0.5).tan();
                self.set_shot_orbit(&cam);
            } else if self.shot_view {
                self.playing = false;
                self.frame = self.frame.round();
                self.begin_edit();
                let mut cam = self.shot_orbit();
                cam.target = c;
                // même cadrage que la vue de travail (45°), corrigé pour la focale de la prise
                cam.distance = size * 1.6 * (22.5_f32.to_radians()).tan() / (cam.fov_deg.to_radians() * 0.5).tan();
                self.set_shot_orbit(&cam);
                self.editing = false;
            } else {
                self.cam.target = c;
                self.cam.distance = size * 1.6;
            }
        }
    }

    // -------------------------------------------------------------- projet

    fn project_json(&self) -> anyhow::Result<String> {
        let pf = ProjectFile {
            format: "PlanetStudio 1".into(),
            fps: self.fps,
            start: self.start,
            end: self.end,
            actors: self.scene.actors.iter().map(ActorFile::from_actor).collect(),
            camera: self.scene.camera.clone(),
            lighting: self.lighting.clone(),
            sun: self.scene.sun.clone(),
            ambiance_keys: self.scene.ambiance.clone(),
            decor: self.decor.applied.clone(),
            grid_yaw: self.grid_yaw,
            grid_origin: self.grid_origin.to_array(),
            particles: self.particles.clone(),
            post: self.post_fx,
            export_marks: self.export.marks,
            markers: self.markers.clone(),
            work_camera: Some(self.cam),
        };
        Ok(serde_json::to_string_pretty(&pf)?)
    }

    fn save_project(&mut self) {
        let path = PathBuf::from(self.project_path.trim());
        let r = self.project_json().and_then(|json| write_project(&path, &json).map(|()| json));
        self.status = match r {
            Ok(json) => {
                self.mark_saved(path.clone(), json);
                trf!("Projet enregistré : {}", "Project saved: {}", path.display())
            }
            Err(e) => trf!("Erreur d'enregistrement : {e:#}", "Error while saving: {e:#}"),
        };
    }

    /// Le projet vit désormais dans `path`, avec le contenu `json`.
    fn mark_saved(&mut self, path: PathBuf, json: String) {
        self.config.last_project = Some(path.display().to_string());
        self.config.save();
        self.project_path = path.display().to_string();
        self.project_file = Some(path);
        self.saved_json = Some(json);
        self.last_autosave = Instant::now();
    }

    /// Enregistre le projet s'il a changé depuis le dernier enregistrement : dans son fichier, ou
    /// dans un nouveau « sans_titre_N.psproj » (Documents/PlanetStudio) s'il n'en a pas encore.
    fn autosave(&mut self) {
        self.last_autosave = Instant::now();
        if self.game.is_none() || self.opening {
            return;
        }
        // projet sans titre et vide : rien à garder
        if self.project_file.is_none() && self.scene.actors.is_empty() {
            return;
        }
        let json = match self.project_json() {
            Ok(j) => j,
            Err(e) => {
                self.status = trf!("Échec de l'enregistrement automatique : {e:#}", "Autosave failed: {e:#}");
                return;
            }
        };
        if self.saved_json.as_deref() == Some(json.as_str()) {
            return;
        }
        let path = self.project_file.clone().unwrap_or_else(untitled_path);
        match write_project(&path, &json) {
            Ok(()) => {
                self.status = trf!("Projet enregistré automatiquement : {}", "Project autosaved: {}", path.display());
                self.mark_saved(path, json);
            }
            Err(e) => self.status = trf!("Échec de l'enregistrement automatique : {e:#}", "Autosave failed: {e:#}"),
        }
    }

    /// À chaque image : enregistrement automatique quand l'intervalle est écoulé.
    fn autosave_tick(&mut self, ctx: &egui::Context) {
        if self.opening && self.pending.is_empty() {
            // projet ouvert entièrement chargé : c'est l'état de référence (pas de modification)
            self.opening = false;
            self.saved_json = self.project_json().ok();
        }
        let a = self.config.autosave;
        // file d'attente en cours : la scène est celle d'un export, pas le projet de l'utilisateur
        if !a.enabled || self.export.queue_running() {
            return;
        }
        let every = std::time::Duration::from_secs(u64::from(a.minutes.max(1)) * 60);
        let since = self.last_autosave.elapsed();
        if since < every {
            // réveille l'application à l'échéance, même si l'utilisateur ne touche plus à rien
            ctx.request_repaint_after(every - since);
        } else if self.pending.is_empty() && self.drag.is_none() {
            self.autosave();
            ctx.request_repaint_after(every);
        } else {
            // chargement ou manipulation en cours : on réessaie un peu plus tard
            ctx.request_repaint_after(std::time::Duration::from_secs(2));
        }
    }

    fn new_project(&mut self) {
        if self.config.autosave.enabled {
            self.autosave();
        }
        for a in &self.scene.actors {
            self.renderer.forget_actor(a.id);
        }
        self.scene = Scene::default();
        self.history.clear();
        self.sel_actor = None;
        self.sel_bone = None;
        self.lighting = Lighting::default();
        self.grid_yaw = 0.0;
        self.grid_origin = Vec3::ZERO;
        self.post_fx = Default::default();
        self.export.marks = None;
        self.markers.clear();
        self.cam = OrbitCamera { fov_deg: self.cam.fov_deg, ..Default::default() };
        self.clear_decor();
        self.set_frame(0.0);
        self.project_file = None;
        self.saved_json = None;
        self.opening = false;
        self.project_path = projects_dir().join("scene.psproj").display().to_string();
    }

    fn open_project(&mut self) {
        let path = PathBuf::from(self.project_path.trim());
        let pf: ProjectFile = match std::fs::read_to_string(&path)
            .map_err(anyhow::Error::from)
            .and_then(|s| serde_json::from_str(&s).map_err(anyhow::Error::from))
        {
            Ok(p) => p,
            Err(e) => {
                self.status = trf!("Impossible d'ouvrir {} : {e:#}", "Could not open {}: {e:#}", path.display());
                return;
            }
        };
        if self.game.is_none() {
            self.status = tr!("Ouvrez d'abord le dossier du jeu", "Open the game folder first").into();
            return;
        }
        // garde le projet en cours avant de le remplacer (sauf si on rouvre le même fichier :
        // c'est alors pour revenir à la version enregistrée)
        if self.config.autosave.enabled && self.project_file.as_ref() != Some(&path) {
            self.autosave();
        }
        for a in &self.scene.actors {
            self.renderer.forget_actor(a.id);
        }
        self.scene = Scene::default();
        self.scene.camera = pf.camera.clone();
        self.history.clear();
        self.sel_actor = None;
        self.sel_bone = None;
        self.fps = pf.fps;
        self.start = pf.start;
        self.end = pf.end;
        self.lighting = pf.lighting.clone();
        self.grid_yaw = pf.grid_yaw;
        self.grid_origin = Vec3::from_array(pf.grid_origin);
        self.particles = pf.particles.clone();
        self.post_fx = pf.post;
        self.export.marks = pf.export_marks;
        self.markers = pf.markers.clone();
        if let Some(c) = pf.work_camera {
            self.cam = c;
        }
        self.scene.sun = pf.sun.clone();
        self.scene.ambiance = pf.ambiance_keys.clone();
        self.scene.next_id = pf.actors.iter().map(|a| a.id + 1).max().unwrap_or(1);
        match pf.decor.clone() {
            Some(d) if self.decor.applied.as_ref() != Some(&d) => {
                self.renderer.decor_visible = d.visible;
                self.decor.draft = d.clone();
                self.decor.ground_auto = d.ground.is_none();
                self.request_decor(d);
            }
            Some(d) => {
                self.renderer.decor_visible = d.visible;
                if let Some(a) = &mut self.decor.applied {
                    a.visible = d.visible;
                }
            }
            None => self.clear_decor(),
        }
        for af in pf.actors {
            let asset = af.asset.clone();
            self.request_model(&asset, PendingAdd { attach: None, file: Some(af), select: false, children: Vec::new(), ..Default::default() });
        }
        self.config.last_project = Some(path.display().to_string());
        self.config.save();
        self.project_path = path.display().to_string();
        self.project_file = Some(path.clone());
        self.saved_json = None;
        self.opening = true;
        self.last_autosave = Instant::now();
        self.status = trf!("Projet ouvert : {}", "Project opened: {}", path.display());
    }

    // -------------------------------------------------------------- vue

    fn work_view(&self) -> ViewParams {
        ViewParams { eye: self.cam.eye(), target: self.cam.target, fov_deg: self.cam.fov_deg }
    }

    /// Caméra de prise affichée à une image : l'aperçu de la caméra prédéfinie s'il y en a un.
    fn shot_params(&self, frame: f32) -> ViewParams {
        if self.cam_rec.is_none() {
            if let Some(track) = self.shot_preview.as_ref().and_then(|p| p.track.as_ref()) {
                let (eye, target, fov_deg) = track.pose(frame);
                return ViewParams { eye, target, fov_deg };
            }
        }
        self.real_shot(frame)
    }

    /// Caméra de prise réelle à une image (sans aperçu) : celle qu'on modifie et qu'on exporte.
    fn real_shot(&self, frame: f32) -> ViewParams {
        // pendant une prise : la pose stabilisée, celle qui s'enregistre
        if let Some(r) = &self.cam_rec {
            return r.out;
        }
        let (eye, target, fov_deg) = self.scene.camera.pose(frame);
        ViewParams { eye, target, fov_deg }
    }

    /// La caméra de prise est modifiée à la main : l'aperçu de la caméra prédéfinie
    /// s'efface (il revient au prochain changement de réglage).
    fn pause_shot_preview(&mut self) {
        if let Some(p) = &mut self.shot_preview {
            if p.track.take().is_some() {
                self.force_render = true;
            }
        }
    }

    /// Proportions de la caméra de prise (son format de prise de vue).
    fn shot_aspect(&self) -> f32 {
        let (w, h) = self.shot_format();
        w as f32 / h as f32
    }

    fn current_view(&self) -> ViewParams {
        if self.shot_view {
            self.shot_params(self.frame)
        } else {
            self.work_view()
        }
    }

    fn draw_items(&self, highlight: bool) -> Vec<DrawItem> {
        self.scene
            .drawn_actors()
            .map(|a| DrawItem {
                id: a.id,
                asset: a.asset.clone(),
                skin: a.skin_matrices(),
                highlight: highlight && self.sel_actor == Some(a.id) && self.sel_bone.is_none(),
                camo: a.camo.clone(),
                decal: a.decal.clone(),
            })
            .collect()
    }

    fn gizmo_radius(&self, pivot: Vec3, view: &ViewParams) -> f32 {
        view.eye.distance(pivot) * 0.12
    }

    fn build_lines(&self, view: &ViewParams) -> Lines {
        let mut l = Lines::default();
        if self.show_grid {
            grid_lines(&mut l, 20, self.grid_rotation(), self.grid_origin);
        }
        // caméra de prise visible depuis la vue de travail : pyramide de visée au format de l'export
        if !self.shot_view {
            let v = self.shot_params(self.frame);
            let c = [0.3, 0.9, 1.0, 0.95];
            let fwd = (v.target - v.eye).normalize_or(Vec3::NEG_Z);
            let right = fwd.cross(Vec3::Y).normalize_or(Vec3::X);
            let up = right.cross(fwd);
            let d = 0.45;
            let hh = (v.fov_deg.to_radians() * 0.5).tan() * d;
            let hw = hh * self.shot_aspect();
            let corners = [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)]
                .map(|(x, y): (f32, f32)| v.eye + fwd * d + right * hw * x + up * hh * y);
            for i in 0..4 {
                l.line(v.eye, corners[i], c, true);
                l.line(corners[i], corners[(i + 1) % 4], c, true);
            }
            // petit triangle sur le bord haut : indique le haut de l'image
            let top = v.eye + fwd * d + up * hh * 1.4;
            l.line(corners[2] - right * hw * 0.5, top, c, true);
            l.line(corners[3] + right * hw * 0.5, top, c, true);
            // corps de la caméra
            let b = 0.07;
            let back = v.eye - fwd * b * 2.5;
            for (x, y) in [(-1.0_f32, -1.0_f32), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                l.line(v.eye + (right * x + up * y) * b, back + (right * x + up * y) * b, c, true);
            }
            l.line(v.eye, v.target, [0.3, 0.9, 1.0, 0.3], true);
        }
        // soleil : disque et rayons à 2,5 m du centre de la scène, dans sa direction
        if !self.shot_view && self.show_sun {
            let roots: Vec<Vec3> = self
                .scene
                .actors
                .iter()
                .filter(|a| a.visible && a.attachment.is_none())
                .map(|a| a.root_world.w_axis.truncate())
                .collect();
            let base = if roots.is_empty() { Vec3::ZERO } else { roots.iter().copied().sum::<Vec3>() / roots.len() as f32 };
            let center = base + Vec3::Y;
            let sun_now = self.lighting.with_sun(self.sun_at(self.frame));
            let to_sun = -sun_now.light_dir();
            let sun = center + to_sun * 2.5;
            let rgb = Vec3::from(sun_now.sun_color);
            let rgb = rgb / rgb.max_element().max(1e-3);
            let c = [rgb.x, rgb.y, rgb.z, 0.95];
            let r = 0.14;
            l.circle(sun, to_sun, r, c);
            let (u, w) = to_sun.any_orthonormal_pair();
            for k in 0..8 {
                let a = k as f32 / 8.0 * std::f32::consts::TAU;
                let d = u * a.cos() + w * a.sin();
                l.line(sun + d * r * 1.4, sun + d * r * 2.0, c, true);
            }
            // rayon vers la scène, et ombre portée de ce rayon au sol
            l.line(sun - to_sun * r, center, [c[0], c[1], c[2], 0.35], true);
            let ground = Vec3::new(sun.x, 0.0, sun.z);
            l.line(ground, Vec3::new(center.x, 0.0, center.z), [c[0], c[1], c[2], 0.25], false);
        }
        // squelettes
        if self.show_skeleton {
            for a in &self.scene.actors {
                let selected = self.sel_actor == Some(a.id);
                if !a.visible || (!selected && !self.show_all_skeletons) {
                    continue;
                }
                for (i, b) in a.model.skeleton.bones.iter().enumerate() {
                    let p = a.world[i].w_axis.truncate();
                    let col = if selected && self.sel_bone == Some(i) {
                        [1.0, 0.85, 0.1, 1.0]
                    } else if selected {
                        [0.55, 0.85, 1.0, 0.85]
                    } else {
                        [0.6, 0.6, 0.6, 0.6]
                    };
                    if let Some(par) = b.parent {
                        if par != 0 {
                            l.line(a.world[par].w_axis.truncate(), p, col, true);
                        }
                    }
                    let s = view.eye.distance(p) * 0.006;
                    for d in [Vec3::X, Vec3::Y, Vec3::Z] {
                        l.line(p - d * s, p + d * s, col, true);
                    }
                }
            }
        }
        // gizmo
        if let Some((pivot, rot)) = self.selection_frame() {
            let r = self.gizmo_radius(pivot, view);
            let axes = [
                (rot * Vec3::X, [0.95, 0.25, 0.25, 1.0]),
                (rot * Vec3::Y, [0.3, 0.9, 0.3, 1.0]),
                (rot * Vec3::Z, [0.3, 0.5, 1.0, 1.0]),
            ];
            let active = self.drag.or(self.hover_axis);
            let hl = |k: DragKind, c: [f32; 4]| if active == Some(k) { [1.0, 0.95, 0.3, 1.0] } else { c };
            match self.gizmo {
                GizmoMode::Rotate => {
                    for (ax, c) in axes {
                        l.circle(pivot, ax, r, hl(DragKind::RotateAxis(ax), c));
                    }
                    l.circle(pivot, (view.eye - pivot).normalize(), r * 1.15, hl(DragKind::RotateFree, [0.8, 0.8, 0.8, 0.5]));
                }
                GizmoMode::Translate => {
                    for (ax, c) in [(Vec3::X, axes[0].1), (Vec3::Y, axes[1].1), (Vec3::Z, axes[2].1)] {
                        let c = hl(DragKind::TranslateAxis(ax), c);
                        let tip = pivot + ax * r * 1.2;
                        l.line(pivot, tip, c, true);
                        let (u, w) = ax.any_orthonormal_pair();
                        for d in [u, -u, w, -w] {
                            l.line(tip, tip - ax * r * 0.15 + d * r * 0.06, c, true);
                        }
                    }
                    l.circle(pivot, (view.eye - pivot).normalize(), r * 0.15, hl(DragKind::TranslateFree, [0.9, 0.9, 0.9, 0.8]));
                }
            }
        }
        l
    }

    /// Détermine la poignée de gizmo sous la souris.
    fn pick_gizmo(&self, mouse: Vec2, vp: &Mat4, size: Vec2, view: &ViewParams) -> Option<DragKind> {
        let (pivot, rot) = self.selection_frame()?;
        let r = self.gizmo_radius(pivot, view);
        let center = camera::project(vp, pivot, size)?;
        let tol = 8.0;
        match self.gizmo {
            GizmoMode::Rotate => {
                let mut best: Option<(f32, DragKind)> = None;
                for ax in [rot * Vec3::X, rot * Vec3::Y, rot * Vec3::Z] {
                    let (u, w) = ax.any_orthonormal_pair();
                    let pts: Vec<Vec2> = (0..=48)
                        .filter_map(|i| {
                            let a = i as f32 / 48.0 * std::f32::consts::TAU;
                            camera::project(vp, pivot + (u * a.cos() + w * a.sin()) * r, size)
                        })
                        .collect();
                    for s in pts.windows(2) {
                        let d = dist_to_segment(mouse, s[0], s[1]);
                        if d < tol && best.is_none_or(|(bd, _)| d < bd) {
                            best = Some((d, DragKind::RotateAxis(ax)));
                        }
                    }
                }
                if let Some((_, k)) = best {
                    return Some(k);
                }
                let edge = camera::project(vp, pivot + view_up(view) * r * 1.15, size)?;
                let rad = edge.distance(center);
                let dist = mouse.distance(center);
                // anneau blanc : rotation libre ; intérieur du disque : rotation libre seulement hors
                // d'un personnage (sur un corps, le glisser déplace le personnage)
                let on_ring = (dist - rad).abs() < tol * 1.5;
                let inside = dist < rad && self.actor_under(mouse, vp, size).is_none();
                (on_ring || inside).then_some(DragKind::RotateFree)
            }
            GizmoMode::Translate => {
                for ax in [Vec3::X, Vec3::Y, Vec3::Z] {
                    let tip = camera::project(vp, pivot + ax * r * 1.2, size)?;
                    if dist_to_segment(mouse, center, tip) < tol && mouse.distance(center) > 6.0 {
                        return Some(DragKind::TranslateAxis(ax));
                    }
                }
                (mouse.distance(center) < 14.0).then_some(DragKind::TranslateFree)
            }
        }
    }

    /// Personnage visible sous le curseur (le plus proche) : (distance le long du rayon, id).
    fn actor_under(&self, mouse: Vec2, vp: &Mat4, size: Vec2) -> Option<(f32, u64)> {
        let (o, d) = camera::ray(vp, mouse, size);
        let mut hit: Option<(f32, u64)> = None;
        for a in &self.scene.actors {
            if !a.visible {
                continue;
            }
            let (lo, hi) = a.bounds_world();
            if let Some(t) = ray_aabb(o, d, lo, hi) {
                if hit.is_none_or(|(bt, _)| t < bt) {
                    hit = Some((t, a.id));
                }
            }
        }
        hit
    }

    /// Glisser commencé sur le corps d'un personnage : il est sélectionné (le personnage entier,
    /// pas un os) et suivra le curseur sur le sol.
    fn begin_ground_drag(&mut self, mouse: Vec2, vp: &Mat4, size: Vec2) -> Option<DragKind> {
        let (t, hit) = self.actor_under(mouse, vp, size)?;
        let id = self.scene.merge_root(hit);
        let a = self.scene.actor(id)?;
        // une arme tenue en main suit son os : elle ne se pose pas au sol
        if matches!(a.attachment, Some(Attachment::Bone { .. })) {
            return None;
        }
        let root = a.root_world.w_axis.truncate();
        // plan horizontal à la hauteur du point saisi (le torse, la tête...) : le personnage reste
        // sous le curseur, alors qu'au niveau des pieds il filerait au loin
        let (o, d) = camera::ray(vp, mouse, size);
        let p = o + d * t;
        if self.sel_actor != Some(id) || self.sel_bone.is_some() {
            self.status = trf!("Déplacement de « {} » (Ctrl : aimanté à la grille)", "Moving “{}” (Ctrl: snap to grid)", a.name);
        }
        self.sel_actor = Some(id);
        self.sel_bone = None;
        Some(DragKind::MoveGround { height: p.y, offset: root - p })
    }

    /// Sélection par clic : articulation puis acteur.
    fn pick(&mut self, mouse: Vec2, vp: &Mat4, size: Vec2) {
        let mut best: Option<(f32, u64, usize)> = None;
        if self.show_skeleton {
            for a in &self.scene.actors {
                let selected = self.sel_actor == Some(a.id);
                if !a.visible || (!selected && !self.show_all_skeletons) {
                    continue;
                }
                for i in 1..a.world.len() {
                    if let Some(p) = camera::project(vp, a.world[i].w_axis.truncate(), size) {
                        let d = p.distance(mouse);
                        if d < 16.0 && best.is_none_or(|(bd, _, _)| d < bd) {
                            best = Some((d, a.id, i));
                        }
                    }
                }
            }
        }
        if let Some((_, mut id, mut b)) = best {
            // os d'une pièce fusionnée : on sélectionne l'os de même nom sur le personnage
            let root = self.scene.merge_root(id);
            if root != id {
                let name = &self.scene.actor(id).unwrap().model.skeleton.bones[b].name;
                if let Some(rb) = self.scene.actor(root).and_then(|r| r.model.skeleton.bones.iter().position(|x| &x.name == name)) {
                    id = root;
                    b = rb;
                }
            }
            self.sel_actor = Some(id);
            self.sel_bone = Some(b);
            if let Some(a) = self.scene.actor(id) {
                self.status = trf!("Os sélectionné : {}", "Selected bone: {}", a.model.skeleton.bones[b].name);
            }
            return;
        }
        let hit = self.actor_under(mouse, vp, size);
        // clic sur une pièce fusionnée (tête, casque, armure...) : on prend le personnage
        self.sel_actor = hit.map(|(_, id)| self.scene.merge_root(id));
        self.sel_bone = None;
    }

    fn viewport_ui(&mut self, ui: &mut egui::Ui) {
        let rect = ui.available_rect_before_wrap();
        let ppp = ui.ctx().pixels_per_point();
        let (pw, ph) = ((rect.width() * ppp) as u32, (rect.height() * ppp) as u32);
        let resp = ui.allocate_rect(rect, Sense::click_and_drag());
        let size = Vec2::new(rect.width(), rect.height());
        let aspect = size.x / size.y.max(1.0);
        let shot_aspect = self.shot_aspect();
        let mut view = self.current_view();
        // vue « Prise » : le cadre de l'export (format de la vidéo) tient entier dans la vue ;
        // si la vue est plus étroite que l'image, on élargit le champ pour ne rien couper
        let gate = if self.shot_view {
            if aspect < shot_aspect {
                let t = (view.fov_deg.to_radians() * 0.5).tan() * shot_aspect / aspect;
                view.fov_deg = (t.atan() * 2.0).to_degrees();
                let h = rect.width() / shot_aspect;
                Some(Rect::from_center_size(rect.center(), EVec2::new(rect.width(), h)))
            } else {
                Some(Rect::from_center_size(rect.center(), EVec2::new(rect.height() * shot_aspect, rect.height())))
            }
        } else {
            None
        };
        // moniteur de la caméra de prise, en haut à droite de la vue de travail
        let monitor = (!self.shot_view && self.show_monitor && rect.width() > 300.0).then(|| {
            let w = (rect.width() * 0.28).clamp(160.0, 420.0);
            let h = w / shot_aspect;
            Rect::from_min_size(Pos2::new(rect.max.x - w - 10.0, rect.min.y + 40.0), EVec2::new(w, h))
        });
        let vp = view.view_proj(aspect);

        // ---- entrées
        let mouse = resp
            .hover_pos()
            .or(resp.interact_pointer_pos())
            .map(|p| Vec2::new(p.x - rect.min.x, p.y - rect.min.y));
        let (alt, shift) = ui.input(|i| (i.modifiers.alt, i.modifiers.shift));
        if self.drag.is_none() {
            self.hover_axis = mouse.and_then(|m| self.pick_gizmo(m, &vp, size, &view));
        }
        // ---- vol libre : clic droit tenu = regarder autour (curseur caché), comme un drone
        let (rmb_pressed, rmb_down) = ui.input(|i| {
            (i.pointer.button_pressed(egui::PointerButton::Secondary), i.pointer.secondary_down())
        });
        if rmb_pressed && resp.hovered() && self.drag.is_none() {
            self.begin_look(ui.ctx());
        }
        if self.drag == Some(DragKind::Look) {
            if rmb_down {
                self.look_step(ui.ctx());
            } else {
                self.end_look(ui.ctx());
            }
        }

        let scroll = ui.input(|i| i.raw_scroll_delta.y);
        if (resp.hovered() || self.drag == Some(DragKind::Look)) && scroll != 0.0 {
            if self.drag == Some(DragKind::Look) || rmb_down {
                // clic droit + molette : vitesse du vol
                let d = &mut self.config.drone;
                d.speed = (d.speed * (scroll * 0.004).exp()).clamp(DroneSettings::MIN_SPEED, DroneSettings::MAX_SPEED);
                self.config.save();
                self.speed_toast = Some(Instant::now());
            } else {
                // molette : avancer / reculer dans la direction du regard
                let fast = if shift { 4.0 } else { 1.0 };
                let step = self.config.drone.speed * fast * scroll * 0.0025;
                let dir = (view.target - view.eye).normalize_or(Vec3::NEG_Z);
                if self.shot_view && self.cam_rec.is_none() {
                    // une seule étape d'annulation par série de crans
                    self.playing = false;
                    self.frame = self.frame.round();
                    self.begin_edit();
                }
                self.move_view(dir * step);
            }
        }
        if resp.drag_started() && self.drag != Some(DragKind::Look) {
            let primary = resp.dragged_by(egui::PointerButton::Primary);
            let secondary = resp.dragged_by(egui::PointerButton::Secondary);
            let middle = resp.dragged_by(egui::PointerButton::Middle);
            self.drag = if middle || (primary && shift && alt) {
                Some(DragKind::Pan)
            } else if primary && alt {
                Some(DragKind::Orbit)
            } else if secondary {
                None // la visée au clic droit est gérée à l'appui
            } else if primary {
                // poignée du gizmo d'abord, sinon le corps d'un personnage : on le fait glisser au sol
                self.hover_axis.or_else(|| mouse.and_then(|m| self.begin_ground_drag(m, &vp, size)))
            } else {
                None
            };
            if matches!(self.drag, Some(DragKind::Orbit | DragKind::Pan)) && self.shot_view {
                self.begin_shot_control();
            }
            if matches!(
                self.drag,
                Some(
                    DragKind::RotateAxis(_)
                        | DragKind::RotateFree
                        | DragKind::TranslateAxis(_)
                        | DragKind::TranslateFree
                        | DragKind::MoveGround { .. }
                )
            ) {
                self.playing = false;
                self.frame = self.frame.round();
                self.begin_edit();
            }
            self.last_mouse = mouse;
        }
        if resp.dragged() {
            let delta = resp.drag_delta();
            let dv = Vec2::new(delta.x, delta.y);
            match self.drag {
                Some(DragKind::Orbit) if self.shot_view => {
                    let mut c = self.shot_orbit();
                    c.orbit(dv);
                    self.set_shot_orbit(&c);
                }
                Some(DragKind::Pan) if self.shot_view => {
                    let mut c = self.shot_orbit();
                    c.fov_deg = view.fov_deg; // champ réellement affiché
                    c.pan(dv, size.y);
                    let fov = self.shot_ctrl().fov_deg;
                    self.set_shot(c.eye(), c.target, fov);
                }
                Some(DragKind::Orbit) => self.cam.orbit(dv),
                Some(DragKind::Pan) => self.cam.pan(dv, size.y),
                Some(DragKind::RotateAxis(ax)) => {
                    if let (Some(m), Some(prev), Some((pivot, _))) = (mouse, self.last_mouse, self.selection_frame()) {
                        if let Some(c) = camera::project(&vp, pivot, size) {
                            let a0 = (-(prev.y - c.y)).atan2(prev.x - c.x);
                            let a1 = (-(m.y - c.y)).atan2(m.x - c.x);
                            let mut da = a1 - a0;
                            if da > std::f32::consts::PI {
                                da -= std::f32::consts::TAU;
                            }
                            if da < -std::f32::consts::PI {
                                da += std::f32::consts::TAU;
                            }
                            let facing = if ax.dot(view.eye - pivot) >= 0.0 { 1.0 } else { -1.0 };
                            self.apply_world_rotation(Quat::from_axis_angle(ax, da * facing));
                            // l'axe suit l'os : on met à jour l'axe actif
                            if let Some((_, rot)) = self.selection_frame() {
                                let axes = [rot * Vec3::X, rot * Vec3::Y, rot * Vec3::Z];
                                let best = axes.into_iter().max_by(|a, b| a.dot(ax).abs().total_cmp(&b.dot(ax).abs()));
                                if let Some(b) = best {
                                    self.drag = Some(DragKind::RotateAxis(b));
                                }
                            }
                        }
                    }
                }
                Some(DragKind::RotateFree) => {
                    let q = Quat::from_axis_angle(view_up(&view), dv.x * 0.01)
                        * Quat::from_axis_angle(view_right(&view), dv.y * 0.01);
                    self.apply_world_rotation(q);
                }
                Some(DragKind::TranslateAxis(ax)) => {
                    if let Some((pivot, _)) = self.selection_frame() {
                        let r = self.gizmo_radius(pivot, &view);
                        if let (Some(c), Some(t)) =
                            (camera::project(&vp, pivot, size), camera::project(&vp, pivot + ax * r, size))
                        {
                            let s = t - c;
                            let len = s.length();
                            if len > 1.0 {
                                let amount = dv.dot(s / len) * r / len;
                                self.apply_world_translation(ax * amount);
                            }
                        }
                    }
                }
                Some(DragKind::MoveGround { height, offset }) => {
                    let snap = ui.input(|i| i.modifiers.ctrl);
                    let cur = self.selection_frame().map(|(p, _)| p);
                    if let (Some(m), Some(cur)) = (mouse, cur) {
                        if let Some(p) = ground_point(&vp, m, size, height) {
                            let mut want = p + offset;
                            if snap {
                                // aimanté aux carreaux de la grille (0,5 m), dans son orientation
                                let (rot, o) = (self.grid_rotation(), self.grid_origin);
                                let g = rot.inverse() * (want - o);
                                want = o + rot * Vec3::new((g.x * 2.0).round() * 0.5, g.y, (g.z * 2.0).round() * 0.5);
                            }
                            let d = want - cur;
                            self.apply_world_translation(Vec3::new(d.x, 0.0, d.z));
                        }
                    }
                }
                Some(DragKind::TranslateFree) => {
                    if let Some((pivot, _)) = self.selection_frame() {
                        let scale = view.eye.distance(pivot) * (view.fov_deg.to_radians() * 0.5).tan() * 2.0 / size.y;
                        let d = (view_right(&view) * dv.x - view_up(&view) * dv.y) * scale;
                        self.apply_world_translation(d);
                    }
                }
                _ => {}
            }
            self.last_mouse = mouse;
        }
        if resp.drag_stopped() && self.drag != Some(DragKind::Look) {
            self.drag = None;
            self.editing = false;
        }
        let over_monitor = |m: Option<Vec2>| {
            monitor.zip(m).is_some_and(|(r, m)| r.contains(rect.min + EVec2::new(m.x, m.y)))
        };
        if resp.clicked() {
            if over_monitor(mouse) {
                self.shot_view = true;
            } else if let Some(m) = mouse {
                self.pick(m, &vp, size);
            }
        }
        if self.drag.is_none() && over_monitor(mouse) {
            self.hover_axis = None;
        }
        if matches!(self.drag, Some(DragKind::MoveGround { .. })) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if self.drag.is_none()
            && self.hover_axis.is_none()
            && resp.hovered()
            && !alt
            && !over_monitor(mouse)
            && mouse.is_some_and(|m| self.actor_under(m, &vp, size).is_some())
        {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }

        // ---- rendu
        let scale = self.active_quality().render_scale.clamp(0.25, 2.0);
        let scaled = |v: u32| ((v as f32 * scale).round() as u32).max(8);
        let monitor_px = monitor.map(|m| (scaled((m.width() * ppp) as u32), scaled((m.height() * ppp) as u32)));
        let render_size = ((scaled(pw), scaled(ph)), monitor_px);
        let reuse = self.skip_3d && self.last_render_size == Some(render_size) && self.renderer.egui_tex.is_some();
        self.renderer.ensure_viewport(&self.rs, scaled(pw), scaled(ph));
        if !reuse {
            let lines = self.build_lines(&view);
            let items = self.draw_items(true);
            self.set_focus(&view);
            self.renderer.render_ss = scale;
            self.renderer.render(None, vp, view.eye, &items, &lines);
            self.last_render_size = Some(render_size);
        }
        if let Some(tex) = self.renderer.egui_tex {
            ui.painter().image(tex, rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        }
        // moniteur : rendu de la caméra de prise, sans gizmo ni squelette
        if let (Some(mrect), Some((mw, mh))) = (monitor, monitor_px) {
            self.renderer.ensure_monitor(&self.rs, mw, mh);
            if !reuse {
                let sv = self.shot_params(self.frame);
                let mut ml = Lines::default();
                if self.show_grid {
                    grid_lines(&mut ml, 20, self.grid_rotation(), self.grid_origin);
                }
                let items = self.draw_items(false);
                let target = self.renderer.monitor.take();
                self.set_focus(&sv);
                self.renderer.render(target.as_ref(), sv.view_proj(shot_aspect), sv.eye, &items, &ml);
                self.renderer.monitor = target;
            }
            if let Some(tex) = self.renderer.monitor_tex {
                let p = ui.painter();
                p.rect_filled(mrect.expand(3.0), 3.0, Color32::from_black_alpha(200));
                p.image(tex, mrect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                let hovered = over_monitor(mouse);
                let border = if hovered { Color32::from_rgb(160, 255, 255) } else { Color32::from_rgb(70, 190, 220) };
                p.rect_stroke(mrect.expand(1.0), 2.0, Stroke::new(1.5_f32, border), egui::StrokeKind::Outside);
                p.text(
                    mrect.left_bottom() + EVec2::new(6.0, -4.0),
                    egui::Align2::LEFT_BOTTOM,
                    if hovered { tr!("🎥 Prise — clic pour y passer (C)", "🎥 Shot — click to switch to it (C)") } else { tr!("🎥 Prise", "🎥 Shot") },
                    egui::FontId::proportional(12.0),
                    Color32::from_rgb(200, 250, 255),
                );
            }
        }

        // ---- incrustations
        let painter = ui.painter_at(rect);
        if let Some(g) = gate {
            // hors cadre : assombri ; le cadre = exactement l'image exportée
            let shade = Color32::from_black_alpha(150);
            for band in [
                Rect::from_min_max(rect.min, Pos2::new(rect.max.x, g.min.y)),
                Rect::from_min_max(Pos2::new(rect.min.x, g.max.y), rect.max),
                Rect::from_min_max(Pos2::new(rect.min.x, g.min.y), Pos2::new(g.min.x, g.max.y)),
                Rect::from_min_max(Pos2::new(g.max.x, g.min.y), Pos2::new(rect.max.x, g.max.y)),
            ] {
                if band.width() > 0.5 && band.height() > 0.5 {
                    painter.rect_filled(band, 0.0, shade);
                }
            }
            painter.rect_stroke(g, 0.0, Stroke::new(1.5_f32, Color32::from_rgb(70, 190, 220)), egui::StrokeKind::Inside);
            let keys = self.scene.camera.keys.len();
            let anim = if keys == 0 { tr!("fixe", "still").to_string() } else { trf!("animée, {keys} clé(s)", "animated, {keys} key(s)") };
            let (fw, fh) = self.shot_format();
            painter.text(
                Pos2::new(g.max.x - 8.0, g.min.y + 6.0),
                egui::Align2::RIGHT_TOP,
                trf!("🎥 CAMÉRA DE PRISE  {fw}×{fh}  {:.0}°  ({anim})", "🎥 SHOT CAMERA  {fw}×{fh}  {:.0}°  ({anim})", self.shot_params(self.frame).fov_deg),
                egui::FontId::proportional(13.0),
                Color32::from_rgb(160, 240, 255),
            );
        }
        if let (Some(aid), Some(b)) = (self.sel_actor, self.sel_bone) {
            if let Some(a) = self.scene.actor(aid) {
                if let Some(bone) = a.model.skeleton.bones.get(b) {
                    if let Some(p) = camera::project(&vp, a.world[b].w_axis.truncate(), size) {
                        painter.text(
                            rect.min + EVec2::new(p.x + 10.0, p.y - 18.0),
                            egui::Align2::LEFT_BOTTOM,
                            &bone.name,
                            egui::FontId::proportional(13.0),
                            Color32::from_rgb(255, 220, 60),
                        );
                    }
                }
            }
        }
        if self.scene.actors.is_empty() {
            let msg = if self.game.is_none() {
                tr!("Indiquez le dossier de PlanetSide 2 dans le panneau de gauche.", "Set the PlanetSide 2 folder in the left panel.")
            } else {
                tr!(
                    "Double-cliquez sur un modèle à gauche (ou « Créer le soldat ») pour l'ajouter à la scène.",
                    "Double-click a model on the left (or “Create soldier”) to add it to the scene."
                )
            };
            painter.text(rect.center(), egui::Align2::CENTER_CENTER, msg, egui::FontId::proportional(16.0), Color32::from_gray(170));
        }
        let hint_en = crate::i18n::en().then(|| self.viewport_hint_en());
        painter.text(
            rect.left_bottom() + EVec2::new(8.0, -6.0),
            egui::Align2::LEFT_BOTTOM,
            if let Some(h) = &hint_en {
                h.as_str()
            } else if self.shot_view {
                "Vous pilotez la caméra de prise : clic droit tenu = regarder   ZQSD = voler (A/E = descendre/monter)   molette = avancer   clic droit + molette = vitesse   Alt+clic = orbite   F = cadrer   K = clé caméra   C = travail   P = photo"
            } else {
                "Clic droit tenu : regarder + ZQSD voler, A/E descendre/monter, molette = vitesse   Maj : vite   Molette : avancer   Alt+clic : orbite   Clic milieu : glisser   Glisser un personnage : le déplacer (Ctrl : grille)   E/W : gizmo   F : cadrer   Espace : lecture   K : clé   C : prise"
            },
            egui::FontId::proportional(11.5),
            Color32::from_gray(140),
        );

        // vitesse du vol, affichée un instant après un changement (clic droit + molette)
        if let Some(t) = self.speed_toast {
            let age = t.elapsed().as_secs_f32();
            if age < 1.5 {
                let alpha = ((1.5 - age) / 0.4).clamp(0.0, 1.0);
                let pos = Pos2::new(rect.center().x, rect.min.y + 52.0);
                let text = format!("🚁 Vitesse : {}", speed_label(self.config.drone.speed));
                let galley = painter.layout_no_wrap(text, egui::FontId::proportional(16.0), Color32::WHITE.gamma_multiply(alpha));
                let r = Rect::from_center_size(pos, galley.size() + EVec2::new(20.0, 10.0));
                painter.rect_filled(r, 5.0, Color32::from_black_alpha((170.0 * alpha) as u8));
                painter.galley(r.center() - galley.size() * 0.5, galley, Color32::WHITE);
                ui.ctx().request_repaint();
            } else {
                self.speed_toast = None;
            }
        }

        // ---- choix de la caméra (au-dessus de la vue, ajouté en dernier pour recevoir les clics)
        let bar = Rect::from_min_size(rect.min + EVec2::new(8.0, 6.0), EVec2::new(rect.width() - 16.0, 26.0));
        let mut bar_ui = ui.new_child(egui::UiBuilder::new().max_rect(bar).layout(egui::Layout::left_to_right(egui::Align::Center)));
        egui::Frame::NONE.fill(Color32::from_black_alpha(160)).corner_radius(4.0).inner_margin(3.0).show(&mut bar_ui, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.shot_view, false, tr!("🛠 Travail", "🛠 Work"))
                    .on_hover_text(trf!(
                        "Caméra de travail : libre, pour poser et régler la scène. Jamais rendue. ({})",
                        "Work camera: free, for posing and setting up the scene. Never rendered. ({})",
                        self.config.keymap.label(Action::ToggleCamera)
                    ));
                ui.selectable_value(&mut self.shot_view, true, tr!("🎥 Prise", "🎥 Shot"))
                    .on_hover_text(trf!(
                        "Caméra de prise : celle qui fait la photo et la vidéo. En la regardant, on la pilote. ({})",
                        "Shot camera: the one that takes the photo and video. While looking through it, you drive it. ({})",
                        self.config.keymap.label(Action::ToggleCamera)
                    ));
                ui.separator();
                if !self.shot_view
                    && ui
                        .button(tr!("📷 Placer la prise ici", "📷 Put the shot here"))
                        .on_hover_text(tr!("Met la caméra de prise exactement à la place de la vue de travail", "Puts the shot camera exactly where the work view is"))
                        .clicked()
                {
                    self.shot_from_work();
                }
                self.shots_button(ui);
                if ui.button("📸 Photo").on_hover_text(trf!(
                    "Enregistre l'image courante vue par la caméra de prise ({})",
                    "Saves the current frame as seen by the shot camera ({})",
                    self.config.keymap.label(Action::Photo)
                )).clicked() {
                    self.take_photo();
                }
                if ui.button(tr!("🎬 Exporter", "🎬 Export")).on_hover_text(tr!("Fenêtre d'export : vidéo H.264 / AV1, GIF animé, images", "Export window: H.264 / AV1 video, animated GIF, images")).clicked() {
                    self.open_export(ExportTab::Video);
                }
                ui.separator();
                self.drone_menu(ui);
                ui.label(egui::RichText::new(trf!("image {}", "frame {}", self.cur_frame())).color(Color32::from_gray(200)));
            });
        });
    }

    /// Aide en bas de la vue, en anglais : les touches sont celles des raccourcis réglés (WASD sur
    /// un clavier QWERTY). Le texte français garde les touches AZERTY par défaut.
    fn viewport_hint_en(&self) -> String {
        let km = &self.config.keymap;
        let k = |a: Action| km.slot_label(a, 0).unwrap_or_else(|| "—".into());
        let wasd = [Action::FlyForward, Action::FlyLeft, Action::FlyBack, Action::FlyRight].map(k).concat();
        let (down, up) = (k(Action::FlyDownRmb), k(Action::FlyUpRmb));
        let (frame, key, cam) = (k(Action::FrameSelection), k(Action::SetKey), k(Action::ToggleCamera));
        if self.shot_view {
            format!(
                "You are driving the shot camera: right click held = look   {wasd} = fly ({down}/{up} = down/up)   \
                 wheel = forward   right click + wheel = speed   Alt+click = orbit   {frame} = frame   {key} = camera key   \
                 {cam} = work   {} = photo",
                k(Action::Photo)
            )
        } else {
            format!(
                "Right click held: look + {wasd} fly, {down}/{up} down/up, wheel = speed   Shift: fast   Wheel: forward   \
                 Alt+click: orbit   Middle click: pan   Drag a character: move it (Ctrl: grid)   {}/{}: gizmo   \
                 {frame}: frame   {}: play   {key}: key   {cam}: shot",
                k(Action::GizmoRotate),
                k(Action::GizmoTranslate),
                k(Action::PlayPause)
            )
        }
    }

    /// Réglages du vol libre : vitesse, sensibilité de la souris, inertie.
    fn drone_menu(&mut self, ui: &mut egui::Ui) {
        let before = self.config.drone;
        ui.menu_button(format!("🚁 {}", speed_label(before.speed)), |ui| {
            ui.set_min_width(260.0);
            ui.label(egui::RichText::new(tr!("Vol libre (drone)", "Free flight (drone)")).strong());
            ui.label(
                egui::RichText::new(
                    tr!(
                        "Clic droit tenu : regarder autour. ZQSD : voler, A/E : descendre/monter, Maj : vite. \
                         Clic droit + molette : vitesse.",
                        "Right click held: look around. WASD: fly, Q/E: down/up, Shift: fast. \
                         Right click + wheel: speed."
                    ),
                )
                .weak(),
            );
            ui.add_space(4.0);
            let d = &mut self.config.drone;
            egui::Grid::new("drone_settings").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label(tr!("Vitesse", "Speed"));
                ui.add(
                    egui::Slider::new(&mut d.speed, DroneSettings::MIN_SPEED..=DroneSettings::MAX_SPEED)
                        .logarithmic(true)
                        .suffix(" m/s")
                        .max_decimals(1),
                );
                ui.end_row();
                ui.label(tr!("Sensibilité souris", "Mouse sensitivity"));
                ui.add(egui::Slider::new(&mut d.sensitivity, 0.1..=4.0).logarithmic(true).max_decimals(2));
                ui.end_row();
                ui.label(tr!("Inertie", "Inertia"));
                ui.add(egui::Slider::new(&mut d.smoothing, 0.0..=0.6).suffix(" s").max_decimals(2))
                    .on_hover_text(tr!("Temps pour accélérer et freiner. 0 = arrêt net, plus haut = vol plus « drone »", "Time to speed up and brake. 0 = hard stop, higher = more “drone-like” flight"));
                ui.end_row();
                ui.label(tr!("Souris inversée", "Inverted mouse"));
                ui.checkbox(&mut d.invert_y, "vertical");
                ui.end_row();
                ui.label(tr!("Stabilisation", "Stabilization"));
                ui.add(egui::Slider::new(&mut d.stabilize, 0.0..=1.0).max_decimals(2)).on_hover_text(
                    tr!(
                        "Enregistrement en direct (⏺ REC) : adoucit les mouvements brusques de la caméra. \
                         0 = enregistre tel quel, 1 = très posé",
                        "Live recording (⏺ REC): smooths sudden camera moves. \
                         0 = records as is, 1 = very steady"
                    ),
                );
                ui.end_row();
            });
            ui.add_space(4.0);
            if ui.button(tr!("Réglages par défaut", "Default settings")).clicked() {
                *d = DroneSettings::default();
            }
        })
        .response
        .on_hover_text(tr!("Vitesse et réglages du vol libre de la caméra", "Speed and settings of the camera's free flight"));
        if self.config.drone != before {
            self.config.save();
        }
    }

    /// Progression de l'ouverture du jeu (indexation des noms au premier lancement).
    fn game_loading_ui(&self, ui: &mut egui::Ui) {
        let total = self.loader.total.load(Ordering::Relaxed).max(1);
        let p = self.loader.progress.load(Ordering::Relaxed);
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new());
            ui.label(tr!("Ouverture des archives du jeu…", "Opening the game archives…"));
        });
        if p > 0 {
            ui.label(tr!("Indexation des noms de fichiers (première fois seulement)...", "Indexing file names (first time only)..."));
            ui.add(egui::ProgressBar::new(p as f32 / total as f32).show_percentage());
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
    }

    /// Fenêtre « Dossier de PlanetSide 2 » : au premier lancement (aucun jeu chargé) ou depuis
    /// Fichier > Changer de dossier de jeu. Le dossier est choisi par l'utilisateur, jamais cherché
    /// automatiquement ; le guide explique comment installer la version 2019 du jeu.
    fn game_setup_ui(&mut self, ctx: &egui::Context) {
        let busy = self.loader.is_busy();
        // ouverture du dossier enregistré au démarrage : la progression s'affiche dans le panneau
        if !self.game_setup && (self.game.is_some() || busy) {
            return;
        }
        let can_cancel = self.game.is_some() && !busy;
        let mut open_now = false;
        let mut cancel = false;
        let resp = egui::Modal::new(egui::Id::new("game_setup")).show(ctx, |ui| {
            ui.set_width(540.0);
            ui.heading(tr!("Dossier de PlanetSide 2 (version 2019)", "PlanetSide 2 folder (2019 version)"));
            ui.add_space(6.0);
            ui.label(tr!(
                "PlanetStudio lit les modèles, textures et animations d'une installation de la \
                 version 2019 de PlanetSide 2. Il ne modifie jamais les fichiers du jeu.",
                "PlanetStudio reads the models, textures and animations of an install of the \
                 2019 version of PlanetSide 2. It never changes the game files."
            ));
            ui.add_space(4.0);
            ui.label(tr!(
                "Si tu ne l'as pas encore, suis le guide pour la télécharger depuis ton propre \
                 compte Steam, puis indique son dossier ci-dessous.",
                "If you don't have it yet, follow the guide to download it from your own \
                 Steam account, then set its folder below."
            ));
            ui.add_space(6.0);
            if ui.button(tr!("📖 Voir le guide d'installation", "📖 See the install guide")).clicked() {
                ctx.open_url(egui::OpenUrl::new_tab(tr!(GAME_GUIDE_URL, GAME_GUIDE_URL_EN)));
            }
            ui.add_space(12.0);
            ui.label(egui::RichText::new(tr!("Dossier du jeu", "Game folder")).strong());
            ui.add_enabled_ui(!busy, |ui| {
                ui.horizontal(|ui| {
                    if ui.button(tr!("📁 Parcourir…", "📁 Browse…")).clicked() {
                        let mut dlg = rfd::FileDialog::new().set_title(tr!("Dossier de PlanetSide 2 (version 2019)", "PlanetSide 2 folder (2019 version)"));
                        let cur = PathBuf::from(self.game_path.trim());
                        if cur.is_dir() {
                            dlg = dlg.set_directory(cur);
                        }
                        if let Some(dir) = dlg.pick_folder() {
                            self.game_path = dir.display().to_string();
                            open_now = true;
                        }
                    }
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut self.game_path)
                            .desired_width(f32::INFINITY)
                            .hint_text(tr!(r"ex. D:\Jeux\PlanetSide 2 (2019)", r"e.g. D:\Games\PlanetSide 2 (2019)")),
                    );
                    if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        open_now = true;
                    }
                });
            });
            if busy {
                ui.add_space(6.0);
                self.game_loading_ui(ui);
            }
            if let Some(e) = &self.game_error {
                ui.add_space(6.0);
                ui.colored_label(Color32::from_rgb(255, 120, 100), e);
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(!busy, egui::Button::new(tr!("Ouvrir", "Open"))).clicked() {
                    open_now = true;
                }
                if can_cancel && ui.button(tr!("Annuler", "Cancel")).clicked() {
                    cancel = true;
                }
            });
        });
        if open_now {
            self.game_setup = true;
            self.open_game();
        }
        if can_cancel && (cancel || resp.should_close()) {
            self.game_setup = false;
            self.game_error = None;
            self.game_path = self.config.game_path.clone().unwrap_or_default();
        }
    }

    // -------------------------------------------------------------- panneaux

    fn assets_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading(tr!("Assets du jeu", "Game assets"));
        if self.game.is_none() {
            if self.loader.is_busy() {
                self.game_loading_ui(ui);
            } else if ui.button(tr!("📁 Choisir le dossier du jeu…", "📁 Choose the game folder…")).clicked() {
                self.game_setup = true;
            }
            return;
        }

        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.left_tab, LeftTab::Models, tr!("🧍 Modèles", "🧍 Models"));
            ui.selectable_value(&mut self.left_tab, LeftTab::Anims, tr!("🏃 Animations du jeu", "🏃 Game animations"));
        });
        ui.separator();
        if self.left_tab == LeftTab::Anims {
            self.anims_ui(ui);
            return;
        }

        egui::CollapsingHeader::new(tr!("Créer un soldat", "Create a soldier")).default_open(true).show(ui, |ui| {
            egui::Grid::new("soldier").num_columns(2).show(ui, |ui| {
                ui.label("Faction");
                egui::ComboBox::from_id_salt("faction").selected_text(FACTIONS[self.soldier_faction]).show_ui(ui, |ui| {
                    for (i, f) in FACTIONS.iter().enumerate() {
                        ui.selectable_value(&mut self.soldier_faction, i, *f);
                    }
                });
                ui.end_row();
                ui.label(tr!("Sexe", "Sex"));
                let max = is_max(self.soldier_class);
                ui.add_enabled_ui(!max, |ui| {
                    let sex = soldier_word(if max { SEXES[0].1 } else { SEXES[self.soldier_sex].1 });
                    egui::ComboBox::from_id_salt("sexe").selected_text(sex).show_ui(ui, |ui| {
                        for (i, s) in SEXES.iter().enumerate() {
                            ui.selectable_value(&mut self.soldier_sex, i, soldier_word(s.1));
                        }
                    });
                });
                ui.end_row();
                ui.label(tr!("Classe", "Class"));
                egui::ComboBox::from_id_salt("classe").selected_text(soldier_word(CLASSES[self.soldier_class].1)).show_ui(ui, |ui| {
                    for (i, c) in CLASSES.iter().enumerate() {
                        ui.selectable_value(&mut self.soldier_class, i, soldier_word(c.1));
                    }
                });
                ui.end_row();
                ui.label(tr!("Visage", "Face"));
                let max = is_max(self.soldier_class);
                let human = FACTIONS[self.soldier_faction] != "NSO";
                ui.add_enabled_ui(human && !max, |ui| {
                    egui::ComboBox::from_id_salt("tete").selected_text(HEADS[self.soldier_head]).show_ui(ui, |ui| {
                        for (i, h) in HEADS.iter().enumerate() {
                            ui.selectable_value(&mut self.soldier_head, i, *h);
                        }
                    });
                });
                ui.end_row();
                if max && !human {
                    ui.label("");
                    ui.label(egui::RichText::new(tr!("MAX NSO : le Defector, sans armes (mains libres)", "NSO MAX: the Defector, without weapons (free hands)")).small().weak());
                    ui.end_row();
                } else if max {
                    ui.label("");
                    ui.label(egui::RichText::new(tr!("MAX : homme seulement, visage caché ; armes anti-infanterie aux bras", "MAX: male only, face hidden; anti-infantry weapons on the arms")).small().weak());
                    ui.end_row();
                } else if !human {
                    ui.label("");
                    ui.label(egui::RichText::new(tr!("Robot NSO : sexe et visage sans objet", "NSO robot: sex and face do not apply")).small().weak());
                    ui.end_row();
                }
                // skins : pièce d'armure posée sur la combinaison, et casque
                self.refresh_skin_lists();
                let (armors, helmets) = match &self.skin_lists {
                    Some((_, a, h)) => (a.clone(), h.clone()),
                    None => (Vec::new(), Vec::new()),
                };
                ui.label(tr!("Armure", "Armor"));
                let (armor_default, armor_none) = if max && human {
                    (tr!("Aucune (MAX de base)", "None (base MAX)"), None)
                } else if human {
                    (tr!("Aucune (tenue de base)", "None (base outfit)"), None)
                } else {
                    (tr!("Par défaut de la classe", "Class default"), Some(tr!("Aucune (corps nu)", "None (bare body)")))
                };
                let tex = self.renderer.preview_tex;
                skin_combo(
                    ui,
                    "skin_armure",
                    &mut self.soldier_armor,
                    &armors,
                    &mut self.skin_filter,
                    Some(armor_default),
                    armor_none,
                    &mut self.asset_preview,
                    tex,
                );
                ui.end_row();
                ui.label(tr!("Casque", "Helmet"));
                skin_combo(
                    ui,
                    "skin_casque",
                    &mut self.soldier_helmet,
                    &helmets,
                    &mut self.skin_filter,
                    Some(tr!("Par défaut (Look001)", "Default (Look001)")),
                    Some(tr!("Aucun casque", "No helmet")),
                    &mut self.asset_preview,
                    tex,
                );
                ui.end_row();
                ui.label("");
                ui.horizontal(|ui| {
                    if ui.button(tr!("🎲 Au hasard", "🎲 Random")).on_hover_text(tr!("Armure et casque tirés au hasard", "Random armor and helmet")).clicked() {
                        if !armors.is_empty() {
                            self.soldier_armor = SkinChoice::Named(armors[skins::random_index(armors.len())].clone());
                        }
                        if !helmets.is_empty() {
                            self.soldier_helmet = SkinChoice::Named(helmets[skins::random_index(helmets.len() + 7) % helmets.len()].clone());
                        }
                    }
                    if ui.button("↺").on_hover_text(tr!("Revenir à l'armure et au casque par défaut", "Back to the default armor and helmet")).clicked() {
                        self.soldier_armor = SkinChoice::Default;
                        self.soldier_helmet = SkinChoice::Default;
                    }
                    ui.label(
                        egui::RichText::new(trf!("{} armures, {} casques", "{} armors, {} helmets", armors.len(), helmets.len())).small().weak(),
                    );
                });
                ui.end_row();
            });
            if ui.button(tr!("➕ Créer le soldat", "➕ Create soldier")).clicked() {
                self.add_soldier();
            }
        });
        ui.separator();

        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label("🔍");
            changed |= ui
                .add(egui::TextEdit::singleline(&mut self.search).hint_text(tr!("rechercher (ex : prowler, TR helmet)", "search (e.g. prowler, TR helmet)")))
                .changed();
        });
        ui.horizontal_wrapped(|ui| {
            for c in Category::ALL {
                changed |= ui.selectable_value(&mut self.category, c, c.label()).changed();
            }
        });
        if self.category == Category::Gameplay {
            ui.horizontal_wrapped(|ui| {
                ui.label(tr!("Faction :", "Faction:"));
                for (f, label) in [(None, tr!("Toutes", "All")), (Some("TR"), "TR"), (Some("NC"), "NC"), (Some("VS"), "VS"), (Some(""), tr!("Neutres", "Neutral"))] {
                    changed |= ui.selectable_value(&mut self.faction_filter, f, label).changed();
                }
            });
        }
        changed |= ui.checkbox(&mut self.show_dme, tr!("Modèles bruts (.dme) au lieu des acteurs (.adr)", "Raw models (.dme) instead of actors (.adr)")).changed();
        if changed || self.filter_dirty {
            self.refilter();
        }
        ui.label(trf!("{} résultats — double-clic pour ajouter, clic droit pour attacher", "{} results — double-click to add, right click to attach", self.filtered.len()));
        ui.separator();

        let row_h = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
        let mut add: Option<(String, Option<PendingAttach>)> = None;
        let sel_actor = self.sel_actor;
        let sel_bone_name = self
            .sel_actor
            .and_then(|id| self.scene.actor(id))
            .and_then(|a| self.sel_bone.and_then(|b| a.model.skeleton.bones.get(b)).map(|b| b.name.clone()));
        let preview_tex = self.renderer.preview_tex;
        egui::ScrollArea::vertical().auto_shrink(false).show_rows(ui, row_h, self.filtered.len(), |ui, range| {
            for i in range {
                let name = &self.filtered[i];
                let r = ui.selectable_label(false, name);
                let r = self.asset_preview.attach(r, name, preview_tex);
                if r.double_clicked() {
                    add = Some((name.clone(), None));
                }
                r.context_menu(|ui| {
                    if ui.button(tr!("Ajouter à la scène", "Add to scene")).clicked() {
                        add = Some((name.clone(), None));
                        ui.close_menu();
                    }
                    if let Some(p) = sel_actor {
                        if ui.button(tr!("Fusionner sur l'acteur sélectionné (tête, casque, armure)", "Merge onto the selected actor (head, helmet, armor)")).clicked() {
                            add = Some((name.clone(), Some(PendingAttach::Merge(p))));
                            ui.close_menu();
                        }
                        let bone = sel_bone_name.clone().unwrap_or_else(|| "R_WEAPON".into());
                        if ui.button(trf!("Attacher à l'os {bone} de l'acteur sélectionné", "Attach to bone {bone} of the selected actor")).clicked() {
                            add = Some((name.clone(), Some(PendingAttach::Bone(p, bone))));
                            ui.close_menu();
                        }
                    }
                });
            }
        });
        if let Some((name, attach)) = add {
            let select = attach.is_none();
            self.request_model(&name, PendingAdd { attach, file: None, select, children: Vec::new(), ..Default::default() });
        }
    }

    // -------------------------------------------------------------- animations du jeu

    fn refilter_clips(&mut self) {
        let q: Vec<String> = self.clip_search.to_ascii_lowercase().split_whitespace().map(String::from).collect();
        self.clip_filtered = self
            .clips
            .iter()
            .enumerate()
            .filter(|(_, c)| self.clip_additive || !c.name.to_ascii_lowercase().contains("additive"))
            .filter(|(_, c)| self.clip_category.is_empty() || c.category == self.clip_category)
            .filter(|(_, c)| {
                let l = c.name.to_ascii_lowercase();
                q.iter().all(|w| l.contains(w.as_str()))
            })
            .map(|(i, _)| i)
            .collect();
        self.clip_dirty = false;
    }

    /// Acteur qui reçoit l'animation : la sélection, ou son parent si elle est fusionnée.
    fn anim_target(&self) -> Option<u64> {
        let a = self.scene.actor(self.sel_actor?)?;
        match a.attachment {
            Some(Attachment::BoneMerge { parent }) => Some(parent),
            _ => Some(a.id),
        }
    }

    fn decoded_clip(&mut self, r: ClipRef) -> Option<Arc<DecodedClip>> {
        let idx = match r {
            ClipRef::Fbx(i) => return self.retargeted_fbx(i),
            ClipRef::Game(i) => i,
        };
        if let Some((i, c, _)) = &self.preview {
            if *i == r {
                return Some(c.clone());
            }
        }
        let net = self.network.clone()?;
        let info = self.clips.get(idx)?.clone();
        match DecodedClip::decode(&net, &info) {
            Ok(c) => Some(Arc::new(c)),
            Err(e) => {
                self.status = trf!("Décodage impossible : {e:#}", "Could not decode: {e:#}");
                None
            }
        }
    }

    /// Animation FBX transférée sur le squelette de l'acteur cible.
    fn retargeted_fbx(&mut self, i: usize) -> Option<Arc<DecodedClip>> {
        let clip = self.fbx_clips.get(i)?;
        let Some(a) = self.anim_target().and_then(|id| self.scene.actor(id)) else {
            self.status = tr!("Sélectionnez d'abord un personnage dans la scène", "First select a character in the scene").into();
            return None;
        };
        match crate::retarget::retarget(&clip.anim, &a.model.skeleton, &clip.name) {
            Ok(c) => Some(Arc::new(c)),
            Err(e) => {
                self.status = format!("{} : {e:#}", clip.name);
                None
            }
        }
    }

    /// Nom, description et durée d'un clip.
    fn clip_meta(&self, r: ClipRef) -> Option<(String, String, f32)> {
        match r {
            ClipRef::Game(i) => self.clips.get(i).map(|c| (c.name.clone(), c.category.clone(), c.duration)),
            ClipRef::Fbx(i) => self.fbx_clips.get(i).map(|c| {
                let desc = trf!("FBX, {} images à {} i/s", "FBX, {} frames at {} fps", c.anim.frames.len(), c.anim.fps);
                (c.name.clone(), desc, c.anim.duration())
            }),
        }
    }

    /// Importe une animation FBX (squelette Mixamo) et la sélectionne.
    fn import_fbx(&mut self, path: PathBuf) {
        let parsed = std::fs::read(&path)
            .map_err(anyhow::Error::from)
            .and_then(|d| forgelight::fbx::FbxAnimation::parse(&d));
        let anim = match parsed {
            Ok(a) => Arc::new(a),
            Err(e) => {
                self.status = trf!("Import de {} impossible : {e:#}", "Could not import {}: {e:#}", path.display());
                return;
            }
        };
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "FBX".into());
        let clip = FbxClip { name: name.clone(), path: path.clone(), anim };
        let i = match self.fbx_clips.iter().position(|c| c.path == path) {
            Some(i) => {
                self.fbx_clips[i] = clip;
                i
            }
            None => {
                self.fbx_clips.push(clip);
                self.fbx_clips.len() - 1
            }
        };
        let a = &self.fbx_clips[i].anim;
        self.status = trf!("{name} importé : {:.2} s, {} os", "{name} imported: {:.2} s, {} bones", a.duration(), a.names.len());
        self.left_tab = LeftTab::Anims;
        self.clip_part = BodyPart::All;
        self.select_clip(ClipRef::Fbx(i));
    }

    /// Sélectionne un clip et lance son aperçu.
    fn select_clip(&mut self, r: ClipRef) {
        if self.clip_selected != Some(r) {
            match r {
                ClipRef::Game(idx) => {
                    let name = self.clips[idx].name.to_ascii_lowercase();
                    self.clip_mode = if name.contains("additive") { BlendMode::Additive } else { BlendMode::Replace };
                    if name.contains("partialupper") {
                        self.clip_part = BodyPart::Upper;
                    }
                }
                ClipRef::Fbx(_) => self.clip_mode = BlendMode::Replace,
            }
        }
        self.clip_selected = Some(r);
        if let Some(c) = self.decoded_clip(r) {
            self.preview = Some((r, c, Instant::now()));
        }
    }

    /// Section d'import des animations FBX (Mixamo).
    fn fbx_ui(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new(tr!("🎞 Animations FBX (Mixamo)", "🎞 FBX animations (Mixamo)")).default_open(false).show(ui, |ui| {
            ui.label(
                egui::RichText::new(
                    tr!("Export Mixamo : format FBX Binary, « Without Skin ». Glissez le fichier sur la fenêtre, \
                     ou choisissez-le ci-dessous. Le mouvement est transféré sur le squelette du personnage.", "Mixamo export: FBX Binary format, “Without Skin”. Drop the file on the window, or pick it below. The motion is transferred onto the character's skeleton."),
                )
                .small()
                .weak(),
            );
            let mut import = None;
            ui.horizontal(|ui| {
                // largeur fixe : une largeur tirée de la place disponible fait grandir le panneau à chaque image
                ui.add(egui::TextEdit::singleline(&mut self.fbx_path).hint_text("C:\\…\\Dancing.fbx").desired_width(200.0));
                if ui.add_enabled(!self.fbx_path.trim().is_empty(), egui::Button::new(tr!("Importer", "Import"))).clicked() {
                    import = Some(PathBuf::from(self.fbx_path.trim().trim_matches('"')));
                }
            });
            // fichiers trouvés dans Téléchargements et Documents/PlanetStudio
            if self.fbx_found.1.is_none_or(|t| t.elapsed().as_secs_f32() > 3.0) {
                let mut found: Vec<(std::time::SystemTime, PathBuf)> = fbx_dirs()
                    .iter()
                    .filter_map(|d| std::fs::read_dir(d).ok())
                    .flatten()
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("fbx")))
                    .map(|p| (std::fs::metadata(&p).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), p))
                    .collect();
                found.sort_by(|a, b| b.0.cmp(&a.0));
                self.fbx_found = (found.into_iter().map(|(_, p)| p).take(12).collect(), Some(Instant::now()));
            }
            let fresh: Vec<PathBuf> =
                self.fbx_found.0.iter().filter(|p| !self.fbx_clips.iter().any(|c| &c.path == *p)).cloned().collect();
            if !fresh.is_empty() {
                ui.label(egui::RichText::new(tr!("Fichiers trouvés (Téléchargements, Documents/PlanetStudio) :", "Files found (Downloads, Documents/PlanetStudio):")).small());
                ui.horizontal_wrapped(|ui| {
                    for p in fresh {
                        let name = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
                        if ui.small_button(format!("➕ {name}")).on_hover_text(p.display().to_string()).clicked() {
                            import = Some(p);
                        }
                    }
                });
            }
            let mut clicked = None;
            let mut double = None;
            let mut remove = None;
            for (i, c) in self.fbx_clips.iter().enumerate() {
                ui.horizontal(|ui| {
                    if ui.small_button("✖").on_hover_text(tr!("Retirer de la liste", "Remove from the list")).clicked() {
                        remove = Some(i);
                    }
                    let r = ui
                        .selectable_label(self.clip_selected == Some(ClipRef::Fbx(i)), format!("{}  ({:.1}s)", c.name, c.anim.duration()))
                        .on_hover_text(c.path.display().to_string());
                    if r.clicked() {
                        clicked = Some(i);
                    }
                    if r.double_clicked() {
                        double = Some(i);
                    }
                });
            }
            if let Some(p) = import {
                self.import_fbx(p);
            }
            if let Some(i) = clicked {
                self.select_clip(ClipRef::Fbx(i));
            }
            if let Some(i) = double {
                self.apply_clip(ClipRef::Fbx(i), false);
            }
            if let Some(i) = remove {
                self.fbx_clips.remove(i);
                let fix = |r: ClipRef| match r {
                    ClipRef::Fbx(j) if j == i => None,
                    ClipRef::Fbx(j) if j > i => Some(ClipRef::Fbx(j - 1)),
                    r => Some(r),
                };
                self.clip_selected = self.clip_selected.and_then(fix);
                self.preview = self.preview.take().and_then(|(r, c, t)| fix(r).map(|r| (r, c, t)));
            }
            if matches!(self.clip_selected, Some(ClipRef::Fbx(_))) {
                self.clip_panel_ui(ui);
            }
        });
    }

    fn apply_clip(&mut self, idx: ClipRef, pose_only: bool) {
        let Some(target) = self.anim_target() else {
            self.status = tr!("Sélectionnez d'abord un personnage dans la scène", "First select a character in the scene").into();
            return;
        };
        let Some(clip) = self.decoded_clip(idx) else { return };
        self.playing = false;
        self.frame = self.frame.round();
        let start = self.cur_frame();
        let (fps, speed, loops, interp) = (self.fps, self.clip_speed, self.clip_loops.max(1), self.interp);
        let preview_time = self
            .preview
            .as_ref()
            .filter(|(i, _, _)| *i == idx)
            .map(|(_, c, t0)| (t0.elapsed().as_secs_f32() * speed) % c.duration().max(0.01))
            .unwrap_or(0.0);
        // repartir de l'état réel (l'aperçu a pu modifier la pose)
        self.scene.evaluate(self.frame);
        let root_motion = self.clip_root_motion && clip.has_root_motion() && self.clip_mode == BlendMode::Replace;
        let (mode, weight, fade_in, fade_out, part) =
            (self.clip_mode, self.clip_weight, self.clip_fade_in, self.clip_fade_out, self.clip_part);
        self.begin_edit();
        let mut last = start;
        if let Some(a) = self.scene.actor_mut(target) {
            let sk = a.model.skeleton.clone();
            let blend = BlendOptions { mode, weight, mask: part.mask(&sk) };
            if pose_only {
                let mapping = clip.bone_mapping(&sk);
                let existing = a.pose.clone();
                for (b, tr) in clip.blend_onto(&sk, &mapping, preview_time, &existing, &blend, weight).into_iter().enumerate() {
                    if let Some(tr) = tr {
                        a.key_bone(b, start, tr, interp);
                    }
                }
            } else {
                let mut root = root_motion.then_some(a.root_pose);
                for l in 0..loops {
                    let opts = BakeOptions {
                        fps,
                        speed,
                        root_start: root,
                        blend: blend.clone(),
                        fade_in: if l == 0 { fade_in } else { 0 },
                        fade_out: if l + 1 == loops { fade_out } else { 0 },
                    };
                    let (e, r) = clip.bake(a, last, &opts);
                    last = e;
                    root = r;
                }
            }
        }
        self.editing = false;
        self.preview = None;
        if last > self.end {
            self.end = last;
        }
        self.scene.evaluate(self.frame);
        self.status = if pose_only {
            trf!("Pose de {} appliquée à l'image {start}", "Pose of {} applied at frame {start}", clip.name)
        } else {
            trf!(
                "{} écrit dans la timeline, images {start} à {last}{}",
                "{} written to the timeline, frames {start} to {last}{}",
                clip.name,
                if root_motion { tr!(" (avec déplacement)", " (with root motion)") } else { "" }
            )
        };
    }

    /// Aperçu en boucle du clip sélectionné sur l'acteur cible (sans écrire de clés).
    fn apply_preview(&mut self, ctx: &egui::Context) {
        if self.left_tab != LeftTab::Anims || !self.clip_preview || self.playing {
            return;
        }
        let Some((_, clip, t0)) = &self.preview else { return };
        let Some(target) = self.anim_target() else { return };
        let t = (t0.elapsed().as_secs_f32() * self.clip_speed) % clip.duration().max(0.01);
        let clip = clip.clone();
        let root_motion = self.clip_root_motion && self.clip_mode == BlendMode::Replace;
        let (mode, weight, part) = (self.clip_mode, self.clip_weight, self.clip_part);
        if let Some(a) = self.scene.actor_mut(target) {
            let sk = a.model.skeleton.clone();
            let mapping = clip.bone_mapping(&sk);
            let blend = BlendOptions { mode, weight, mask: part.mask(&sk) };
            let existing = a.pose.clone();
            for (b, tr) in clip.blend_onto(&sk, &mapping, t, &existing, &blend, weight).into_iter().enumerate() {
                if let Some(tr) = tr {
                    a.pose[b] = tr;
                }
            }
            if root_motion && clip.has_root_motion() {
                a.root_pose = clip.root_at(&a.root_pose, t);
            }
            self.scene.update_world();
            ctx.request_repaint();
        }
    }

    fn anims_ui(&mut self, ui: &mut egui::Ui) {
        // un FBX retiré de la liste ne peut plus être la danse choisie
        if self.dance_fbx.as_ref().is_some_and(|p| !self.fbx_clips.iter().any(|c| &c.path == p)) {
            self.dance_fbx = None;
        }
        egui::CollapsingHeader::new(tr!("💃 Danses de groupe", "💃 Group dances")).default_open(false).show(ui, |ui| {
            ui.label(
                egui::RichText::new(
                    tr!("La danse du cheval (générée) ou une animation FBX importée plus bas, \
                     sur un personnage, sur toute la scène ou sur une troupe.", "The horse dance (generated) or an FBX animation imported below, on one character, on the whole scene or on a troupe."),
                )
                .small()
                .weak(),
            );
            let chosen_name = self
                .dance_fbx
                .as_ref()
                .and_then(|p| self.fbx_clips.iter().find(|c| &c.path == p))
                .map(|c| format!("🎞 {}", c.name))
                .unwrap_or_else(|| tr!("🐎 Danse du cheval", "🐎 Horse dance").into());
            ui.horizontal(|ui| {
                ui.label(tr!("Danse", "Dance"));
                egui::ComboBox::from_id_salt("dance_choice").selected_text(chosen_name).width(180.0).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.dance_fbx, None, tr!("🐎 Danse du cheval", "🐎 Horse dance"));
                    for c in &self.fbx_clips {
                        ui.selectable_value(&mut self.dance_fbx, Some(c.path.clone()), format!("🎞 {}  ({:.1}s)", c.name, c.anim.duration()));
                    }
                });
            });
            ui.horizontal(|ui| {
                if self.dance_fbx.is_none() {
                    ui.add(crate::calc::drag(&mut self.dance_bpm).range(60.0..=200.0).suffix(" BPM"));
                    ui.add(crate::calc::drag(&mut self.dance_secs).range(1.0..=120.0).suffix(" s"));
                } else {
                    ui.add(crate::calc::drag(&mut self.dance_loops).range(1..=50).prefix("× ").suffix(tr!(" répétitions", " loops")));
                }
            });
            if self.fbx_clips.is_empty() {
                ui.label(egui::RichText::new(tr!("Importez un FBX Mixamo (section ci-dessous) pour le proposer ici.", "Import a Mixamo FBX (section below) to offer it here.")).small().weak());
            }
            let label = match self.anim_target().and_then(|id| self.scene.actor(id)) {
                Some(a) => trf!("💃 Faire danser {} (dès l'image {})", "💃 Make {} dance (from frame {})", a.name, self.cur_frame()),
                None => tr!("💃 Faire danser (sélectionnez un personnage)", "💃 Make dance (select a character)").into(),
            };
            if ui.add_enabled(self.anim_target().is_some(), egui::Button::new(label).wrap()).clicked() {
                if let Some(t) = self.anim_target() {
                    self.dance_actors(vec![t]);
                }
            }
            let roots: Vec<u64> = self.scene.actors.iter().filter(|a| a.attachment.is_none()).map(|a| a.id).collect();
            if ui
                .add_enabled(!roots.is_empty(), egui::Button::new(trf!("🕺 Faire danser tous les personnages ({})", "🕺 Make all characters dance ({})", roots.len())))
                .clicked()
            {
                self.dance_actors(roots);
            }
            ui.horizontal(|ui| {
                ui.add(crate::calc::drag(&mut self.troupe_size).range(2..=24).suffix(tr!(" danseurs", " dancers")));
                if ui
                    .add_enabled(self.game.is_some(), egui::Button::new(tr!("👯 Créer une troupe", "👯 Create a troupe")))
                    .on_hover_text(tr!("TR, NC, VS et NSO mélangés, en formation en V, qui dansent tous ensemble", "TR, NC, VS and NSO mixed, in a V formation, all dancing together"))
                    .clicked()
                {
                    let job = self.dance_job();
                    self.create_troupe(self.troupe_size, job);
                }
            });
        });
        self.fbx_ui(ui);
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(tr!("Réseau", "Network"));
            let mut name = self.network_name.clone();
            egui::ComboBox::from_id_salt("mrn").selected_text(&name).width(180.0).show_ui(ui, |ui| {
                for n in &self.mrn_names {
                    ui.selectable_value(&mut name, n.clone(), n);
                }
            });
            let changed = name != self.network_name;
            self.network_name = name;
            let label = if self.network.is_some() && !changed { tr!("Recharger", "Reload") } else { tr!("Charger", "Load") };
            let clicked = ui.add_enabled(!self.network_loading, egui::Button::new(label)).clicked();
            if (changed || clicked) && !self.network_loading {
                if let Some(g) = self.game.clone() {
                    self.network_loading = true;
                    self.preview = None;
                    self.status = trf!("Chargement de {}...", "Loading {}...", self.network_name);
                    self.loader.load_network(g, self.network_name.clone());
                }
            }
        });
        if self.network_loading {
            ui.add(egui::Spinner::new());
        }
        if self.network.is_none() {
            ui.label(tr!("ThirdPerson.mrn contient les animations des soldats (course, tir, emotes, morts...).", "ThirdPerson.mrn holds the soldiers' animations (running, shooting, emotes, deaths...)."));
            if !self.network_loading {
                if let Some(g) = self.game.clone() {
                    self.network_loading = true;
                    self.loader.load_network(g, self.network_name.clone());
                }
            }
            return;
        }

        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label("🔍");
            changed |= ui
                .add(egui::TextEdit::singleline(&mut self.clip_search).hint_text(tr!("ex : rifle sprint, emote, death", "e.g. rifle sprint, emote, death")))
                .changed();
        });
        ui.horizontal(|ui| {
            let mut cats: Vec<String> = self.clips.iter().map(|c| c.category.clone()).collect();
            cats.sort();
            cats.dedup();
            let cur = if self.clip_category.is_empty() { tr!("Toutes catégories", "All categories").to_string() } else { self.clip_category.clone() };
            egui::ComboBox::from_id_salt("clipcat").selected_text(cur).width(160.0).show_ui(ui, |ui| {
                changed |= ui.selectable_value(&mut self.clip_category, String::new(), tr!("Toutes catégories", "All categories")).changed();
                for c in cats {
                    changed |= ui.selectable_value(&mut self.clip_category, c.clone(), c).changed();
                }
            });
            changed |= ui
                .checkbox(&mut self.clip_additive, tr!("additifs", "additive"))
                .on_hover_text(tr!("Clips de superposition (recul, visée) : à appliquer par-dessus une autre animation", "Overlay clips (recoil, aim): to apply on top of another animation"))
                .changed();
        });
        if changed || self.clip_dirty {
            self.refilter_clips();
        }

        // ---- clip sélectionné (celui d'un FBX s'affiche sous la liste des FBX)
        if !matches!(self.clip_selected, Some(ClipRef::Fbx(_))) {
            self.clip_panel_ui(ui);
        }

        ui.label(format!("{} clips", self.clip_filtered.len()));
        let row_h = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
        let mut clicked = None;
        let mut double = None;
        egui::ScrollArea::vertical().id_salt("clips").auto_shrink(false).show_rows(ui, row_h, self.clip_filtered.len(), |ui, range| {
            for i in range {
                let idx = self.clip_filtered[i];
                let c = &self.clips[idx];
                let r = ui.selectable_label(self.clip_selected == Some(ClipRef::Game(idx)), format!("{}  ({:.1}s)", c.name, c.duration));
                if r.clicked() {
                    clicked = Some(idx);
                }
                if r.double_clicked() {
                    double = Some(idx);
                }
            }
        });
        if let Some(idx) = clicked {
            self.select_clip(ClipRef::Game(idx));
        }
        if let Some(idx) = double {
            self.apply_clip(ClipRef::Game(idx), false);
        }
    }

    /// Réglages et boutons du clip sélectionné.
    fn clip_panel_ui(&mut self, ui: &mut egui::Ui) {
        let target_name = self.anim_target().and_then(|id| self.scene.actor(id)).map(|a| a.name.clone());
        let mut action: Option<(ClipRef, bool)> = None;
        let meta = self.clip_selected.and_then(|r| self.clip_meta(r).map(|m| (r, m)));
        egui::Frame::group(ui.style()).show(ui, |ui| match meta {
            None => {
                ui.label(tr!("Cliquez sur un clip pour le prévisualiser, double-cliquez pour l'écrire dans la timeline.", "Click a clip to preview it, double-click to write it to the timeline."));
            }
            Some((sel, (name, category, duration))) => {
                ui.label(egui::RichText::new(&name).strong());
                ui.label(format!("{category}  —  {duration:.2} s"));
                ui.horizontal(|ui| {
                    ui.add(crate::calc::drag(&mut self.clip_speed).speed(0.01).range(0.05..=5.0).prefix(tr!("vitesse ×", "speed ×")));
                    ui.add(crate::calc::drag(&mut self.clip_loops).range(1..=50).prefix(tr!("répétitions ", "loops ")));
                });
                egui::CollapsingHeader::new(tr!("Mélange", "Blending")).default_open(true).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.clip_mode, BlendMode::Replace, tr!("Remplacer", "Replace"))
                            .on_hover_text(tr!("Le clip remplace l'animation existante (selon le poids et les fondus)", "The clip replaces the existing animation (based on weight and fades)"));
                        ui.selectable_value(&mut self.clip_mode, BlendMode::Additive, tr!("Superposer", "Add on top"))
                            .on_hover_text(tr!("Le clip s'ajoute par-dessus l'animation existante (clips « Additive » : recul, visée...)", "The clip is added on top of the existing animation (“Additive” clips: recoil, aim...)"));
                    });
                    ui.horizontal(|ui| {
                        ui.label(tr!("Poids", "Weight"));
                        ui.add(egui::Slider::new(&mut self.clip_weight, 0.0..=1.0).fixed_decimals(2));
                    });
                    ui.horizontal(|ui| {
                        ui.add(crate::calc::drag(&mut self.clip_fade_in).range(0..=600).prefix(tr!("fondu entrée ", "fade in ")).suffix(tr!(" im.", " fr.")));
                        ui.add(crate::calc::drag(&mut self.clip_fade_out).range(0..=600).prefix(tr!("sortie ", "out ")).suffix(tr!(" im.", " fr.")));
                    });
                    egui::ComboBox::from_id_salt("bodypart").selected_text(self.clip_part.label()).show_ui(ui, |ui| {
                        for bp in BodyPart::ALL {
                            ui.selectable_value(&mut self.clip_part, bp, bp.label());
                        }
                    });
                });
                ui.checkbox(&mut self.clip_preview, tr!("Aperçu en boucle sur l'acteur", "Looping preview on the actor"));
                // sans aperçu en cours, on ne sait pas encore : la case reste active
                let has_motion = match &self.preview {
                    Some((i, c, _)) if *i == sel => c.has_root_motion(),
                    _ => true,
                };
                ui.add_enabled_ui(has_motion, |ui| {
                    ui.checkbox(&mut self.clip_root_motion, tr!("Déplacement (le personnage avance)", "Root motion (the character moves forward)"))
                        .on_hover_text(tr!("Applique la trajectoire du clip à la position de l'acteur ; les répétitions s'enchaînent", "Applies the clip's path to the actor's position; loops follow each other"))
                        .on_disabled_hover_text(tr!("Ce clip se joue sur place", "This clip plays in place"));
                });
                match &target_name {
                    None => {
                        ui.colored_label(Color32::from_rgb(255, 190, 90), tr!("Sélectionnez un personnage dans la scène.", "Select a character in the scene."));
                    }
                    Some(t) => {
                        let idx = sel;
                        if ui
                            .button(trf!("⬇ Écrire dans la timeline de {t} (dès l'image {})", "⬇ Write to {t}'s timeline (from frame {})", self.cur_frame()))
                            .on_hover_text(tr!("Crée des clés pour chaque image, à partir de l'image courante", "Creates keys for every frame, from the current frame"))
                            .clicked()
                        {
                            action = Some((idx, false));
                        }
                        if ui.button(tr!("📌 Garder seulement la pose affichée", "📌 Keep only the displayed pose")).on_hover_text(tr!("Une seule clé, à l'image courante", "A single key, on the current frame")).clicked() {
                            action = Some((idx, true));
                        }
                    }
                }
            }
        });
        if let Some((idx, pose_only)) = action {
            self.apply_clip(idx, pose_only);
        }
    }

    fn bone_tree(&mut self, ui: &mut egui::Ui, a_id: u64, bone: usize, depth: usize) {
        let Some(a) = self.scene.actor(a_id) else { return };
        let name = a.model.skeleton.bones[bone].name.clone();
        let children: Vec<usize> = a.model.skeleton.children(bone).collect();
        let keyed = !a.anim.bones[bone].is_empty();
        let label = if keyed { format!("🔑 {name}") } else { name };
        let selected = self.sel_bone == Some(bone);
        if children.is_empty() {
            if ui.selectable_label(selected, label).clicked() {
                self.sel_bone = Some(bone);
            }
            return;
        }
        let id = ui.make_persistent_id(("bone", a_id, bone));
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, depth < 5)
            .show_header(ui, |ui| {
                if ui.selectable_label(selected, label).clicked() {
                    self.sel_bone = Some(bone);
                }
            })
            .body(|ui| {
                for c in children {
                    self.bone_tree(ui, a_id, c, depth + 1);
                }
            });
    }

    fn scene_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading(tr!("Scène", "Scene"));
        let mut select = None;
        let mut toggle_vis = None;
        let mut toggle_expand = None;
        // clic droit sur un acteur : copier-coller de pose
        let mut pose_req: Option<(u64, PoseOp)> = None;
        // clic droit > Supprimer : l'acteur et toutes ses pièces
        let mut delete_req: Option<u64> = None;
        // hauteur de la liste réglable en tirant le coin bas-droit
        let w = ui.available_width();
        egui::Resize::default()
            .id_salt("actors_resize")
            .resizable([false, true])
            .default_size([w, 160.0])
            .min_width(w)
            .max_width(w)
            .min_height(60.0)
            .max_height(2000.0)
            .show(ui, |ui| {
        egui::ScrollArea::vertical().id_salt("actors").auto_shrink([false, false]).show(ui, |ui| {
            if self.scene.actors.is_empty() {
                ui.label(egui::RichText::new(tr!("(vide)", "(empty)")).weak());
            }
            // affichage en arbre : chaque enfant sous son parent, quel que soit l'ordre de chargement
            let actors = &self.scene.actors;
            let parent_idx = |a: &crate::scene::Actor| a.attachment.as_ref().and_then(|t| self.scene.index_of(t.parent()));
            // groupes : un personnage et ses pièces ; replié par défaut, on ne voit que le personnage
            struct Row {
                i: usize,
                depth: usize,
                pieces: usize,
                open: bool,
                holds_sel: bool,
            }
            let mut rows: Vec<Row> = Vec::with_capacity(actors.len());
            let mut shown = vec![false; actors.len()];
            let expanded = &self.expanded_groups;
            let sel = self.sel_actor;
            let scene = &self.scene;
            let push_tree = |root: usize, shown: &mut Vec<bool>, rows: &mut Vec<Row>| {
                let mut stack = vec![(root, 0usize)];
                while let Some((i, depth)) = stack.pop() {
                    if shown[i] {
                        continue;
                    }
                    shown[i] = true;
                    let id = actors[i].id;
                    let desc = scene.descendants(id);
                    let open = expanded.contains(&id);
                    rows.push(Row {
                        i,
                        depth,
                        pieces: desc.len(),
                        open,
                        holds_sel: !open && sel.is_some_and(|s| desc.contains(&s)),
                    });
                    if open {
                        let kids: Vec<usize> =
                            actors.iter().enumerate().filter(|(_, a)| parent_idx(a) == Some(i)).map(|(c, _)| c).collect();
                        for c in kids.into_iter().rev() {
                            stack.push((c, depth + 1));
                        }
                    } else {
                        for d in desc {
                            if let Some(k) = scene.index_of(d) {
                                shown[k] = true;
                            }
                        }
                    }
                }
            };
            for (i, a) in actors.iter().enumerate() {
                if parent_idx(a).is_none() {
                    push_tree(i, &mut shown, &mut rows);
                }
            }
            // cycles éventuels : affichés quand même
            for i in 0..actors.len() {
                if !shown[i] {
                    push_tree(i, &mut shown, &mut rows);
                }
            }
            for r in rows {
                let a = &actors[r.i];
                ui.horizontal(|ui| {
                    let eye = if a.visible { "👁" } else { "—" };
                    let eye_tip = if r.pieces > 0 { tr!("Afficher / masquer (avec ses pièces)", "Show / hide (with its pieces)") } else { tr!("Afficher / masquer", "Show / hide") };
                    if ui.small_button(eye).on_hover_text(eye_tip).clicked() {
                        toggle_vis = Some(a.id);
                    }
                    ui.add_space(r.depth as f32 * 14.0);
                    if r.pieces > 0 {
                        let arrow = if r.open { "⏷" } else { "⏵" };
                        let tip = if r.open { tr!("Replier les pièces", "Collapse the pieces") } else { tr!("Déplier les pièces", "Expand the pieces") };
                        if ui.small_button(arrow).on_hover_text(tip).clicked() {
                            toggle_expand = Some(a.id);
                        }
                    }
                    let icon = match a.attachment {
                        Some(Attachment::BoneMerge { .. }) => "🔗 ",
                        Some(Attachment::Bone { .. }) => "📌 ",
                        None => "",
                    };
                    let count = if r.pieces > 0 && !r.open { format!("  (+{})", r.pieces) } else { String::new() };
                    let selected = self.sel_actor == Some(a.id) || r.holds_sel;
                    let resp = ui.selectable_label(selected, format!("{icon}{}{count}", a.name));
                    if resp.clicked() {
                        select = Some(a.id);
                    }
                    resp.context_menu(|ui| {
                        if let Some(op) = pose_buttons(ui, self.clipboard_pose.as_ref(), true) {
                            pose_req = Some((a.id, op));
                            ui.close_menu();
                        }
                        ui.separator();
                        let (label, tip) = if r.pieces > 0 {
                            (
                                trf!("🗑 Supprimer le personnage (+{} pièces)", "🗑 Delete the character (+{} pieces)", r.pieces),
                                tr!(
                                    "Retire de la scène ce personnage et toutes ses pièces attachées (tête, casque, armure, arme...)",
                                    "Removes this character and all its attached pieces (head, helmet, armor, weapon...) from the scene"
                                ),
                            )
                        } else {
                            (tr!("🗑 Supprimer", "🗑 Delete").to_string(), tr!("Retire cet acteur de la scène", "Removes this actor from the scene"))
                        };
                        if ui.button(label).on_hover_text(tip).clicked() {
                            delete_req = Some(a.id);
                            ui.close_menu();
                        }
                    });
                });
            }
        });
            });
        if let Some(id) = toggle_vis {
            // le personnage entraîne ses pièces (tête, casque, armure...)
            if let Some(v) = self.scene.actor(id).map(|a| !a.visible) {
                for d in std::iter::once(id).chain(self.scene.descendants(id)) {
                    if let Some(a) = self.scene.actor_mut(d) {
                        a.visible = v;
                    }
                }
            }
        }
        if let Some(id) = toggle_expand {
            if !self.expanded_groups.remove(&id) {
                self.expanded_groups.insert(id);
            }
        }
        if let Some(id) = select {
            if self.sel_actor != Some(id) {
                self.sel_bone = None;
            }
            self.sel_actor = Some(id);
        }
        if let Some((id, op)) = pose_req {
            // le clic droit vise ce personnage : il devient la sélection
            if self.sel_actor != Some(id) {
                self.sel_bone = None;
            }
            self.sel_actor = Some(id);
            self.pose_op(ui.ctx(), op);
        }
        if let Some(id) = delete_req {
            self.delete_actor_tree(id);
        }
        ui.separator();

        let Some(aid) = self.sel_actor else {
            ui.label(tr!("Aucun acteur sélectionné.", "No actor selected."));
            return;
        };
        if self.scene.actor(aid).is_none() {
            self.sel_actor = None;
            return;
        }

        // ---- acteur
        // cibles de lien, recommandées d'abord : même faction (TR/NC/VS/NSO), personnage libre, même classe
        fn faction(asset: &str) -> Option<&'static str> {
            let stem = asset.rsplit(['/', '\\']).next().unwrap_or(asset);
            stem.split(['_', '.']).find_map(|t| match t.to_ascii_uppercase().as_str() {
                "TR" => Some("TR"),
                "NC" => Some("NC"),
                "VS" => Some("VS"),
                "NSO" => Some("NSO"),
                _ => None,
            })
        }
        fn class(asset: &str) -> Option<&'static str> {
            let s = asset.to_ascii_lowercase();
            ["lightassault", "heavyassault", "medic", "engineer", "infiltrator", "max"]
                .into_iter()
                .find(|c| s.split(['_', '.', '/', '\\']).any(|t| t == *c || (*c == "medic" && t == "combatmedic")))
        }
        let (sel_faction, sel_class) =
            self.scene.actor(aid).map(|a| (faction(&a.asset), class(&a.asset))).unwrap_or((None, None));
        let mut ranked: Vec<(u32, u64, String)> = self
            .scene
            .actors
            .iter()
            .filter(|o| o.id != aid)
            .map(|o| {
                let f = faction(&o.asset);
                let mut score = 0;
                if sel_faction.is_some() && f == sel_faction {
                    score += 4;
                }
                if o.attachment.is_none() {
                    score += 2;
                }
                if sel_class.is_some() && class(&o.asset) == sel_class {
                    score += 1;
                }
                (score, o.id, o.name.clone())
            })
            .collect();
        // tri stable : à score égal on garde l'ordre de la scène
        ranked.sort_by(|a, b| b.0.cmp(&a.0));
        let best = ranked.first().map(|r| r.0).unwrap_or(0);
        let recommended = |score: u32| sel_faction.is_some() && score == best && score >= 4;
        let others: Vec<(u64, String, bool)> =
            ranked.into_iter().map(|(s, id, name)| (id, name, recommended(s))).collect();
        let mut delete = false;
        let mut root_changed: Option<Transform> = None;
        let mut new_attach: Option<Option<Attachment>> = None;
        {
            let a = self.scene.actor_mut(aid).unwrap();
            ui.horizontal(|ui| {
                ui.label(tr!("Nom", "Name"));
                ui.add(egui::TextEdit::singleline(&mut a.name).id(egui::Id::new(("actor_name", aid))));
            });
            ui.label(egui::RichText::new(&a.asset).small().weak());
            ui.label(trf!(
                "{} maillage(s), {} os, {} triangles",
                "{} mesh(es), {} bones, {} triangles",
                a.model.meshes.len(),
                a.model.skeleton.bones.len(),
                a.model.meshes.iter().map(|m| m.indices.len() / 3).sum::<usize>()
            ));

            let cur = a.attachment.clone();
            let name_of = |id: u64| others.iter().find(|o| o.0 == id).map(|o| o.1.clone()).unwrap_or_else(|| "?".into());
            let cur_text = match &cur {
                None => tr!("Libre", "Free").to_string(),
                Some(Attachment::BoneMerge { parent }) => trf!("Fusionné sur {}", "Merged onto {}", name_of(*parent)),
                Some(Attachment::Bone { parent, bone }) => trf!("Attaché à {}.{bone}", "Attached to {}.{bone}", name_of(*parent)),
            };
            ui.horizontal(|ui| {
                ui.label(tr!("Lien", "Link"));
                egui::ComboBox::from_id_salt("attach").selected_text(cur_text).width(230.0).show_ui(ui, |ui| {
                    if ui.selectable_label(cur.is_none(), tr!("Libre", "Free")).clicked() {
                        new_attach = Some(None);
                    }
                    let mut prev_rec = false;
                    for (i, (oid, oname, rec)) in others.iter().enumerate() {
                        if i > 0 && prev_rec && !rec {
                            ui.separator();
                        }
                        prev_rec = *rec;
                        let star = if *rec { "⭐ " } else { "" };
                        if ui.selectable_label(false, trf!("{star}Fusionner sur {oname}", "{star}Merge onto {oname}")).clicked() {
                            new_attach = Some(Some(Attachment::BoneMerge { parent: *oid }));
                        }
                        for bone in ["R_WEAPON", "L_WEAPON", "HEAD", "SPINEUPPER", "PELVIS"] {
                            if ui.selectable_label(false, trf!("Attacher à {oname}.{bone}", "Attach to {oname}.{bone}")).clicked() {
                                new_attach = Some(Some(Attachment::Bone { parent: *oid, bone: bone.into() }));
                            }
                        }
                    }
                });
            });

            let t = a.root_pose;
            let mut pos = t.translation;
            let mut rot = euler_deg_from_quat(t.rotation);
            let mut scale = t.scale.x;
            let mut changed = false;
            ui.label(if a.attachment.is_some() { tr!("Décalage par rapport au parent", "Offset from the parent") } else { "Position / orientation" });
            ui.horizontal(|ui| {
                ui.label("Pos");
                changed |= ui.add(crate::calc::drag(&mut pos.x).speed(0.01).prefix("x ")).changed();
                changed |= ui.add(crate::calc::drag(&mut pos.y).speed(0.01).prefix("y ")).changed();
                changed |= ui.add(crate::calc::drag(&mut pos.z).speed(0.01).prefix("z ")).changed();
            });
            ui.horizontal(|ui| {
                ui.label("Rot");
                changed |= ui.add(crate::calc::drag(&mut rot.x).speed(0.5).suffix("°")).changed();
                changed |= ui.add(crate::calc::drag(&mut rot.y).speed(0.5).suffix("°")).changed();
                changed |= ui.add(crate::calc::drag(&mut rot.z).speed(0.5).suffix("°")).changed();
            });
            ui.horizontal(|ui| {
                ui.label(tr!("Échelle", "Scale"));
                changed |= ui.add(crate::calc::drag(&mut scale).speed(0.01).range(0.01..=100.0)).changed();
                if ui.button(tr!("🗑 Supprimer l'acteur", "🗑 Delete actor")).clicked() {
                    delete = true;
                }
            });
            if changed {
                root_changed =
                    Some(Transform { translation: pos, rotation: quat_from_euler_deg(rot), scale: Vec3::splat(scale) });
            }
        }
        if let Some(att) = new_attach {
            self.begin_edit();
            if let Some(a) = self.scene.actor_mut(aid) {
                a.attachment = att;
                if a.attachment.is_some() {
                    a.anim.root_static = Transform::IDENTITY;
                }
            }
            self.editing = false;
            self.scene.evaluate(self.frame);
        }
        if let Some(t) = root_changed {
            self.begin_edit();
            let (frame, interp) = (self.cur_frame(), self.interp);
            if let Some(a) = self.scene.actor_mut(aid) {
                a.key_root(frame, t, interp, false);
            }
            self.scene.update_world();
        }
        if delete {
            self.delete_selected_actor();
            return;
        }
        ui.separator();

        // ---- skins, camouflage et décalcomanie
        if self.skins_ui(ui, aid) {
            ui.add_space(4.0);
        }
        self.camo_ui(ui, aid);
        ui.add_space(4.0);
        self.decal_ui(ui, aid);
        ui.separator();

        // ---- os
        ui.horizontal(|ui| {
            ui.heading(tr!("Os", "Bones"));
            if ui.selectable_label(self.sel_bone.is_none(), tr!("(racine de l'acteur)", "(actor root)")).clicked() {
                self.sel_bone = None;
            }
        });
        ui.add(egui::TextEdit::singleline(&mut self.bone_filter).id(egui::Id::new("bone_filter")).hint_text(tr!("filtrer les os", "filter bones")));
        egui::ScrollArea::vertical().id_salt("bones").max_height(300.0).show(ui, |ui| {
            if self.bone_filter.trim().is_empty() {
                let roots: Vec<usize> = self
                    .scene
                    .actor(aid)
                    .map(|a| {
                        a.model.skeleton.bones.iter().enumerate().filter(|(_, b)| b.parent.is_none()).map(|(i, _)| i).collect()
                    })
                    .unwrap_or_default();
                for r in roots {
                    self.bone_tree(ui, aid, r, 0);
                }
            } else {
                let f = self.bone_filter.to_ascii_lowercase();
                let names: Vec<(usize, String)> = self
                    .scene
                    .actor(aid)
                    .map(|a| {
                        a.model
                            .skeleton
                            .bones
                            .iter()
                            .enumerate()
                            .filter(|(_, b)| b.name.to_ascii_lowercase().contains(&f))
                            .map(|(i, b)| (i, b.name.clone()))
                            .collect()
                    })
                    .unwrap_or_default();
                for (i, n) in names {
                    if ui.selectable_label(self.sel_bone == Some(i), n).clicked() {
                        self.sel_bone = Some(i);
                    }
                }
            }
        });

        if let Some(b) = self.sel_bone {
            ui.separator();
            let mut new_t = None;
            if let Some(a) = self.scene.actor(aid) {
                if let Some(bone) = a.model.skeleton.bones.get(b) {
                    ui.label(egui::RichText::new(&bone.name).strong());
                    if matches!(a.attachment, Some(Attachment::BoneMerge { .. })) {
                        ui.colored_label(
                            Color32::from_rgb(255, 190, 90),
                            tr!("Acteur fusionné : animez plutôt l'acteur parent.", "Merged actor: animate the parent actor instead."),
                        );
                    }
                    let t = a.pose[b];
                    let mut rot = euler_deg_from_quat(t.rotation);
                    let mut pos = t.translation;
                    let mut changed = false;
                    ui.horizontal(|ui| {
                        ui.label("Rot");
                        changed |= ui.add(crate::calc::drag(&mut rot.x).speed(0.5).suffix("°")).changed();
                        changed |= ui.add(crate::calc::drag(&mut rot.y).speed(0.5).suffix("°")).changed();
                        changed |= ui.add(crate::calc::drag(&mut rot.z).speed(0.5).suffix("°")).changed();
                    });
                    ui.horizontal(|ui| {
                        ui.label("Pos");
                        changed |= ui.add(crate::calc::drag(&mut pos.x).speed(0.002)).changed();
                        changed |= ui.add(crate::calc::drag(&mut pos.y).speed(0.002)).changed();
                        changed |= ui.add(crate::calc::drag(&mut pos.z).speed(0.002)).changed();
                    });
                    if changed {
                        new_t = Some(Transform { translation: pos, rotation: quat_from_euler_deg(rot), scale: t.scale });
                    }
                }
            }
            if let Some(t) = new_t {
                self.begin_edit();
                let (frame, interp) = (self.cur_frame(), self.interp);
                if let Some(a) = self.scene.actor_mut(aid) {
                    a.key_bone(b, frame, t, interp);
                }
                self.scene.update_world();
            }
            ui.horizontal_wrapped(|ui| {
                if ui.button(tr!("Réinitialiser l'os", "Reset bone")).clicked() {
                    self.reset_bone(false);
                }
                if ui.button(tr!("Réinitialiser la pose", "Reset pose")).clicked() {
                    self.reset_bone(true);
                }
            });
        }
        ui.add_space(4.0);
        ui.label(egui::RichText::new(tr!("📋 Copier-coller de pose", "📋 Pose copy-paste")).strong());
        let op = ui.horizontal_wrapped(|ui| pose_buttons(ui, self.clipboard_pose.as_ref(), true)).inner;
        if let Some(c) = &self.clipboard_pose {
            ui.label(
                egui::RichText::new(trf!("Copié : « {} », image {} ({} clés d'animation)", "Copied: “{}”, frame {} ({} animation keys)", c.from, c.frame, c.key_count()))
                    .weak()
                    .small(),
            );
        }
        if let Some(op) = op {
            self.pose_op(ui.ctx(), op);
        }
    }

    fn timeline_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("⏮").on_hover_text(tr!("Début", "Start")).clicked() {
                self.playing = false;
                self.set_frame(self.start as f32);
            }
            if ui.button("◀").on_hover_text(tr!("Image précédente (←)", "Previous frame (←)")).clicked() {
                self.playing = false;
                self.set_frame((self.cur_frame() - 1).max(self.start) as f32);
            }
            let play = if self.playing { "⏸" } else { "▶" };
            if ui.button(play).on_hover_text(trf!("Lecture / pause ({})", "Play / pause ({})", self.config.keymap.label(Action::PlayPause))).clicked() {
                self.toggle_play();
            }
            if ui.button("▶|").on_hover_text(tr!("Image suivante (→)", "Next frame (→)")).clicked() {
                self.playing = false;
                self.set_frame((self.cur_frame() + 1).min(self.end) as f32);
            }
            if ui.button("⏭").on_hover_text(tr!("Fin", "End")).clicked() {
                self.playing = false;
                self.set_frame(self.end as f32);
            }
            ui.checkbox(&mut self.looping, tr!("Boucle", "Loop"));
            ui.separator();
            let mut f = self.cur_frame();
            if ui.add(crate::calc::drag(&mut f).prefix(tr!("Image ", "Frame ")).range(self.start..=self.end)).changed() {
                self.playing = false;
                self.set_frame(f as f32);
            }
            // timecode HH:MM:SS:mmm depuis Début (temps de la vidéo), fluide pendant la lecture
            let secs = ((self.frame - self.start as f32) / self.fps.max(1.0)).max(0.0) as f64;
            let ms_total = (secs * 1000.0).round() as u64;
            let tc = format!(
                "{:02}:{:02}:{:02}:{:03}",
                ms_total / 3_600_000,
                (ms_total / 60_000) % 60,
                (ms_total / 1000) % 60,
                ms_total % 1000
            );
            ui.label(egui::RichText::new(tc).monospace().strong())
                .on_hover_text(tr!("Temps écoulé depuis Début (HH:MM:SS:millisecondes)", "Time elapsed since Start (HH:MM:SS:milliseconds)"));
            ui.add(crate::calc::drag(&mut self.start).prefix(tr!("Début ", "Start ")).range(-10000..=self.end - 1));
            ui.add(crate::calc::drag(&mut self.end).prefix(tr!("Fin ", "End ")).range(self.start + 1..=100000));
            ui.add(crate::calc::drag(&mut self.fps).suffix(tr!(" ips", " fps")).range(1.0..=240.0));
            ui.separator();
            egui::ComboBox::from_id_salt("interp").selected_text(self.interp.label()).width(80.0).show_ui(ui, |ui| {
                for i in Interp::ALL {
                    ui.selectable_value(&mut self.interp, i, i.label());
                }
            });
            if ui.button(tr!("🔑 Clé", "🔑 Key")).on_hover_text(trf!("Poser une clé sur la sélection ({})", "Set a key on the selection ({})", self.config.keymap.label(Action::SetKey))).clicked() {
                self.key_selection();
            }
            if ui.button(tr!("🔑 Pose entière", "🔑 Whole pose")).on_hover_text(tr!("Clé sur tous les os de l'acteur", "Key on all of the actor's bones")).clicked() {
                self.key_all_bones();
            }
            if ui.button(tr!("✖ Suppr. clé", "✖ Del. key")).on_hover_text(tr!("Supprimer la clé à l'image courante", "Delete the key on the current frame")).clicked() {
                self.delete_key();
            }
            if ui
                .add_enabled(self.sel_actor.is_some(), egui::Button::new(tr!("🗑 Tout effacer", "🗑 Erase all")))
                .on_hover_text(tr!("Effacer toutes les clés de l'acteur sélectionné (racine et tous les os).\nL'acteur garde la pose de l'image courante. Ctrl+Z pour annuler.", "Erase all keys of the selected actor (root and all bones).\nThe actor keeps the pose of the current frame. Ctrl+Z to undo."))
                .clicked()
            {
                self.clear_actor_keys();
            }
            ui.separator();
            ui.label(tr!("🎥 Prise :", "🎥 Shot:"));
            if ui
                .button(tr!("📷 Copier la vue", "📷 Copy view"))
                .on_hover_text(tr!("Place la caméra de prise exactement comme la vue de travail\n(clé à l'image courante si elle est animée)", "Puts the shot camera exactly like the work view\n(key on the current frame if it is animated)"))
                .clicked()
            {
                self.shot_from_work();
            }
            let rec_label = if self.cam_rec.is_some() { "⏺ REC…" } else { "⏺ REC" };
            let rec_btn = egui::Button::new(egui::RichText::new(rec_label).color(if self.cam_rec_armed {
                Color32::WHITE
            } else {
                Color32::from_rgb(230, 80, 80)
            }))
            .fill(if self.cam_rec_armed { Color32::from_rgb(190, 40, 40) } else { Color32::TRANSPARENT });
            if ui
                .add(rec_btn)
                .on_hover_text(tr!(
                    "Enregistrement en direct de la caméra de prise.\n\
                     Armé, piloter la caméra (ZQSD, clic droit, Alt+glisser) lance la lecture et enregistre \
                     une clé par image ; lâcher les commandes arrête la prise.\n\
                     Les mouvements brusques sont adoucis (réglage « Stabilisation » du menu 🚁).",
                    "Live recording of the shot camera.\n\
                     When armed, driving the camera (WASD, right click, Alt+drag) starts playback and records \
                     one key per frame; letting go of the controls ends the take.\n\
                     Sudden moves are smoothed (“Stabilization” setting in the 🚁 menu)."
                ))
                .clicked()
            {
                if self.cam_rec.is_some() {
                    self.stop_recording();
                }
                self.cam_rec_armed = !self.cam_rec_armed;
                if self.cam_rec_armed {
                    self.shot_view = true;
                    self.status = tr!("Enregistrement armé : pilotez la caméra pour enregistrer.", "Recording armed: drive the camera to record.").into();
                }
            }
            if ui
                .button(tr!("🔑 Clé", "🔑 Key"))
                .on_hover_text(tr!(
                    "Pose une clé de caméra de prise à l'image courante : elle devient animée.\n\
                     En mode Travail, la clé reprend la vue de travail ; en mode Prise, la caméra de prise.",
                    "Sets a shot camera key on the current frame: it becomes animated.\n\
                     In Work mode, the key takes the work view; in Shot mode, the shot camera."
                ))
                .clicked()
            {
                self.key_camera();
            }
            let has_cam = !self.scene.camera.keys.is_empty();
            if has_cam && ui.small_button(tr!("✖ clé", "✖ key")).on_hover_text(tr!("Supprimer la clé de caméra à l'image courante", "Delete the camera key on the current frame")).clicked() {
                self.begin_edit();
                let f = self.cur_frame();
                self.scene.camera.remove_key(f);
                self.editing = false;
            }
            if has_cam
                && ui
                    .small_button(tr!("🗑 tout", "🗑 all"))
                    .on_hover_text(tr!("Effacer toutes les clés de caméra : elle reste fixe à sa position actuelle.\nCtrl+Z pour annuler.", "Erase all camera keys: it stays still at its current position.\nCtrl+Z to undo."))
                    .clicked()
            {
                self.clear_camera_keys();
            }
            let mut fov = self.shot_params(self.frame).fov_deg;
            if ui
                .add(crate::calc::drag(&mut fov).range(5.0..=120.0).speed(0.3).suffix("°"))
                .on_hover_text(tr!("Champ de vision de la caméra de prise", "Shot camera field of view"))
                .changed()
            {
                self.playing = false;
                self.frame = self.frame.round();
                self.begin_edit();
                let v = self.real_shot(self.frame);
                self.set_shot(v.eye, v.target, fov);
            }
            let (fw, fh) = self.shot_format();
            ui.menu_button(format!("▭ {fw}×{fh}"), |ui| {
                ui.set_min_width(300.0);
                ui.label(egui::RichText::new(tr!("Format de prise de vue", "Shot format")).strong());
                self.shot_format_editor(ui, false);
                ui.label(
                    egui::RichText::new(tr!("Cadre de la vue « Prise » et proportions des vidéos et des photos (enregistré dans le projet).", "Frame of the “Shot” view and proportions of videos and photos (saved in the project)."))
                        .small()
                        .weak(),
                );
            })
            .response
            .on_hover_text(tr!("Format de prise de vue de la caméra (largeur × hauteur)", "Camera shot format (width × height)"));
            if ui.button(tr!("🛠 ← prise", "🛠 ← shot")).on_hover_text(tr!("Amène la vue de travail sur la caméra de prise", "Brings the work view to the shot camera")).clicked() {
                self.work_from_shot();
            }
            ui.separator();
            ui.label(tr!("☀ Soleil :", "☀ Sun:"));
            if ui
                .button(tr!("🔑 Clé", "🔑 Key"))
                .on_hover_text(tr!("Pose une clé de soleil à l'image courante (réglages dans Affichage > Soleil)", "Sets a sun key on the current frame (settings in View > Sun)"))
                .clicked()
            {
                self.key_sun();
            }
            let cur = self.cur_frame();
            if self.scene.sun.key_at(cur).is_some()
                && ui.small_button(tr!("✖ clé", "✖ key")).on_hover_text(tr!("Supprimer la clé de soleil à l'image courante", "Delete the sun key on the current frame")).clicked()
            {
                self.delete_sun_key(cur);
            }
        });

        // ---- pistes
        // pendant la lecture zoomée, la vue suit la tête de lecture page par page (comme Premiere)
        if self.playing && self.tl_view.is_some() {
            let (v0, v1) = self.tl_view_range();
            if self.frame > v1 || self.frame < v0 {
                self.set_tl_view(self.frame, self.frame + (v1 - v0));
            }
        }
        let width = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(EVec2::new(width, 126.0), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, Color32::from_gray(28));
        let label_w = 100.0;
        let track_rect = Rect::from_min_max(rect.min + EVec2::new(label_w, 0.0), rect.max - EVec2::new(10.0, 0.0));
        // ce qui dépend du zoom est découpé à la zone des pistes (les clés hors champ ne débordent pas sur les noms)
        let tp = ui.painter_at(track_rect.expand2(EVec2::new(6.0, 0.0)));
        let (start, end) = (self.start, self.end);
        let (v0, v1) = self.tl_view_range();
        let vspan = (v1 - v0).max(1.0);
        let fx = |f: f32| track_rect.min.x + (f - v0) / vspan * track_rect.width();
        let xf = |x: f32| v0 + (x - track_rect.min.x) / track_rect.width() * vspan;

        let step = [1, 2, 5, 10, 20, 50, 100, 200, 500, 1000, 2000, 5000, 10000]
            .into_iter()
            .find(|s| track_rect.width() / vspan * *s as f32 > 40.0)
            .unwrap_or(10000);
        let mut f = (v0 / step as f32).ceil() as i32 * step;
        while f as f32 <= v1 {
            let x = fx(f as f32);
            tp.line_segment(
                [Pos2::new(x, rect.min.y), Pos2::new(x, rect.min.y + 8.0)],
                Stroke::new(1.0_f32, Color32::from_gray(110)),
            );
            tp.text(
                Pos2::new(x + 2.0, rect.min.y + 8.0),
                egui::Align2::LEFT_TOP,
                f.to_string(),
                egui::FontId::monospace(10.0),
                Color32::from_gray(150),
            );
            f += step;
        }

        // repères d'entrée et de sortie de l'export
        if let Some((a, b)) = self.export.marks {
            let (xa, xb) = (fx(a as f32), fx(b as f32));
            let col = Color32::from_rgb(255, 214, 90);
            let y = rect.min.y + 22.0;
            tp.rect_filled(Rect::from_min_max(Pos2::new(xa, y - 2.0), Pos2::new(xb, y + 2.0)), 1.0, col.gamma_multiply(0.45));
            for (x, d) in [(xa, 1.0), (xb, -1.0)] {
                let st = Stroke::new(2.0_f32, col);
                tp.line_segment([Pos2::new(x, y - 6.0), Pos2::new(x, y + 6.0)], st);
                tp.line_segment([Pos2::new(x, y - 6.0), Pos2::new(x + 4.0 * d, y - 6.0)], st);
                tp.line_segment([Pos2::new(x, y + 6.0), Pos2::new(x + 4.0 * d, y + 6.0)], st);
            }
        }

        // marqueurs : drapeau vert sur la règle, trait discret sur les pistes, nom à côté
        let mk_col = Color32::from_rgb(110, 205, 95);
        let mk_frames: Vec<i32> = self.markers.iter().map(|m| m.frame).collect();
        for m in &self.markers {
            let x = fx(m.frame as f32);
            let (top, y) = (rect.min.y + 15.0, rect.min.y + 22.0);
            tp.line_segment(
                [Pos2::new(x, y + 6.0), Pos2::new(x, rect.max.y)],
                Stroke::new(1.0_f32, mk_col.gamma_multiply(0.35)),
            );
            tp.add(egui::Shape::convex_polygon(
                vec![
                    Pos2::new(x - 4.0, top),
                    Pos2::new(x + 4.0, top),
                    Pos2::new(x + 4.0, y + 2.0),
                    Pos2::new(x, y + 6.0),
                    Pos2::new(x - 4.0, y + 2.0),
                ],
                mk_col,
                Stroke::new(1.0_f32, Color32::BLACK),
            ));
            if !m.name.is_empty() {
                tp.text(
                    Pos2::new(x + 6.0, y),
                    egui::Align2::LEFT_CENTER,
                    truncate(&m.name, 24),
                    egui::FontId::proportional(10.0),
                    mk_col,
                );
            }
        }
        // repère de la règle sous la souris : marqueur, sinon entrée ou sortie
        let marks = self.export.marks;
        let near_mark = |pos: Pos2| -> Option<markers::MarkDrag> {
            if !(rect.min.y + 12.0..=rect.min.y + 30.0).contains(&pos.y) {
                return None;
            }
            let best = mk_frames
                .iter()
                .enumerate()
                .map(|(i, &f)| (i, (fx(f as f32) - pos.x).abs()))
                .filter(|&(_, d)| d < 6.0)
                .min_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((i, _)) = best {
                return Some(markers::MarkDrag::Marker(i));
            }
            let (a, b) = marks?;
            if (fx(b as f32) - pos.x).abs() < 6.0 {
                Some(markers::MarkDrag::Out)
            } else if (fx(a as f32) - pos.x).abs() < 6.0 {
                Some(markers::MarkDrag::In)
            } else {
                None
            }
        };
        if self.tl_mark_drag.is_some() || resp.hover_pos().and_then(near_mark).is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }

        let rows_y = [rect.min.y + 38.0, rect.min.y + 62.0, rect.min.y + 86.0, rect.min.y + 110.0];
        let sel_label = match (self.sel_actor.and_then(|id| self.scene.actor(id)), self.sel_bone) {
            (Some(a), Some(b)) => a.model.skeleton.bones.get(b).map(|b| b.name.clone()).unwrap_or_default(),
            (Some(a), None) => a.name.clone(),
            _ => "—".into(),
        };
        painter.text(
            Pos2::new(rect.min.x + 6.0, rows_y[0]),
            egui::Align2::LEFT_CENTER,
            truncate(&sel_label, 13),
            egui::FontId::proportional(12.0),
            Color32::from_gray(200),
        );
        painter.text(
            Pos2::new(rect.min.x + 6.0, rows_y[1]),
            egui::Align2::LEFT_CENTER,
            tr!("Caméra", "Camera"),
            egui::FontId::proportional(12.0),
            Color32::from_gray(200),
        );
        painter.text(
            Pos2::new(rect.min.x + 6.0, rows_y[2]),
            egui::Align2::LEFT_CENTER,
            tr!("☀ Soleil", "☀ Sun"),
            egui::FontId::proportional(12.0),
            Color32::from_gray(200),
        );
        painter.text(
            Pos2::new(rect.min.x + 6.0, rows_y[3]),
            egui::Align2::LEFT_CENTER,
            tr!("🌄 Ambiance", "🌄 Ambiance"),
            egui::FontId::proportional(12.0),
            Color32::from_gray(200),
        );
        for y in rows_y {
            painter.line_segment(
                [Pos2::new(track_rect.min.x, y), Pos2::new(track_rect.max.x, y)],
                Stroke::new(1.0_f32, Color32::from_gray(45)),
            );
        }
        let sel_keys = self.selection_key_frames();
        let cam_keys: Vec<i32> = self.scene.camera.keys.iter().map(|k| k.frame).collect();
        let sun_keys: Vec<i32> = self.scene.sun.keys.iter().map(|k| k.frame).collect();
        let amb_keys: Vec<i32> = self.scene.ambiance.keys.iter().map(|k| k.frame).collect();
        let diamond = |p: &egui::Painter, x: f32, y: f32, c: Color32| {
            let s = 5.0;
            p.add(egui::Shape::convex_polygon(
                vec![Pos2::new(x, y - s), Pos2::new(x + s, y), Pos2::new(x, y + s), Pos2::new(x - s, y)],
                c,
                Stroke::new(1.0_f32, Color32::BLACK),
            ));
        };
        let cur = self.cur_frame();
        for &k in &sel_keys {
            let c = if k == cur { Color32::from_rgb(255, 230, 80) } else { Color32::from_rgb(230, 150, 40) };
            diamond(&tp, fx(k as f32), rows_y[0], c);
        }
        for &k in &cam_keys {
            let c = if k == cur { Color32::from_rgb(160, 255, 255) } else { Color32::from_rgb(70, 190, 220) };
            diamond(&tp, fx(k as f32), rows_y[1], c);
        }
        for &k in &sun_keys {
            let c = if k == cur { Color32::from_rgb(255, 245, 170) } else { Color32::from_rgb(240, 180, 50) };
            diamond(&tp, fx(k as f32), rows_y[2], c);
        }
        for &k in &amb_keys {
            let c = if k == cur { Color32::from_rgb(220, 200, 255) } else { Color32::from_rgb(150, 120, 230) };
            diamond(&tp, fx(k as f32), rows_y[3], c);
        }
        let px = fx(self.frame);
        tp.line_segment(
            [Pos2::new(px, rect.min.y), Pos2::new(px, rect.max.y)],
            Stroke::new(2.0_f32, Color32::from_rgb(230, 60, 60)),
        );

        // clic/glisser = déplacer la tête ; glisser une clé = la déplacer ; clic droit sur une clé = la supprimer
        let near_key = |pos: Pos2| -> Option<i32> {
            if (pos.y - rows_y[0]).abs() < 9.0 {
                sel_keys.iter().copied().find(|&k| (fx(k as f32) - pos.x).abs() < 6.0)
            } else {
                None
            }
        };
        let near_sun_key = |pos: Pos2| -> Option<i32> {
            if (pos.y - rows_y[2]).abs() < 9.0 {
                sun_keys.iter().copied().find(|&k| (fx(k as f32) - pos.x).abs() < 6.0)
            } else {
                None
            }
        };
        let near_amb_key = |pos: Pos2| -> Option<i32> {
            if (pos.y - rows_y[3]).abs() < 9.0 {
                amb_keys.iter().copied().find(|&k| (fx(k as f32) - pos.x).abs() < 6.0)
            } else {
                None
            }
        };
        if resp.drag_started() {
            self.tl_mark_drag = resp.interact_pointer_pos().and_then(near_mark);
        }
        if resp.drag_started() && self.tl_mark_drag.is_none() {
            if let Some(pos) = resp.interact_pointer_pos() {
                self.key_drag = near_key(pos).map(|k| (k, k));
                self.sun_drag = if self.key_drag.is_none() { near_sun_key(pos).map(|k| (k, k)) } else { None };
                self.amb_drag = if self.key_drag.is_none() && self.sun_drag.is_none() {
                    near_amb_key(pos).map(|k| (k, k))
                } else {
                    None
                };
                if self.key_drag.is_some() || self.sun_drag.is_some() || self.amb_drag.is_some() {
                    self.begin_edit();
                }
            }
        }
        if let Some(pos) = resp.interact_pointer_pos() {
            if resp.dragged_by(egui::PointerButton::Primary) || resp.clicked() {
                let f = xf(pos.x).round().clamp(start as f32, end as f32);
                let fi = f as i32;
                let on_mark = if resp.clicked() { near_mark(pos) } else { None };
                if let Some(d) = self.tl_mark_drag {
                    match d {
                        markers::MarkDrag::In => {
                            if let Some(m) = &mut self.export.marks {
                                m.0 = fi.min(m.1);
                            }
                        }
                        markers::MarkDrag::Out => {
                            if let Some(m) = &mut self.export.marks {
                                m.1 = fi.max(m.0);
                            }
                        }
                        markers::MarkDrag::Marker(i) => {
                            if let Some(m) = self.markers.get_mut(i) {
                                m.frame = fi;
                            }
                        }
                    }
                } else if let Some(mf) = on_mark.map(|d| match d {
                    // clic sur un repère : la tête s'y place exactement
                    markers::MarkDrag::Marker(i) => mk_frames[i],
                    markers::MarkDrag::In => marks.map_or(fi, |m| m.0),
                    markers::MarkDrag::Out => marks.map_or(fi, |m| m.1),
                }) {
                    self.playing = false;
                    self.set_frame(mf as f32);
                } else if let Some((orig, last)) = self.key_drag {
                    if fi != last {
                        self.move_selection_keys(last, fi);
                        self.key_drag = Some((orig, fi));
                        self.set_frame(f);
                    }
                } else if let Some((orig, last)) = self.sun_drag {
                    if fi != last && self.scene.sun.move_key(last, fi) {
                        self.sun_drag = Some((orig, fi));
                        self.set_frame(f);
                    }
                } else if let Some((orig, last)) = self.amb_drag {
                    if fi != last && self.scene.ambiance.move_key(last, fi) {
                        self.amb_drag = Some((orig, fi));
                        self.set_frame(f);
                    }
                } else {
                    self.playing = false;
                    self.set_frame(f);
                }
            }
        }
        if resp.drag_stopped() {
            if self.tl_mark_drag.take().is_some() {
                self.tidy_markers();
            }
            self.key_drag = None;
            self.sun_drag = None;
            self.amb_drag = None;
            self.editing = false;
        }
        let mut on_key = false;
        if resp.secondary_clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                on_key = near_sun_key(pos).is_some() || near_amb_key(pos).is_some() || near_key(pos).is_some();
                self.tl_menu_marker = match near_mark(pos) {
                    Some(markers::MarkDrag::Marker(i)) => Some(i),
                    _ => None,
                };
                if let Some(k) = near_sun_key(pos) {
                    self.delete_sun_key(k);
                } else if let Some(k) = near_amb_key(pos) {
                    self.delete_amb_key(k);
                } else if let Some(k) = near_key(pos) {
                    let old = self.frame;
                    self.frame = k as f32;
                    self.delete_key();
                    self.set_frame(old);
                }
            }
        }
        // clic droit ailleurs que sur une clé : menu des repères
        if !on_key {
            resp.context_menu(|ui| self.timeline_menu(ui));
        }

        // ---- barre de zoom (comme Premiere) : glisser les ronds = zoomer, glisser le milieu = défiler
        let (bar, bresp) = ui.allocate_exact_size(EVec2::new(width, 14.0), Sense::click_and_drag());
        let groove = Rect::from_min_max(Pos2::new(track_rect.min.x, bar.min.y), Pos2::new(track_rect.max.x, bar.max.y));
        let full = (end - start).max(1) as f32;
        let bx = |f: f32| groove.min.x + (f - start as f32) / full * groove.width();
        let bf = |x: f32| start as f32 + (x - groove.min.x) / groove.width() * full;
        let r = groove.height() / 2.0;
        // le curseur garde une largeur minimale pour que ses deux ronds restent attrapables
        let (mut x0, mut x1) = (bx(v0), bx(v1));
        let min_w = 4.0 * r + 6.0;
        if x1 - x0 < min_w {
            let c = ((x0 + x1) / 2.0).clamp(groove.min.x + min_w / 2.0, groove.max.x - min_w / 2.0);
            (x0, x1) = (c - min_w / 2.0, c + min_w / 2.0);
        }
        let thumb = Rect::from_min_max(Pos2::new(x0, groove.min.y), Pos2::new(x1, groove.max.y));
        let (hl, hr) = (Pos2::new(x0 + r, groove.center().y), Pos2::new(x1 - r, groove.center().y));
        let hit = |p: Pos2| -> Option<TlBarDrag> {
            let (dl, dr) = ((p.x - hl.x).abs(), (p.x - hr.x).abs());
            if dl <= r + 2.0 && dl <= dr {
                Some(TlBarDrag::Left)
            } else if dr <= r + 2.0 {
                Some(TlBarDrag::Right)
            } else if thumb.x_range().contains(p.x) {
                Some(TlBarDrag::Pan)
            } else {
                None
            }
        };
        let hover_part = bresp.hover_pos().and_then(hit);
        let active = self.tl_bar_drag.map(|(m, _)| m).or(hover_part);
        match active {
            Some(TlBarDrag::Left | TlBarDrag::Right) => ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal),
            Some(TlBarDrag::Pan) if self.tl_bar_drag.is_some() => ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing),
            Some(TlBarDrag::Pan) => ui.ctx().set_cursor_icon(egui::CursorIcon::Grab),
            None => {}
        }

        let bp = ui.painter_at(bar);
        bp.rect_filled(groove, r, Color32::from_gray(38));
        let thumb_fill = if active.is_some() { Color32::from_gray(88) } else { Color32::from_gray(68) };
        bp.rect_filled(thumb, r, thumb_fill);
        bp.line_segment(
            [Pos2::new(bx(self.frame), groove.min.y + 2.0), Pos2::new(bx(self.frame), groove.max.y - 2.0)],
            Stroke::new(1.5_f32, Color32::from_rgb(230, 60, 60)),
        );
        for (h, part) in [(hl, TlBarDrag::Left), (hr, TlBarDrag::Right)] {
            let lit = active == Some(part);
            bp.circle(
                h,
                r - 2.0,
                Color32::from_gray(if lit { 70 } else { 45 }),
                Stroke::new(1.5_f32, Color32::from_gray(if lit { 245 } else { 200 })),
            );
        }

        if bresp.drag_started() {
            if let Some(pos) = bresp.interact_pointer_pos() {
                let f = bf(pos.x);
                self.tl_bar_drag = Some(match hit(pos) {
                    Some(TlBarDrag::Left) => (TlBarDrag::Left, f - v0),
                    Some(TlBarDrag::Right) => (TlBarDrag::Right, f - v1),
                    Some(TlBarDrag::Pan) => (TlBarDrag::Pan, f - v0),
                    // à côté du curseur : on le centre sous la souris, puis on le fait glisser
                    None => (TlBarDrag::Pan, (v1 - v0) / 2.0),
                });
            }
        }
        if let (Some((mode, grab)), Some(pos)) = (self.tl_bar_drag, bresp.interact_pointer_pos()) {
            if bresp.dragged_by(egui::PointerButton::Primary) {
                let f = bf(pos.x) - grab;
                match mode {
                    TlBarDrag::Left => self.set_tl_view(f.min(v1 - TL_MIN_SPAN), v1),
                    TlBarDrag::Right => self.set_tl_view(v0, f.max(v0 + TL_MIN_SPAN)),
                    TlBarDrag::Pan => self.set_tl_view(f, f + (v1 - v0)),
                }
            }
        }
        if bresp.drag_stopped() {
            self.tl_bar_drag = None;
        }
        if bresp.double_clicked() {
            self.tl_view = None;
        } else if bresp.clicked() {
            if let Some(pos) = bresp.interact_pointer_pos().filter(|p| hit(*p).is_none()) {
                let f = bf(pos.x);
                self.set_tl_view(f - (v1 - v0) / 2.0, f + (v1 - v0) / 2.0);
            }
        }
        bresp.on_hover_text(
            tr!("Zoom de la timeline : glisser un rond pour zoomer, le milieu pour défiler.\n\
             Double-clic : tout afficher. Sur les pistes : Ctrl/Alt + molette = zoom, molette = défiler.", "Timeline zoom: drag a dot to zoom, the middle to scroll.\nDouble-click: show all. On the tracks: Ctrl/Alt + wheel = zoom, wheel = scroll."),
        );

        // molette sur les pistes ou la barre : Ctrl/Alt = zoom autour de la souris, sinon défilement
        if let Some(pos) = ui.input(|i| i.pointer.hover_pos()).filter(|p| rect.union(bar).contains(*p)) {
            let (zoom, scroll, alt) = ui.input(|i| (i.zoom_delta(), i.raw_scroll_delta, i.modifiers.alt));
            let wheel = scroll.x + scroll.y;
            let anchor = if track_rect.x_range().contains(pos.x) { xf(pos.x) } else { (v0 + v1) / 2.0 };
            if zoom != 1.0 {
                self.zoom_tl_view(anchor, zoom);
            } else if alt && wheel != 0.0 {
                self.zoom_tl_view(anchor, (wheel * 0.004).exp());
            } else if wheel != 0.0 && self.tl_view.is_some() {
                let d = -wheel * 2.0 / track_rect.width() * vspan;
                self.set_tl_view(v0 + d, v1 + d);
            }
        }
    }

    /// Plage d'images visible dans la timeline (zoom), bornée à [Début, Fin].
    fn tl_view_range(&self) -> (f32, f32) {
        let (s, e) = (self.start as f32, self.end as f32);
        match self.tl_view {
            Some((a, b)) => {
                let (a, b) = (a.clamp(s, e), b.clamp(s, e));
                if b - a < TL_MIN_SPAN.min(e - s) {
                    (s, e)
                } else {
                    (a, b)
                }
            }
            None => (s, e),
        }
    }

    /// Règle la plage visible de la timeline ; revient à « tout » si elle couvre Début..Fin.
    fn set_tl_view(&mut self, a: f32, b: f32) {
        let (s, e) = (self.start as f32, self.end as f32);
        let full = (e - s).max(1.0);
        let span = (b - a).clamp(TL_MIN_SPAN.min(full), full);
        let a = a.clamp(s, e - span);
        self.tl_view = if span >= full - 1e-3 { None } else { Some((a, a + span)) };
    }

    /// Zoom de la timeline autour de l'image `anchor` (facteur > 1 = rapprocher).
    fn zoom_tl_view(&mut self, anchor: f32, factor: f32) {
        let (v0, v1) = self.tl_view_range();
        let span = (v1 - v0) / factor;
        let a = anchor - (anchor - v0) / factor;
        self.set_tl_view(a, a + span);
    }

    /// Images où la sélection a des clés.
    fn selection_key_frames(&self) -> Vec<i32> {
        let Some(a) = self.sel_actor.and_then(|id| self.scene.actor(id)) else { return Vec::new() };
        let mut set: HashSet<i32> = HashSet::new();
        match self.sel_bone {
            Some(b) => set.extend(a.anim.bones[b].keys.iter().map(|k| k.frame)),
            None => {
                set.extend(a.anim.root.keys.iter().map(|k| k.frame));
                for t in &a.anim.bones {
                    set.extend(t.keys.iter().map(|k| k.frame));
                }
            }
        }
        let mut v: Vec<i32> = set.into_iter().collect();
        v.sort();
        v
    }

    fn move_selection_keys(&mut self, from: i32, to: i32) {
        let sel_bone = self.sel_bone;
        let Some(a) = self.sel_actor.and_then(|id| self.scene.actor_mut(id)) else { return };
        let tracks: Vec<&mut crate::anim::Track> = match sel_bone {
            Some(b) => vec![&mut a.anim.bones[b]],
            None => std::iter::once(&mut a.anim.root).chain(a.anim.bones.iter_mut()).collect(),
        };
        for t in tracks {
            if t.key_at(to).is_some() {
                continue; // n'écrase pas une clé existante
            }
            if let Some(k) = t.key_at(from).copied() {
                t.remove_key(from);
                t.set_key(crate::anim::Key { frame: to, ..k });
            }
        }
    }

    fn toggle_play(&mut self) {
        self.playing = !self.playing;
        if self.playing && self.cur_frame() >= self.end {
            self.frame = self.start as f32;
        }
        if !self.playing {
            self.set_frame(self.frame.round());
        }
        self.last_tick = Instant::now();
    }


    fn file_dialog_ui(&mut self, ctx: &egui::Context) {
        let Some(kind) = &self.dialog else { return };
        let saving = *kind == Dialog::Save;
        let title = if saving { tr!("Enregistrer le projet", "Save project") } else { tr!("Ouvrir un projet", "Open project") };
        let mut close = false;
        let mut action = false;
        egui::Window::new(title).id(egui::Id::new("project_dialog")).collapsible(false).resizable(false).show(ctx, |ui| {
            ui.label(tr!("Chemin du fichier projet (.psproj) :", "Project file path (.psproj):"));
            ui.add(egui::TextEdit::singleline(&mut self.project_path).desired_width(480.0));
            if let Ok(rd) = std::fs::read_dir(projects_dir()) {
                let files: Vec<PathBuf> = rd
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.extension().is_some_and(|e| e == "psproj"))
                    .collect();
                if !files.is_empty() {
                    ui.label(tr!("Projets dans Documents/PlanetStudio :", "Projects in Documents/PlanetStudio:"));
                    for f in files {
                        if ui.selectable_label(false, f.file_name().unwrap_or_default().to_string_lossy()).clicked() {
                            self.project_path = f.display().to_string();
                        }
                    }
                }
            }
            ui.horizontal(|ui| {
                if ui.button(if saving { tr!("Enregistrer", "Save") } else { tr!("Ouvrir", "Open") }).clicked() {
                    action = true;
                }
                if ui.button(tr!("Annuler", "Cancel")).clicked() {
                    close = true;
                }
            });
        });
        if action {
            if saving {
                self.save_project();
            } else {
                self.open_project();
            }
            close = true;
        }
        if close {
            self.dialog = None;
        }
    }

    /// Vol au clavier dans le décor, comme un drone ou la caméra d'observateur d'un jeu. Les
    /// touches (réglables, voir `keymap`) sont lues à leur position physique : par défaut Z/Q/S/D
    /// en AZERTY (W/A/S/D en QWERTY) avancent dans la direction du regard, reculent et décalent ;
    /// A/E (Q/E en QWERTY) descendent et montent en tenant le clic droit, Pg.Préc/Pg.Suiv à tout
    /// moment. Maj = plus vite. La vitesse est lissée (accélération et freinage en douceur).
    /// Pilote la caméra de prise en vue « Prise ».
    fn fly(&mut self, ctx: &egui::Context, dt: f32) {
        let typing = ctx.wants_keyboard_input() || self.help.capturing();
        let (focused, cmd, shift, rmb) = ctx.input(|i| {
            for ev in &i.events {
                if let egui::Event::Key { key, physical_key, pressed, .. } = ev {
                    let k = physical_key.unwrap_or(*key);
                    if *pressed && !typing {
                        self.fly_keys.insert(k);
                    } else if !*pressed {
                        self.fly_keys.remove(&k);
                    }
                }
            }
            (i.focused, i.modifiers.command, i.modifiers.shift, i.pointer.secondary_down())
        });
        if typing || !focused {
            self.fly_keys.clear();
            self.fly_vel = Vec3::ZERO;
        }
        if !focused && self.look_anchor.is_some() {
            self.end_look(ctx);
        }
        let km = &self.config.keymap;
        let held = |a: Action| km.held(&self.fly_keys, a);
        let axis = |pos: bool, neg: bool| (pos as i32 - neg as i32) as f32;
        let fwd = axis(held(Action::FlyForward), held(Action::FlyBack));
        let side = axis(held(Action::FlyRight), held(Action::FlyLeft));
        // A/E servent aussi de raccourcis (E = rotation) : la montée/descente par lettres
        // demande le clic droit, comme dans les éditeurs de jeu
        let vert = axis(
            held(Action::FlyUp) || (rmb && held(Action::FlyUpRmb)),
            held(Action::FlyDown) || (rmb && held(Action::FlyDownRmb)),
        );
        let view = self.current_view();
        let dir = (view.target - view.eye).normalize_or(Vec3::NEG_Z);
        let right = view_right(&view);
        // Ctrl tenu : raccourci (Ctrl+S…), pas un déplacement
        let wish = if cmd { Vec3::ZERO } else { dir * fwd + right * side + Vec3::Y * vert };
        let wish = if wish.length_squared() > 1.0 { wish.normalize() } else { wish };
        let speed = self.config.drone.speed * if shift { 4.0 } else { 1.0 };
        let smooth = self.config.drone.smoothing;
        let a = if smooth <= 0.005 { 1.0 } else { 1.0 - (-dt / smooth).exp() };
        self.fly_vel += (wish * speed - self.fly_vel) * a;
        if wish == Vec3::ZERO && self.fly_vel.length() < 0.02 * self.config.drone.speed.max(0.1) {
            self.fly_vel = Vec3::ZERO;
        }
        if self.fly_vel == Vec3::ZERO {
            if self.fly_moving && self.drag.is_none() {
                self.editing = false; // fin de la série : une seule étape d'annulation
            }
            self.fly_moving = false;
            return;
        }
        if self.shot_view && !self.fly_moving {
            self.begin_shot_control();
        }
        self.move_view(self.fly_vel * dt);
        self.fly_moving = true;
        ctx.request_repaint();
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        // file d'attente en cours : le projet affiché est celui de l'export, on n'y touche pas
        if ctx.wants_keyboard_input() || self.help.capturing() || self.export.queue_running() {
            return;
        }
        // clic droit tenu = mode vol : les lettres servent au déplacement, pas aux raccourcis
        if ctx.input(|i| i.pointer.secondary_down()) && !self.fly_keys.is_empty() {
            return;
        }
        let km = &self.config.keymap;
        let (space, left, right, undo, redo, save, k, e, w, f, g, del, c, p) = ctx.input(|i| {
            let on = |a: Action| km.pressed(i, a);
            (
                on(Action::PlayPause),
                on(Action::PrevFrame),
                on(Action::NextFrame),
                on(Action::Undo),
                on(Action::Redo),
                on(Action::Save),
                on(Action::SetKey),
                on(Action::GizmoRotate),
                on(Action::GizmoTranslate),
                on(Action::FrameSelection),
                on(Action::GridToSelection),
                on(Action::DeleteActor),
                on(Action::ToggleCamera),
                on(Action::Photo),
            )
        });
        if space {
            self.toggle_play();
        }
        if left {
            self.playing = false;
            self.set_frame((self.cur_frame() - 1).max(self.start) as f32);
        }
        if right {
            self.playing = false;
            self.set_frame((self.cur_frame() + 1).min(self.end) as f32);
        }
        if undo {
            self.undo_redo(false);
        }
        if redo {
            self.undo_redo(true);
        }
        if save {
            self.dialog = Some(Dialog::Save);
        }
        if k {
            if self.sel_actor.is_none() {
                self.key_camera();
            } else {
                self.key_selection();
            }
        }
        if c {
            self.shot_view = !self.shot_view;
        }
        if p {
            self.take_photo();
        }
        if e {
            self.gizmo = GizmoMode::Rotate;
        }
        if w {
            self.gizmo = GizmoMode::Translate;
        }
        if f {
            self.frame_selection();
        }
        if g {
            self.place_grid_at_selection();
        }
        if del && self.sel_bone.is_none() && self.sel_actor.is_some() {
            self.delete_selected_actor();
        }
        let km = &self.config.keymap;
        let (mark_in, mark_out, go_in, go_out, clear_io, add_mk) = ctx.input(|i| {
            let on = |a: Action| km.pressed(i, a);
            (
                on(Action::MarkIn),
                on(Action::MarkOut),
                on(Action::GoToIn),
                on(Action::GoToOut),
                on(Action::ClearInOut),
                on(Action::AddMarker),
            )
        });
        let cur = self.cur_frame();
        if mark_in {
            self.mark_in(cur);
        }
        if mark_out {
            self.mark_out(cur);
        }
        if go_in {
            self.go_to_in();
        }
        if go_out {
            self.go_to_out();
        }
        if clear_io {
            self.clear_in_out();
        }
        if add_mk {
            self.add_marker(cur);
        }
        // Ctrl+C / Ctrl+V : egui les transmet comme évènements Copier / Coller, pas comme touches
        let (copy, paste, shift) = ctx.input(|i| {
            let copy = i.events.iter().any(|e| matches!(e, egui::Event::Copy));
            let paste = i.events.iter().any(|e| matches!(e, egui::Event::Paste(_)));
            (copy, paste, i.modifiers.shift)
        });
        if copy && self.sel_actor.is_some() {
            self.copy_pose(ctx);
        }
        if paste && self.clipboard_pose.is_some() {
            self.paste_pose(shift);
        }
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        format!("{}…", s.chars().take(n).collect::<String>())
    } else {
        s.to_string()
    }
}

/// Grille au sol (pas de 0,5 m) avec les axes X (rouge) et Z (bleu), tournée de `rot` autour
/// de l'origine.
fn grid_lines(l: &mut Lines, n: i32, rot: Quat, origin: Vec3) {
    let h = n as f32 * 0.5;
    let mut line = |a: Vec3, b: Vec3, c: [f32; 4]| l.line(origin + rot * a, origin + rot * b, c, false);
    for i in -n..=n {
        let f = i as f32 * 0.5;
        let c = if i == 0 { [0.45, 0.45, 0.5, 0.9] } else { [0.3, 0.3, 0.33, 0.6] };
        line(Vec3::new(f, 0.0, -h), Vec3::new(f, 0.0, h), c);
        line(Vec3::new(-h, 0.0, f), Vec3::new(h, 0.0, f), c);
    }
    line(Vec3::new(0.0, 0.001, 0.0), Vec3::new(0.5, 0.001, 0.0), [0.9, 0.2, 0.2, 1.0]);
    line(Vec3::new(0.0, 0.001, 0.0), Vec3::new(0.0, 0.001, 0.5), [0.2, 0.4, 0.95, 1.0]);
}

/// Point du plan horizontal y = `height` sous le curseur (`None` si le rayon ne le coupe pas
/// devant la caméra, ou trop loin : regard presque à l'horizontale).
fn ground_point(vp: &Mat4, mouse: Vec2, size: Vec2, height: f32) -> Option<Vec3> {
    let (o, d) = camera::ray(vp, mouse, size);
    if d.y.abs() < 1e-4 {
        return None;
    }
    let t = (height - o.y) / d.y;
    (t > 0.0 && t < 2000.0).then(|| o + d * t)
}

/// Angle ramené entre -180° et 180°.
fn wrap_deg(a: f32) -> f32 {
    (a + 180.0).rem_euclid(360.0) - 180.0
}

/// Boutons du copier-coller de pose (inspecteur, menu Édition, clic droit dans la scène).
fn pose_buttons(ui: &mut egui::Ui, clip: Option<&PoseClipboard>, can_copy: bool) -> Option<PoseOp> {
    let mut op = None;
    let has = clip.is_some();
    let from = clip.map(|c| trf!(" de « {} »", " of “{}”", c.from)).unwrap_or_default();
    if ui
        .add_enabled(can_copy, egui::Button::new(tr!("Copier la pose", "Copy pose")).shortcut_text("Ctrl+C"))
        .on_hover_text(tr!("Copie la pose du personnage (avec toute son animation et sa position) pour la coller sur un autre", "Copies the character's pose (with all its animation and its position) to paste it onto another"))
        .clicked()
    {
        op = Some(PoseOp::Copy);
    }
    let paste = |ui: &mut egui::Ui, label: &str, short: &str, tip: String| {
        ui.add_enabled(has, egui::Button::new(label).shortcut_text(short))
            .on_hover_text(tip)
            .on_disabled_hover_text(tr!("Copiez d'abord la pose d'un personnage", "First copy a character's pose"))
            .clicked()
    };
    if paste(ui, tr!("Coller la pose", "Paste pose"), "Ctrl+V", trf!("Pose{from} posée en clé à l'image courante", "Pose{from} set as a key on the current frame")) {
        op = Some(PoseOp::Paste);
    }
    if paste(ui, tr!("Coller en miroir", "Paste mirrored"), tr!("Ctrl+Maj+V", "Ctrl+Shift+V"), trf!("Pose{from} inversée gauche / droite, à l'image courante", "Pose{from} flipped left / right, on the current frame")) {
        op = Some(PoseOp::PasteMirror);
    }
    if paste(
        ui,
        tr!("Coller l'animation", "Paste animation"),
        "",
        trf!(
            "Remplace toutes les clés des os par celles{from} (la position du personnage ne change pas)",
            "Replaces all bone keys with those{from} (the character's position does not change)"
        ),
    ) {
        op = Some(PoseOp::PasteAnim);
    }
    if paste(
        ui,
        tr!("Coller la position", "Paste position"),
        "",
        trf!(
            "Place le personnage à l'endroit et dans l'orientation{from}, à l'image courante (la pose ne change pas)",
            "Puts the character at the place and orientation{from}, on the current frame (the pose does not change)"
        ),
    ) {
        op = Some(PoseOp::PastePosition);
    }
    op
}


/// Vitesse de vol lisible : « 0.5 m/s », « 3 m/s », « 25 m/s ».
fn speed_label(speed: f32) -> String {
    if speed < 1.0 {
        format!("{speed:.2} m/s")
    } else if speed < 10.0 {
        format!("{speed:.1} m/s")
    } else {
        format!("{speed:.0} m/s")
    }
}

fn view_right(v: &ViewParams) -> Vec3 {
    (v.target - v.eye).normalize().cross(Vec3::Y).normalize_or(Vec3::X)
}

fn view_up(v: &ViewParams) -> Vec3 {
    view_right(v).cross((v.target - v.eye).normalize()).normalize_or(Vec3::Y)
}

/// Réglages de post-traitement (fenêtre « Post-traitement » et fenêtre « Exporter »).
pub(super) fn post_fx_controls(ui: &mut egui::Ui, fx: &mut crate::post::PostFx) {
    use crate::post::{PostFx, ToneMap};
    ui.horizontal(|ui| {
        ui.label("Ambiance");
        if ui.selectable_label(*fx == PostFx::default(), tr!("Neutre", "Neutral")).on_hover_text(tr!("Image telle que rendue", "Image as rendered")).clicked() {
            *fx = PostFx::default();
        }
        if ui
            .selectable_label(*fx == PostFx::cinema(), tr!("Cinéma", "Cinema"))
            .on_hover_text(tr!("Courbe filmique, un peu de contraste, vignettage, grain et netteté", "Filmic curve, a little contrast, vignette, grain and sharpness"))
            .clicked()
        {
            // garde la profondeur de champ en cours
            *fx = PostFx { dof: fx.dof, focus_auto: fx.focus_auto, focus: fx.focus, blur: fx.blur, ..PostFx::cinema() };
        }
        if ui
            .selectable_label(*fx == PostFx::game(), tr!("Jeu (live)", "Game (live)"))
            .on_hover_text(
                tr!("Calé sur des captures d'Indar du jeu : hautes lumières adoucies, dominante rose-lavande, \
                 ombres un peu plus profondes. À utiliser avec le ciel et le brouillard de la carte", "Matched to in-game captures of Indar: softened highlights, pink-lavender cast, slightly deeper shadows. Use with the map's sky and fog"),
            )
            .clicked()
        {
            *fx = PostFx { dof: fx.dof, focus_auto: fx.focus_auto, focus: fx.focus, blur: fx.blur, ..PostFx::game() };
        }
    });
    ui.separator();
    egui::Grid::new("postfx").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label(egui::RichText::new(tr!("Étalonnage", "Color grading")).strong());
        ui.end_row();
        ui.label(tr!("Exposition", "Exposure"));
        ui.add(egui::Slider::new(&mut fx.exposure, -3.0..=3.0).suffix(" IL"))
            .on_hover_text(tr!("+1 : deux fois plus clair (en lumière, avant la courbe)", "+1: twice as bright (in light, before the curve)"));
        ui.end_row();
        ui.label(tr!("Étalonnage du jeu", "Game color grading"));
        ui.checkbox(&mut fx.game_grading, tr!("Selon l'heure de la carte", "From the map's hour")).on_hover_text(tr!(
            "Comme le jeu : rendu des tons dont le point blanc suit l'heure du ciel de la carte (plein jour, \
             coucher, nuit), puis sa table d'étalonnage de l'heure. Remplace le rendu des tons ci-dessous",
            "Like the game: tone mapping whose white point follows the hour of the map's sky (daylight, \
             sunset, night), then its color grading table for that hour. Replaces the tone mapping below"
        ));
        ui.end_row();
        ui.label(tr!("Rendu des tons", "Tone mapping"));
        ui.add_enabled_ui(!fx.game_grading, |ui| {
            egui::ComboBox::from_id_salt("tonemap").selected_text(fx.tonemap.label()).show_ui(ui, |ui| {
                for t in ToneMap::ALL {
                    ui.selectable_value(&mut fx.tonemap, t, t.label());
                }
            })
            .response
            .on_hover_text(tr!("Ramène les lumières très fortes (lune, soleil, néons) vers le blanc en douceur, au lieu de les écrêter", "Brings very strong lights (moon, sun, neons) smoothly toward white, instead of clipping them"));
        });
        ui.end_row();
        ui.label(tr!("Contraste", "Contrast"));
        ui.add(egui::Slider::new(&mut fx.contrast, 0.5..=1.8));
        ui.end_row();
        ui.label("Saturation");
        ui.add(egui::Slider::new(&mut fx.saturation, 0.0..=2.0));
        ui.end_row();
        ui.label(tr!("Température", "Temperature"));
        ui.add(egui::Slider::new(&mut fx.temperature, -1.0..=1.0)).on_hover_text(tr!("Froid (bleu) ↔ chaud (orangé)", "Cool (blue) ↔ warm (orange)"));
        ui.end_row();
        ui.label(tr!("Teinte", "Tint"));
        ui.add(egui::Slider::new(&mut fx.tint, -1.0..=1.0)).on_hover_text(tr!("Vert ↔ magenta", "Green ↔ magenta"));
        ui.end_row();

        ui.label(egui::RichText::new(tr!("Objectif", "Lens")).strong());
        ui.end_row();
        ui.label(tr!("Vignettage", "Vignette"));
        ui.add(egui::Slider::new(&mut fx.vignette, 0.0..=1.0)).on_hover_text(tr!("Assombrit les bords de l'image", "Darkens the edges of the image"));
        ui.end_row();
        ui.label("Grain");
        ui.add(egui::Slider::new(&mut fx.grain, 0.0..=1.0)).on_hover_text(tr!("Grain de pellicule, un par pixel de l'image exportée", "Film grain, one per pixel of the exported image"));
        ui.end_row();
        ui.label(tr!("Netteté", "Sharpness"));
        ui.add(egui::Slider::new(&mut fx.sharpen, 0.0..=1.0))
            .on_hover_text(tr!("Fait ressortir les détails fins ; dans les exports, appliquée après la réduction du suréchantillonnage", "Brings out fine details; in exports, applied after the supersampling downscale"));
        ui.end_row();

        ui.label(egui::RichText::new(tr!("Profondeur de champ", "Depth of field")).strong());
        ui.checkbox(&mut fx.dof, tr!("Active", "On"));
        ui.end_row();
        ui.label(tr!("Mise au point", "Focus"));
        ui.add_enabled_ui(fx.dof, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut fx.focus_auto, true, tr!("Cible de la caméra", "Camera target"))
                    .on_hover_text(tr!("Le point visé par la caméra est net (il suit la caméra animée)", "The camera's target point is sharp (it follows the animated camera)"));
                ui.selectable_value(&mut fx.focus_auto, false, "Distance");
                ui.add_enabled(
                    !fx.focus_auto,
                    crate::calc::drag(&mut fx.focus).range(0.1..=2000.0).speed(0.05).suffix(" m"),
                );
            });
        });
        ui.end_row();
        ui.label(tr!("Flou du fond", "Background blur"));
        ui.add_enabled(fx.dof, egui::Slider::new(&mut fx.blur, 0.1..=3.0).suffix(" %"))
            .on_hover_text(tr!("Flou de l'arrière-plan lointain, en % de la hauteur de l'image ; le premier plan peut l'être davantage", "Blur of the far background, in % of the image height; the foreground can be blurrier"));
        ui.end_row();
    });
}

pub(crate) fn write_png(path: &std::path::Path, w: u32, h: u32, rgba: &[u8]) -> anyhow::Result<()> {
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(file, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut wr = enc.write_header()?;
    wr.write_image_data(rgba)?;
    Ok(())
}

impl eframe::App for StudioApp {
    /// Fermeture de la fenêtre : enregistre le projet s'il a changé.
    fn on_exit(&mut self) {
        // file d'attente en cours : remettre le projet de l'utilisateur avant de l'enregistrer
        self.queue_abort();
        if self.config.autosave.enabled {
            self.autosave();
        }
        // nouvelle version installée : relancée une fois le projet enregistré
        if self.restart_after_exit {
            crate::update::relaunch();
        }
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_loader();
        // fichiers .fbx glissés sur la fenêtre : import d'animation
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        for p in dropped {
            if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("fbx")) {
                self.import_fbx(p);
            } else {
                self.status = trf!("{} : seuls les .fbx (animations) se déposent ici", "{}: only .fbx files (animations) can be dropped here", p.display());
            }
        }
        self.sync_lighting();
        self.sync_camo_textures();

        let now = Instant::now();
        let dt = (now - self.last_tick).as_secs_f32().min(0.25);
        self.last_tick = now;
        if self.playing {
            let mut f = self.frame + dt * self.fps;
            if f > self.end as f32 {
                if self.looping && self.cam_rec.is_none() {
                    f = self.start as f32 + (f - self.end as f32) % ((self.end - self.start).max(1) as f32);
                } else {
                    f = self.end as f32;
                    self.playing = false;
                }
            }
            self.frame = f;
            ctx.request_repaint();
        }
        self.capture_binding(ctx);
        self.fly(ctx, dt);
        self.record_tick(dt);
        self.scene.evaluate(self.frame);
        self.apply_preview(ctx);

        // Image sans aucune entrée (ex. déplacement de la fenêtre, animation d'un widget) : si la
        // précédente n'en avait pas non plus et que rien ne tourne, la scène 3D n'a pas pu changer.
        let idle = ctx.input(|i| {
            i.events.is_empty() && i.pointer.delta() == egui::Vec2::ZERO && i.raw_scroll_delta == egui::Vec2::ZERO
        });
        self.skip_3d = idle
            && self.idle_prev
            && !self.force_render
            && !self.playing
            && self.preview.is_none()
            && !self.loader.is_busy()
            && self.drag.is_none()
            && !self.export.busy();
        self.idle_prev = idle;
        self.force_render = false;

        self.shortcuts(ctx);
        self.autosave_tick(ctx);

        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button(tr!("Fichier", "File"), |ui| {
                    if ui.button(tr!("Nouveau projet", "New project")).clicked() {
                        self.new_project();
                        ui.close_menu();
                    }
                    if ui.button(tr!("Ouvrir un projet...", "Open project...")).clicked() {
                        if let Some(p) = &self.config.last_project {
                            self.project_path = p.clone();
                        }
                        self.dialog = Some(Dialog::Open);
                        ui.close_menu();
                    }
                    if ui.button(trf!("Enregistrer le projet...  ({})", "Save project...  ({})", self.config.keymap.label(Action::Save))).clicked() {
                        self.dialog = Some(Dialog::Save);
                        ui.close_menu();
                    }
                    let mut changed = ui
                        .checkbox(&mut self.config.autosave.enabled, tr!("Enregistrement automatique", "Autosave"))
                        .on_hover_text(
                            tr!("Enregistre le projet à intervalle régulier et à la fermeture de l'application, \
                             dans son fichier (ou dans Documents/PlanetStudio/sans_titre_N.psproj s'il n'a pas encore de nom)", "Saves the project at regular intervals and when the application closes, to its file (or to Documents/PlanetStudio/sans_titre_N.psproj if it has no name yet)"),
                        )
                        .changed();
                    ui.add_enabled_ui(self.config.autosave.enabled, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(tr!("    toutes les", "    every"));
                            changed |= ui
                                .add(crate::calc::drag(&mut self.config.autosave.minutes).range(1..=60).suffix(" min"))
                                .changed();
                        });
                    });
                    if changed {
                        self.last_autosave = Instant::now();
                        self.config.save();
                    }
                    ui.separator();
                    if ui
                        .button(trf!("📸 Prendre une photo  ({})", "📸 Take a photo  ({})", self.config.keymap.label(Action::Photo)))
                        .on_hover_text(tr!("Image courante, par la caméra de prise, avec les réglages de l'onglet Image de la fenêtre Exporter", "Current frame, from the shot camera, with the settings of the Image tab of the Export window"))
                        .clicked()
                    {
                        self.take_photo();
                        ui.close_menu();
                    }
                    if ui.button(tr!("🎬 Exporter une vidéo… (H.264, AV1, GIF, images)", "🎬 Export a video… (H.264, AV1, GIF, images)")).clicked() {
                        self.open_export(ExportTab::Video);
                        ui.close_menu();
                    }
                    if ui.button(tr!("🖼 Exporter une image…", "🖼 Export an image…")).clicked() {
                        self.open_export(ExportTab::Image);
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button(tr!("📁 Changer de dossier de jeu…", "📁 Change game folder…")).clicked() {
                        self.game_error = None;
                        self.game_setup = true;
                        ui.close_menu();
                    }
                });
                ui.menu_button(tr!("Édition", "Edit"), |ui| {
                    if ui.button(trf!("Annuler  ({})", "Undo  ({})", self.config.keymap.label(Action::Undo))).clicked() {
                        self.undo_redo(false);
                        ui.close_menu();
                    }
                    if ui.button(trf!("Rétablir  ({})", "Redo  ({})", self.config.keymap.label(Action::Redo))).clicked() {
                        self.undo_redo(true);
                        ui.close_menu();
                    }
                    ui.separator();
                    if let Some(op) = pose_buttons(ui, self.clipboard_pose.as_ref(), self.sel_actor.is_some()) {
                        self.pose_op(ui.ctx(), op);
                        ui.close_menu();
                    }
                });
                ui.menu_button(tr!("Affichage", "View"), |ui| {
                    ui.checkbox(&mut self.show_grid, tr!("Grille", "Grid"));
                    ui.add_enabled_ui(self.show_grid, |ui| {
                        ui.horizontal(|ui| {
                            ui.add_space(18.0);
                            ui.label("Rotation");
                            ui.add(
                                crate::calc::drag(&mut self.grid_yaw)
                                    .speed(0.5)
                                    .range(-180.0..=180.0)
                                    .suffix("°")
                                    .max_decimals(1),
                            )
                            .on_hover_text(tr!("Tourne la grille au sol autour de l'origine (glisser ou taper un angle)", "Rotates the ground grid around the origin (drag or type an angle)"));
                            for (label, d) in [("⟲ 15°", 15.0), ("⟳ 15°", -15.0)] {
                                if ui.small_button(label).clicked() {
                                    self.grid_yaw = wrap_deg(self.grid_yaw + d);
                                }
                            }
                            if ui.small_button("0°").on_hover_text(tr!("Remettre la grille droite", "Straighten the grid")).clicked() {
                                self.grid_yaw = 0.0;
                            }
                        });
                        let can_align = self.sel_actor.is_some();
                        ui.horizontal(|ui| {
                            ui.add_space(18.0);
                            if ui
                                .add_enabled(can_align, egui::Button::new(tr!("Aligner sur le personnage sélectionné", "Align with the selected character")))
                                .on_hover_text(tr!("La grille prend l'orientation du personnage", "The grid takes the character's orientation"))
                                .on_disabled_hover_text(tr!("Sélectionnez d'abord un personnage", "First select a character"))
                                .clicked()
                            {
                                self.align_grid_to_selection();
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.add_space(18.0);
                            if ui
                                .add_enabled(
                                    can_align,
                                    egui::Button::new(trf!(
                                        "Placer sous le personnage sélectionné  ({})",
                                        "Put under the selected character  ({})",
                                        self.config.keymap.label(Action::GridToSelection)
                                    )),
                                )
                                .on_hover_text(tr!("Déplace la grille aux pieds du personnage (toit, étage…), sans changer son orientation", "Moves the grid to the character's feet (roof, upper floor…), without changing its orientation"))
                                .on_disabled_hover_text(tr!("Sélectionnez d'abord un personnage", "First select a character"))
                                .clicked()
                            {
                                self.place_grid_at_selection();
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.add_space(18.0);
                            if ui
                                .add_enabled(can_align, egui::Button::new(tr!("Poser le personnage au centre de la grille", "Put the character at the center of the grid")))
                                .on_hover_text(tr!("Amène les pieds du personnage sélectionné au centre de la grille, sans changer son orientation", "Brings the selected character's feet to the center of the grid, without changing its orientation"))
                                .on_disabled_hover_text(tr!("Sélectionnez d'abord un personnage", "First select a character"))
                                .clicked()
                            {
                                self.place_selection_on_grid();
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.add_space(18.0);
                            ui.label(tr!("Hauteur", "Height"));
                            ui.add(crate::calc::drag(&mut self.grid_origin.y).speed(0.05).suffix(" m").max_decimals(2))
                                .on_hover_text(tr!("Hauteur de la grille (glisser ou taper une valeur)", "Grid height (drag or type a value)"));
                            if ui
                                .add_enabled(self.grid_origin != Vec3::ZERO, egui::Button::new(tr!("Origine", "Origin")).small())
                                .on_hover_text(tr!("Ramener la grille à l'origine du monde (0, 0, 0)", "Bring the grid back to the world origin (0, 0, 0)"))
                                .clicked()
                            {
                                self.grid_origin = Vec3::ZERO;
                            }
                        });
                    });
                    ui.checkbox(&mut self.show_monitor, tr!("Moniteur de la caméra de prise", "Shot camera monitor"));
                    ui.checkbox(&mut self.show_skeleton, tr!("Squelette de la sélection", "Selection skeleton"));
                    ui.checkbox(&mut self.show_all_skeletons, tr!("Squelettes de tous les acteurs", "Skeletons of all actors"));
                    ui.separator();
                    if ui.button(tr!("⚙ Qualité graphique…", "⚙ Graphics quality…")).clicked() {
                        self.quality_open = true;
                        ui.close_menu();
                    }
                    if ui.button(tr!("🎨 Post-traitement…", "🎨 Post-processing…")).on_hover_text(tr!("Étalonnage, profondeur de champ, netteté, grain (enregistrés dans le projet)", "Color grading, depth of field, sharpness, grain (saved in the project)")).clicked() {
                        self.post_fx_open = true;
                        ui.close_menu();
                    }
                    if ui.button(tr!("⌨ Raccourcis clavier…", "⌨ Keyboard shortcuts…")).clicked() {
                        self.open_help(help_ui::HelpTab::Keys);
                        ui.close_menu();
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label("Ambiance");
                        egui::ComboBox::from_id_salt("ambiance")
                            .selected_text(self.lighting.ambiance.label())
                            .show_ui(ui, |ui| {
                                for a in Ambiance::ALL {
                                    ui.selectable_value(&mut self.lighting.ambiance, a, a.label());
                                }
                            });
                    });
                    let amb_animated = !self.scene.ambiance.is_empty();
                    let cur = self.cur_frame();
                    if amb_animated {
                        ui.label(
                            egui::RichText::new(trf!(
                                "Ambiance animée, {} clé(s) : orientation, intensité et fond posent une clé à l'image {cur}",
                                "Animated ambiance, {} key(s): orientation, intensity and background set a key at frame {cur}",
                                self.scene.ambiance.keys.len()
                            ))
                            .small()
                            .weak(),
                        );
                    }
                    // Seuls les gestes de l'utilisateur comptent : le bouton couleur d'egui
                    // repasse par TSV et modifie la couleur de quelques millièmes, ce qui
                    // poserait une clé fantôme à chaque image interpolée.
                    let mut amb = self.amb_at(self.frame);
                    let mut amb_changed = false;
                    ui.add_enabled_ui(self.lighting.ambiance != Ambiance::Studio, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Orientation");
                            amb_changed |= ui
                                .add(crate::calc::drag(&mut amb.rotation).range(-180.0..=180.0).speed(1.0).max_decimals(1).suffix("°"))
                                .changed();
                        });
                    });
                    ui.horizontal(|ui| {
                        ui.label(tr!("Intensité", "Intensity"));
                        amb_changed |= ui
                            .add(egui::Slider::new(&mut amb.intensity, 0.0..=3.0).custom_formatter(two_decimals))
                            .on_hover_text(tr!("Lumière d'ambiance (en mode Studio : dôme et remplissage)", "Ambient light (in Studio mode: dome and fill)"))
                            .changed();
                    });
                    ui.horizontal(|ui| {
                        ui.label(tr!("Fond", "Background"));
                        amb_changed |= ui
                            .color_edit_button_rgb(&mut amb.background)
                            .on_hover_text(tr!("Couleur de fond de la vue, des photos et des vidéos (animable avec l'ambiance)", "Background color of the viewport, photos and videos (animatable with the ambiance)"))
                            .changed();
                        if ui.small_button("↺").on_hover_text(tr!("Fond d'origine", "Original background")).clicked() {
                            amb.background = crate::anim::DEFAULT_BACKGROUND;
                            amb_changed = true;
                        }
                    });
                    if amb_changed {
                        self.playing = false;
                        self.frame = self.frame.round();
                        self.set_amb(amb);
                    }
                    if amb_animated && self.editing && !ui.ctx().input(|i| i.pointer.any_down()) {
                        self.editing = false;
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .button(tr!("🔑 Clé d'ambiance", "🔑 Ambiance key"))
                            .on_hover_text(tr!("Pose une clé à l'image courante : orientation et intensité deviennent animées", "Sets a key on the current frame: orientation and intensity become animated"))
                            .clicked()
                        {
                            self.key_amb();
                        }
                        if self.scene.ambiance.key_at(cur).is_some() && ui.button(tr!("✖ Supprimer la clé", "✖ Delete key")).clicked() {
                            self.delete_amb_key(cur);
                        }
                    });
                    ui.separator();
                    let animated = !self.scene.sun.is_empty();
                    let cur = self.cur_frame();
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(tr!("Soleil", "Sun")).strong());
                        let state = if animated {
                            trf!("animé, {} clé(s) : les réglages posent une clé à l'image {cur}", "animated, {} key(s): settings set a key at frame {cur}", self.scene.sun.keys.len())
                        } else {
                            tr!("fixe", "still").into()
                        };
                        ui.label(egui::RichText::new(state).small().weak());
                    });
                    // comme pour l'ambiance : on ne réagit qu'aux gestes, pas aux arrondis du bouton couleur
                    let mut st = self.sun_at(self.frame);
                    let mut sun_changed = false;
                    // avec le ciel de la carte, les moments de la journée règlent son heure : dôme,
                    // lune, brouillard et étalonnage suivent (sinon : soleil de nuit sous un ciel de midi)
                    let sky_shown = self.decor.applied.as_ref().is_some_and(|a| a.sky);
                    let mut sky_hour = None;
                    ui.horizontal_wrapped(|ui| {
                        for (i, p) in SUN_PRESETS.iter().enumerate() {
                            let hour = crate::environment::preset_sky_hour(i).filter(|_| sky_shown);
                            let mut b = ui.button(crate::environment::sun_preset_label(p.0));
                            if let Some(h) = hour {
                                b = b.on_hover_text(trf!("Ciel de la carte à {h:.0} h, soleil (ou lune) du jeu", "Map sky at {h:.0}:00, the game's sun (or moon)"));
                            }
                            if b.clicked() {
                                if hour.is_some() {
                                    sky_hour = hour;
                                } else if let Some(p) = preset_state(i) {
                                    st = p;
                                    sun_changed = true;
                                }
                            }
                        }
                    });
                    if let Some(h) = sky_hour {
                        self.set_sky_hour(h);
                    }
                    ui.horizontal(|ui| {
                        ui.label(tr!("Azimut", "Azimuth"));
                        sun_changed |= ui
                            .add(crate::calc::drag(&mut st.azimuth).range(-180.0..=180.0).speed(1.0).max_decimals(1).suffix("°"))
                            .changed();
                        ui.label(tr!("Hauteur", "Height"));
                        sun_changed |= ui
                            .add(crate::calc::drag(&mut st.elevation).range(1.0..=90.0).speed(0.5).max_decimals(1).suffix("°"))
                            .changed();
                    });
                    ui.horizontal(|ui| {
                        ui.label(tr!("Couleur", "Color"));
                        sun_changed |= ui.color_edit_button_rgb(&mut st.color).changed();
                        ui.label(tr!("Intensité", "Intensity"));
                        sun_changed |= ui.add(egui::Slider::new(&mut st.intensity, 0.0..=3.0).custom_formatter(two_decimals)).changed();
                    });
                    if sun_changed {
                        self.playing = false;
                        self.frame = self.frame.round();
                        self.set_sun(st);
                    }
                    // fin d'un glisser : l'annulation regroupe tout le geste
                    if animated && self.editing && !ui.ctx().input(|i| i.pointer.any_down()) {
                        self.editing = false;
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .button(tr!("🔑 Clé de soleil", "🔑 Sun key"))
                            .on_hover_text(tr!("Pose une clé à l'image courante : le soleil devient animé", "Sets a key on the current frame: the sun becomes animated"))
                            .clicked()
                        {
                            self.key_sun();
                        }
                        if self.scene.sun.key_at(cur).is_some() && ui.button(tr!("✖ Supprimer la clé", "✖ Delete key")).clicked() {
                            self.delete_sun_key(cur);
                        }
                    });
                    ui.checkbox(&mut self.show_sun, tr!("Montrer le soleil dans la vue de travail", "Show the sun in the work view"));
                    ui.checkbox(&mut self.lighting.shadows, tr!("Ombres portées du soleil", "Sun cast shadows"));
                    ui.add_enabled_ui(self.lighting.shadows, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(tr!("Ombre au sol", "Ground shadow"));
                            ui.add(egui::Slider::new(&mut self.lighting.ground_shadow, 0.0..=1.0));
                        });
                        ui.checkbox(&mut self.lighting.decor_shades_actors, tr!("Le décor ombre les personnages", "Scenery shades the characters"))
                            .on_hover_text(tr!(
                                "Décoché : les arbres et les bâtiments n'ombrent plus les personnages, qui \
                                 gardent le soleil et ses reflets même sous la canopée (le sol et le décor \
                                 restent ombrés). Éclairage de cinéma.",
                                "Unchecked: trees and buildings no longer shade the characters, who \
                                 keep the sun and its highlights even under the canopy (the ground and scenery \
                                 stay shaded). Movie lighting."
                            ));
                    });
                    ui.horizontal(|ui| {
                        ui.label(tr!("Champ de vision", "Field of view"));
                        ui.add(crate::calc::drag(&mut self.cam.fov_deg).range(10.0..=120.0).suffix("°"));
                    });
                });
                ui.menu_button(tr!("Décor", "Scenery"), |ui| self.decor_menu(ui));
                ui.menu_button(tr!("Aide", "Help"), |ui| {
                    if ui.button(tr!("📖 Prise en main", "📖 Getting started")).clicked() {
                        self.open_help(help_ui::HelpTab::Start);
                        ui.close_menu();
                    }
                    if ui.button(tr!("🖱 Souris et caméras", "🖱 Mouse and cameras")).clicked() {
                        self.open_help(help_ui::HelpTab::Mouse);
                        ui.close_menu();
                    }
                    if ui.button(tr!("⌨ Raccourcis clavier…", "⌨ Keyboard shortcuts…")).clicked() {
                        self.open_help(help_ui::HelpTab::Keys);
                        ui.close_menu();
                    }
                    if !DISCORD_URL.is_empty() {
                        ui.separator();
                        if ui
                            .button(tr!("💬 Rejoindre le Discord", "💬 Join the Discord"))
                            .on_hover_text(tr!("Questions, idées, partage de tes animations", "Questions, ideas, sharing your animations"))
                            .clicked()
                        {
                            ctx.open_url(egui::OpenUrl::new_tab(DISCORD_URL));
                            ui.close_menu();
                        }
                    }
                    self.update_menu(ui);
                });
                // nom écrit dans les deux langues : il se trouve quelle que soit la langue affichée
                ui.menu_button("🌐 Langue / Language", |ui| {
                    let cur = crate::i18n::current();
                    for lang in crate::i18n::Lang::ALL {
                        if ui.selectable_label(cur == lang, lang.native_name()).clicked() {
                            crate::i18n::set(lang);
                            self.config.lang = Some(lang);
                            self.config.save();
                            ui.close_menu();
                        }
                    }
                });
                ui.separator();
                ui.label(tr!("Gizmo :", "Gizmo:"));
                let km = &self.config.keymap;
                let (rot, tr, fr) =
                    (km.label(Action::GizmoRotate), km.label(Action::GizmoTranslate), km.label(Action::FrameSelection));
                ui.selectable_value(&mut self.gizmo, GizmoMode::Rotate, format!("⟳ Rotation ({rot})"));
                ui.selectable_value(&mut self.gizmo, GizmoMode::Translate, format!("✥ Translation ({tr})"));
                if ui.button(trf!("Cadrer ({fr})", "Frame ({fr})")).clicked() {
                    self.frame_selection();
                }
            });
        });
        self.update_banner(ctx);

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if self.loader.is_busy() {
                    ui.add(egui::Spinner::new().size(12.0));
                }
                ui.label(&self.status);
            });
        });

        egui::TopBottomPanel::bottom("timeline").resizable(false).show(ctx, |ui| {
            ui.add_space(4.0);
            self.timeline_ui(ui);
            ui.add_space(4.0);
        });

        egui::SidePanel::left("assets").default_width(330.0).min_width(300.0).show(ctx, |ui| self.assets_ui(ui));
        egui::SidePanel::right("scene").default_width(340.0).min_width(300.0).show(ctx, |ui| {
            egui::ScrollArea::vertical().id_salt("scene_scroll").show(ui, |ui| self.scene_ui(ui));
        });

        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ctx, |ui| self.viewport_ui(ui));

        self.export_ui(ctx);
        self.quality_ui(ctx);
        self.post_fx_ui(ctx);
        self.decor_window(ctx);
        self.shots_window(ctx);
        self.file_dialog_ui(ctx);
        self.help_window(ctx);
        self.game_setup_ui(ctx);
        self.export_step(ctx);
        // après toutes les listes : l'asset survolé à cette image est connu
        self.update_asset_preview(ctx);

        if !ctx.input(|i| i.pointer.any_down()) && self.drag.is_none() && self.key_drag.is_none() {
            self.editing = false;
        }
        if self.loader.is_busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

/// Affiche une valeur avec deux décimales sans la modifier (`Slider::max_decimals`
/// arrondirait la valeur elle-même à chaque image, et poserait une clé fantôme).
fn two_decimals(v: f64, _: std::ops::RangeInclusive<usize>) -> String {
    format!("{v:.2}")
}
