//! Décor : bâtiments, props, rochers et végétation placés par le jeu sur ses cartes (`.zone`),
//! chargés dans un rayon autour d'un point choisi.
//!
//! Le point choisi devient l'origine du studio : les personnages et la grille (y = 0) s'y
//! retrouvent, posés sur le sol. Le terrain du jeu (chunks `.cnk`, voir [`load_terrain`]) donne
//! la hauteur du sol ; sans lui, elle est estimée d'après les objets posés près du point. Elle
//! se corrige à la main.

use crate::render::DecorInstance;
use anyhow::{Context, Result};
use forgelight::zone::Zone;
use forgelight::{dds, GameData, Model};
use glam::{Mat4, Vec3};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Plus grand côté des textures décompressées (RGBA, 4 octets par pixel) : terrain, détails,
/// et décor quand le GPU ne lit pas les formats BC. Les niveaux de mip au-dessus sont ignorés :
/// des centaines de textures en pleine résolution ne tiendraient pas en mémoire.
pub const MAX_TEXTURE: u32 = 1024;

/// Plus grand côté des textures du décor gardées compressées (BC, 0,5 à 1 octet par pixel) :
/// la pleine résolution du jeu (2048 pour la plupart des bâtiments).
pub const MAX_COMPRESSED_TEXTURE: u32 = 4096;

/// Cartes proposées : (nom du fichier sans `.zone`, nom affiché).
pub const ZONES: &[(&str, &str)] = &[
    ("Indar", "Indar"),
    ("Amerish", "Amerish"),
    ("Esamir", "Esamir"),
    ("Hossin", "Hossin"),
    ("VR", "Réalité virtuelle (VR)"),
    ("Tutorial", "Tutoriel"),
    ("quickload", "quickload"),
];

/// Nom affiché d'une carte de `ZONES`.
pub fn zone_label(label: &'static str) -> &'static str {
    match label {
        "Réalité virtuelle (VR)" => tr!(label, "Virtual reality (VR)"),
        "Tutoriel" => tr!(label, "Tutorial"),
        l => l,
    }
}

fn yes() -> bool {
    true
}

fn one() -> f32 {
    1.0
}

fn sky_hour() -> f32 {
    crate::sky::DEFAULT_HOUR
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DecorSettings {
    /// carte (nom du fichier sans `.zone`)
    pub zone: String,
    /// point choisi, coordonnées du jeu : (x, z)
    pub center: [f32; 2],
    /// rayon de chargement (m)
    pub radius: f32,
    /// végétation (arbustes, arbres, herbes)
    #[serde(default = "yes")]
    pub flora: bool,
    /// hauteur du jeu ramenée à y = 0 ; `None` : estimée au chargement
    #[serde(default)]
    pub ground: Option<f32>,
    #[serde(default = "yes")]
    pub visible: bool,
    /// terrain (relief et sol du jeu)
    #[serde(default = "yes")]
    pub terrain: bool,
    /// ciel de la carte (dôme du jeu) derrière le décor
    #[serde(default = "yes")]
    pub sky: bool,
    /// heure du ciel (0 à 24), qui choisit le dôme de la carte
    #[serde(default = "sky_hour")]
    pub sky_hour: f32,
    /// le soleil et l'ambiance suivent l'heure du ciel
    #[serde(default = "yes")]
    pub sun_follows: bool,
    /// brouillard de la carte (et fondu du décor lointain dans le ciel)
    #[serde(default = "yes")]
    pub fog: bool,
    /// multiplicateur de la densité du brouillard (1 = celle du jeu)
    #[serde(default = "one")]
    pub fog_density: f32,
    /// rayons du soleil : l'air éclairé par le soleil le long du regard, coupé par les ombres
    /// (« Fog Shadows » du jeu)
    #[serde(default = "yes")]
    pub sun_rays: bool,
    /// force des rayons du soleil (1 = réglage d'origine)
    #[serde(default = "one")]
    pub sun_rays_strength: f32,
    /// lampes et projecteurs placés de la carte : ils éclairent le décor et les personnages
    #[serde(default = "yes")]
    pub lamps: bool,
    /// force des lampes (1 = réglage d'origine)
    #[serde(default = "one")]
    pub lamps_strength: f32,
}

impl Default for DecorSettings {
    fn default() -> Self {
        Self {
            zone: "Indar".into(),
            center: [0.0, 0.0],
            radius: 250.0,
            flora: true,
            ground: None,
            visible: true,
            terrain: true,
            sky: true,
            sky_hour: crate::sky::DEFAULT_HOUR,
            sun_follows: true,
            fog: true,
            fog_density: 1.0,
            sun_rays: true,
            sun_rays_strength: 1.0,
            lamps: true,
            lamps_strength: 1.0,
        }
    }
}

impl DecorSettings {
    /// Origine du studio dans le monde du jeu.
    pub fn origin(&self) -> Vec3 {
        Vec3::new(self.center[0], self.ground.unwrap_or(0.0), self.center[1])
    }
}

pub struct DecorData {
    /// réglages, avec la hauteur du sol renseignée
    pub settings: DecorSettings,
    pub models: Vec<(String, Arc<Model>)>,
    pub textures: Vec<(String, dds::Image)>,
    pub instances: Vec<DecorInstance>,
    /// acteurs illisibles (ignorés)
    pub failed: Vec<String>,
    /// le terrain a été chargé
    pub has_terrain: bool,
    /// ciel de la carte (dôme, brouillard, soleil), si demandé et trouvé
    pub sky: Option<crate::sky::Sky>,
    /// lumières placées dans le rayon (lampes, projecteurs)
    pub lights: Vec<crate::render::DecorLight>,
    /// flore du sol, posée ensuite autour de la caméra (`None` : végétation désactivée)
    pub ground_flora: Option<GroundFlora>,
}

pub fn is_flora(actor: &str) -> bool {
    let a = actor.to_ascii_lowercase();
    a.contains("flora") || a.contains("_grass")
}

/// Estime la hauteur du sol près du point : les objets y sont posés, on prend le 10e centile
/// de leurs hauteurs (les toits et passerelles sont plus haut, rien n'est sous le sol).
pub fn estimate_ground(zone: &Zone, center: [f32; 2]) -> Option<f32> {
    for r in [40.0f32, 100.0, 250.0, 600.0] {
        let mut ys: Vec<f32> = zone
            .objects
            .iter()
            .flat_map(|o| &o.instances)
            .filter(|i| {
                let (dx, dz) = (i.position.x - center[0], i.position.z - center[1]);
                dx * dx + dz * dz <= r * r
            })
            .map(|i| i.position.y)
            .collect();
        if ys.len() >= 5 {
            ys.sort_by(f32::total_cmp);
            return Some(ys[ys.len() / 10]);
        }
    }
    None
}

/// Retire les niveaux de mip plus grands que `max` pixels de côté.
pub fn limit_mips(img: &mut dds::Image, max: u32) {
    while img.mips.len() > 1 && (img.width > max || img.height > max) {
        img.mips.remove(0);
        img.width = (img.width / 2).max(1);
        img.height = (img.height / 2).max(1);
    }
}

/// Lit la carte et charge le décor autour du point. `skip_textures` : textures déjà en
/// mémoire (noms en minuscules). `progress` / `total` comptent les fichiers lus.
pub fn load(
    game: &GameData,
    settings: &DecorSettings,
    skip_textures: &HashSet<String>,
    progress: &AtomicUsize,
    total: &AtomicUsize,
) -> Result<DecorData> {
    let file = format!("{}.zone", settings.zone);
    let zone = Zone::parse(&game.read(&file)?).with_context(|| trf!("lecture de {file}", "reading {file}"))?;
    let mut settings = settings.clone();
    let terrain = if settings.terrain { load_terrain(game, &zone, &settings)? } else { TerrainParts::default() };
    if settings.ground.is_none() {
        settings.ground = Some(terrain.ground.or_else(|| estimate_ground(&zone, settings.center)).unwrap_or(0.0));
    }
    let origin = settings.origin();
    let to_studio = Mat4::from_translation(-origin);
    let r2 = settings.radius * settings.radius;

    // instances retenues
    // (acteur, matrice, ombre portée, teinte de la flore du sol)
    let mut placed: Vec<(String, Mat4, bool, Option<[f32; 3]>)> = Vec::new();
    for o in &zone.objects {
        if !settings.flora && is_flora(&o.actor) {
            continue;
        }
        for i in &o.instances {
            let (dx, dz) = (i.position.x - settings.center[0], i.position.z - settings.center[1]);
            if dx * dx + dz * dz <= r2 {
                placed.push((o.actor.clone(), to_studio * i.matrix(), !i.no_shadow, None));
            }
        }
    }
    let ground_flora = settings.flora.then(|| GroundFlora::new(&zone, &settings));
    let mut actors: BTreeSet<&String> = placed.iter().map(|p| &p.0).collect();
    // modèles de la flore du sol : tous ceux de la carte, la caméra pouvant aller partout
    actors.extend(ground_flora.iter().flat_map(|g| &g.models));
    total.store(actors.len(), Ordering::Relaxed);
    progress.store(0, Ordering::Relaxed);

    // modèles (en parallèle)
    let loaded: Vec<(String, Result<Model>)> = actors
        .par_iter()
        .map(|a| {
            let r = game.load_model(a);
            progress.fetch_add(1, Ordering::Relaxed);
            ((*a).clone(), r)
        })
        .collect();
    let mut models = Vec::new();
    let mut failed = Vec::new();
    for (a, r) in loaded {
        match r {
            Ok(m) => models.push((a, Arc::new(m))),
            Err(e) => {
                eprintln!("décor : {a} : {e:#}");
                failed.push(a);
            }
        }
    }

    // textures (en parallèle), réduites à MAX_COMPRESSED_TEXTURE (MAX_TEXTURE si décompressées)
    let mut names: Vec<String> = Vec::new();
    let mut seen = HashSet::new();
    for (_, m) in &models {
        for t in m.meshes.iter().flat_map(|m| m.render_textures()) {
            let key = t.to_ascii_lowercase();
            if !skip_textures.contains(&key) && seen.insert(key) {
                names.push(t.clone());
            }
        }
    }
    total.store(actors.len() + names.len(), Ordering::Relaxed);
    let textures: Vec<(String, dds::Image)> = names
        .par_iter()
        .filter_map(|t| {
            let r = game.load_texture_gpu(t);
            progress.fetch_add(1, Ordering::Relaxed);
            match r {
                Ok(mut img) => {
                    let max = if img.encoding == dds::Encoding::Rgba8 { MAX_TEXTURE } else { MAX_COMPRESSED_TEXTURE };
                    limit_mips(&mut img, max);
                    Some((t.clone(), img))
                }
                Err(e) => {
                    eprintln!("texture {t} : {e:#}");
                    None
                }
            }
        })
        .collect();

    let ok: HashSet<&String> = models.iter().map(|(a, _)| a).collect();
    // une plante de la carte sans modèle lisible n'est pas posée (et pas signalée : la plupart
    // ne servent pas près du lieu)
    let mut ground_flora = ground_flora;
    if let Some(g) = ground_flora.as_mut() {
        g.keep_models(&ok);
        failed.retain(|a| !g.models.contains(a));
    }
    let mut instances: Vec<DecorInstance> = placed
        .into_iter()
        .filter(|p| ok.contains(&p.0))
        .map(|(asset, matrix, cast_shadow, tint)| DecorInstance { asset, matrix, cast_shadow, tint })
        .collect();

    // terrain : un modèle par quart, une instance chacun
    let mut textures = textures;
    let mut models = models;
    let has_terrain = !terrain.quads.is_empty();
    textures.extend(terrain.details);
    for t in terrain.quads {
        instances.push(DecorInstance { asset: t.name.clone(), matrix: to_studio, cast_shadow: t.near, tint: None });
        textures.push((t.texture_name, t.texture));
        textures.extend(t.normal);
        textures.extend(t.blend);
        models.push((t.name, Arc::new(t.model)));
    }
    let sky = if settings.sky { load_sky(game, &settings) } else { None };
    let lights = zone
        .lights
        .iter()
        .filter(|l| {
            let (dx, dz) = (l.position.x - settings.center[0], l.position.z - settings.center[1]);
            l.range > 0.0 && dx * dx + dz * dz <= r2
        })
        .map(|l| crate::render::DecorLight::from_zone(l, origin))
        .collect();
    Ok(DecorData { settings, models, textures, instances, failed, has_terrain, sky, lights, ground_flora })
}

// ---------------------------------------------------------------- flore du sol

/// Distance (m) jusqu'où la flore du sol (herbes, fougères, nénuphars…) est posée autour de la
/// caméra : le jeu ne la dessine que de près, et elle compte des dizaines de milliers de plantes
/// dans ce rayon.
pub const GROUND_FLORA_RADIUS: f32 = 150.0;

/// La flore est reposée quand une caméra s'éloigne de plus de cette distance (m) du point
/// autour duquel elle l'a été.
const GROUND_FLORA_REFRESH: f32 = 20.0;

/// Côté d'un chunk `.cnk0` (m) : 4 × 4 tuiles de 64 m.
const FLORA_CHUNK: f32 = 256.0;

/// Une plante de la flore du sol, en coordonnées du jeu.
#[derive(Clone, Copy, Debug)]
struct Plant {
    pos: [f32; 3],
    yaw: f32,
    scale: f32,
    /// rang du modèle dans [`GroundFlora::models`]
    model: u16,
    /// teinte (linéaire)
    tint: [f32; 3],
}

/// Flore du sol : les plantes que le jeu a placées dans les `.cnk0` (voir [`forgelight::flora`]).
///
/// Comme dans le jeu, elle n'est posée qu'autour de la caméra, et non du point choisi pour le
/// décor : les chunks sont lus à la demande quand la caméra se déplace, et oubliés quand elle
/// s'en éloigne. Chaque plante devient une instance de son modèle (`.dme`, donné par la section
/// flore de la carte), tournée et mise à l'échelle (bornes de sa couche) d'après sa propre valeur,
/// pour un tirage stable d'un chargement à l'autre. Pas d'ombre : trop petites, et des dizaines
/// de milliers.
pub struct GroundFlora {
    zone: String,
    /// modèles des plantes de la carte
    pub models: Vec<String>,
    /// (écosystème, couche) → (modèle, échelle min, échelle max)
    layers: std::collections::HashMap<(u32, usize), (u16, f32, f32)>,
    /// point choisi pour le décor (x, z du jeu) : origine horizontale du studio
    center: [f32; 2],
    /// hauteur du jeu ramenée à y = 0 dans les matrices (celle du chargement)
    ground: f32,
    /// plantes des chunks lus, par chunk (a, b)
    chunks: std::collections::HashMap<(i32, i32), Arc<Vec<Plant>>>,
    /// points (x, z du jeu) autour desquels la flore affichée a été posée
    centers: Vec<[f32; 2]>,
}

impl GroundFlora {
    pub fn new(zone: &Zone, settings: &DecorSettings) -> Self {
        let mut models: Vec<String> = Vec::new();
        let mut layers = std::collections::HashMap::new();
        for eco in &zone.ecos {
            for (li, layer) in eco.flora.iter().enumerate() {
                let found = zone.floras.iter().find(|f| f.name.eq_ignore_ascii_case(&layer.flora));
                let Some(model) = found.map(|f| f.model.clone()).filter(|m| !m.is_empty()) else { continue };
                let k = match models.iter().position(|m| *m == model) {
                    Some(k) => k,
                    None => {
                        models.push(model);
                        models.len() - 1
                    }
                };
                layers.insert((eco.index, li), (k as u16, layer.min_scale, layer.max_scale));
            }
        }
        Self {
            zone: settings.zone.clone(),
            models,
            layers,
            center: settings.center,
            ground: settings.ground.unwrap_or(0.0),
            chunks: Default::default(),
            centers: Vec::new(),
        }
    }

    /// Ne garde que les couches dont le modèle a pu être chargé.
    fn keep_models(&mut self, loaded: &HashSet<&String>) {
        let models = &self.models;
        self.layers.retain(|_, l| loaded.contains(&models[l.0 as usize]));
    }

    /// Point du jeu (x, z) sous une position du studio.
    fn to_game(&self, p: Vec3) -> [f32; 2] {
        [p.x + self.center[0], p.z + self.center[1]]
    }

    /// Faut-il reposer la flore pour ces caméras (positions du studio) ?
    pub fn needs_update(&self, eyes: &[Vec3]) -> bool {
        let r2 = GROUND_FLORA_REFRESH * GROUND_FLORA_REFRESH;
        self.centers.is_empty()
            || eyes.iter().any(|e| {
                let [x, z] = self.to_game(*e);
                !self.centers.iter().any(|c| (c[0] - x).powi(2) + (c[1] - z).powi(2) <= r2)
            })
    }

    /// Flore autour de ces caméras (positions du studio), si elle doit changer : lit les chunks
    /// qui manquent, oublie ceux qui ne servent plus.
    pub fn update(&mut self, game: &GameData, eyes: &[Vec3]) -> Option<Vec<DecorInstance>> {
        if eyes.is_empty() || !self.needs_update(eyes) {
            return None;
        }
        let t = std::time::Instant::now();
        let centers: Vec<[f32; 2]> = eyes.iter().map(|e| self.to_game(*e)).collect();
        let r = GROUND_FLORA_RADIUS;
        // chunks de 4 × 4 tuiles de 64 m : `a` suit z, `b` suit x
        let mut wanted: BTreeSet<(i32, i32)> = BTreeSet::new();
        for [cx, cz] in &centers {
            let range = |c: f32| ((c - r) / FLORA_CHUNK).floor() as i32..=((c + r) / FLORA_CHUNK).floor() as i32;
            for a in range(*cz) {
                for b in range(*cx) {
                    wanted.insert((a * 4, b * 4));
                }
            }
        }
        self.chunks.retain(|k, _| wanted.contains(k));
        let missing: Vec<(i32, i32)> = wanted.iter().copied().filter(|k| !self.chunks.contains_key(k)).collect();
        let read: Vec<((i32, i32), Vec<Plant>)> = missing.par_iter().map(|&(a, b)| ((a, b), self.read_chunk(game, a, b))).collect();
        self.chunks.extend(read.into_iter().map(|(k, v)| (k, Arc::new(v))));

        let r2 = r * r;
        let origin = Vec3::new(self.center[0], self.ground, self.center[1]);
        let mut out = Vec::new();
        for plants in self.chunks.values() {
            for p in plants.iter() {
                let [x, y, z] = p.pos;
                if !centers.iter().any(|c| (x - c[0]).powi(2) + (z - c[1]).powi(2) <= r2) {
                    continue;
                }
                let matrix = Mat4::from_scale_rotation_translation(
                    Vec3::splat(p.scale),
                    glam::Quat::from_rotation_y(p.yaw),
                    Vec3::new(x, y, z) - origin,
                );
                let asset = self.models[p.model as usize].clone();
                out.push(DecorInstance { asset, matrix, cast_shadow: false, tint: Some(p.tint) });
            }
        }
        self.centers = centers;
        eprintln!(
            "flore du sol : {} plantes autour de {} caméra(s), {} chunks lus ({:.0} ms)",
            out.len(),
            eyes.len(),
            missing.len(),
            t.elapsed().as_secs_f32() * 1000.0
        );
        Some(out)
    }

    /// Plantes d'un chunk `.cnk0` (vide s'il manque ou est illisible).
    fn read_chunk(&self, game: &GameData, a: i32, b: i32) -> Vec<Plant> {
        let mut out = Vec::new();
        let name = format!("{}_{a}_{b}.cnk0", self.zone);
        let tiles = match game.read(&name).and_then(|d| forgelight::flora::parse_cnk0(&d)) {
            Ok(t) => t,
            Err(e) => {
                if !e.to_string().contains("introuvable") {
                    eprintln!("flore du sol : {name} : {e:#}");
                }
                return out;
            }
        };
        for t in &tiles {
            for te in &t.ecos {
                for (li, plants) in te.layers.iter().enumerate() {
                    let Some(&(model, min_scale, max_scale)) = self.layers.get(&(te.eco, li)) else { continue };
                    for p in plants {
                        let Some(pos) = t.place(p) else { continue };
                        // tirage stable : cap et taille d'après la valeur de la plante
                        let h = p.packed.wrapping_mul(0x9E37_79B9) ^ (p.packed >> 15);
                        let yaw = (h & 0xffff) as f32 / 65536.0 * std::f32::consts::TAU;
                        let k = ((h >> 16) & 0xffff) as f32 / 65536.0;
                        let scale = (min_scale + (max_scale - min_scale).max(0.0) * k).max(0.05);
                        // teinte : octets B, V, R, A (comme une couleur Direct3D)
                        let [b, g, r, _] = p.tint;
                        let tint = [r, g, b].map(|c| crate::sky::srgb_to_linear(c as f32));
                        out.push(Plant { pos, yaw, scale, model, tint });
                    }
                }
            }
        }
        out
    }
}

/// Ciel de la carte à l'heure réglée (`None`, avec un message, s'il est illisible).
pub fn load_sky(game: &GameData, settings: &DecorSettings) -> Option<crate::sky::Sky> {
    crate::sky::load(game, &settings.zone, settings.sky_hour)
        .map_err(|e| eprintln!("ciel : {e:#}"))
        .ok()
}

// ---------------------------------------------------------------- terrain

/// Distance (m) jusqu'où le terrain est chargé autour du lieu : au-delà, il n'y a plus que le ciel.
pub fn terrain_reach(s: &DecorSettings) -> f32 {
    TERRAIN_FAR.max(s.radius + 1000.0)
}

/// Côté d'une cellule du terrain (m) : un quart de `.cnk3`, ou 2 × 2 chunks `.cnk1`.
const TERRAIN_CELL: f32 = 512.0;
/// Le terrain lointain (`.cnk3`, un sommet tous les 8 m) s'étend jusqu'à cette distance.
const TERRAIN_FAR: f32 = 2500.0;
/// Le terrain détaillé (`.cnk1`, un sommet tous les 2 m) s'arrête à cette distance même avec un
/// plus grand rayon : sur 6 km, il ferait des dizaines de millions de sommets. Au-delà, `.cnk3`.
const NEAR_TERRAIN_MAX: f32 = 1000.0;
/// Taille maximale des textures du terrain lointain.
const FAR_TEXTURE: u32 = 256;
/// Distance jusqu'où le terrain garde ses textures en pleine résolution (m).
const NEAR_FULL_TEXTURE: f32 = 400.0;

pub struct TerrainQuad {
    pub name: String,
    pub model: Model,
    pub texture_name: String,
    pub texture: dds::Image,
    /// carte de normales (monde) du quart, si ses textures la contiennent
    pub normal: Option<(String, dds::Image)>,
    /// carte de mélange des écosystèmes du quart (voir [`quad_blend`])
    pub blend: Option<(String, dds::Image)>,
    /// terrain proche (`.cnk1`) : projette des ombres
    pub near: bool,
}

#[derive(Default)]
pub struct TerrainParts {
    pub quads: Vec<TerrainQuad>,
    /// textures de détail des écosystèmes (composées par [`eco_detail`])
    pub details: Vec<(String, dds::Image)>,
    /// hauteur du terrain au point choisi
    pub ground: Option<f32>,
}

/// Carte de normales d'un quart, rangée comme un `_N` (X dans A, Y dans G) mais en repère du
/// monde : X = x du monde (alpha de `ColorNX`), Y = z du monde (alpha de `SpecNY`). Convention
/// vérifiée contre les normales du maillage (flcli `normales`, cos moyen ≈ 0,97).
fn terrain_normals(color: &dds::Image, spec: &dds::Image) -> Option<dds::Image> {
    if (color.width, color.height) != (spec.width, spec.height) {
        return None;
    }
    let mips: Vec<Vec<u8>> = color
        .mips
        .iter()
        .zip(&spec.mips)
        .take_while(|(c, s)| c.len() == s.len())
        .map(|(c, s)| {
            c.chunks_exact(4).zip(s.chunks_exact(4)).flat_map(|(c, s)| [128, s[3], 128, c[3]]).collect()
        })
        .collect();
    (!mips.is_empty()).then(|| dds::Image::rgba(color.width, color.height, mips))
}

/// Écosystème de roche (falaises, pentes) plutôt que de sol.
fn is_rock_eco(e: &forgelight::zone::Eco) -> bool {
    e.physics.to_ascii_lowercase().starts_with("rock") || e.name.to_ascii_lowercase().contains("rock")
}

/// Nom de la texture de détail composée d'un écosystème.
fn eco_texture_name(e: &forgelight::zone::Eco) -> String {
    format!("terrain/eco_{}_n.dds", e.color_nx.trim_end_matches(".dds").to_ascii_lowercase())
}

/// Détails d'un quart : l'écosystème de sol et celui de roche les plus présents sur ses tuiles
/// (les premiers de chaque liste comptent davantage), avec le côté d'une répétition (m).
fn quad_details(ecos: &[forgelight::zone::Eco], lists: &[Vec<u8>]) -> Vec<(String, f32)> {
    let pick = |rock: bool| {
        let mut score: std::collections::HashMap<u32, f32> = Default::default();
        for list in lists {
            for (k, &i) in list.iter().enumerate() {
                if ecos.iter().any(|e| e.index == i as u32 && is_rock_eco(e) == rock) {
                    *score.entry(i as u32).or_default() += 1.0 / (k as f32 + 1.0);
                }
            }
        }
        let best = score.into_iter().max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0))).map(|s| s.0);
        best.and_then(|i| ecos.iter().find(|e| e.index == i)).or_else(|| ecos.iter().find(|e| is_rock_eco(e) == rock))
    };
    [pick(false), pick(true)]
        .into_iter()
        .flatten()
        .map(|e| (eco_texture_name(e), 64.0 / e.detail_repeat.max(1) as f32))
        .collect()
}

/// Texture de détail d'un écosystème, rangée comme un `_N` : R = luminosité relative (0,5 =
/// moyenne), G = normale Y (alpha de `_sbny`), A = normale X (alpha de `_cnx`). Seule la
/// luminosité du détail est reprise : la couleur reste celle du terrain.
fn eco_detail(game: &GameData, e: &forgelight::zone::Eco) -> Result<dds::Image> {
    let mut c = game.load_texture(&e.color_nx)?;
    let mut s = game.load_texture(&e.spec_blend_ny)?;
    limit_mips(&mut c, MAX_TEXTURE);
    limit_mips(&mut s, MAX_TEXTURE);
    anyhow::ensure!((c.width, c.height) == (s.width, s.height), "tailles différentes");
    let luma = |p: &[u8]| 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32;
    let mean = {
        let m = &c.mips[0];
        (m.chunks_exact(4).map(luma).sum::<f32>() / (m.len() / 4).max(1) as f32).max(1.0)
    };
    let mips = c
        .mips
        .iter()
        .zip(&s.mips)
        .take_while(|(a, b)| a.len() == b.len())
        .map(|(cm, sm)| {
            cm.chunks_exact(4)
                .zip(sm.chunks_exact(4))
                .flat_map(|(cp, sp)| [((luma(cp) / mean) * 127.5).clamp(0.0, 255.0) as u8, sp[3], 128, cp[3]])
                .collect()
        })
        .collect();
    Ok(dds::Image::rgba(c.width, c.height, mips))
}

/// Carte de mélange d'un quart (128 px, 1 m par pixel) assemblée à partir des tuiles de 64 m du
/// `.cnk0` de même nom, avec les écosystèmes de chaque tuile (numéros du `.zone`, −1 : aucun).
///
/// Chaque tuile du `.cnk0` range une image (DXT1 ou DXT5, 64 px) : ses canaux R, V, B, A sont la
/// présence des écosystèmes 1 à 4 de sa liste, peints l'un sur l'autre dans cet ordre par-dessus
/// l'écosystème 0, qui fait le fond. Vérifié avec la flore (`flcli melange`) : les plantes d'un
/// écosystème tombent là où son canal est fort (≈ 230 sur 255), les autres canaux restant bas.
/// Les lignes de l'image suivent le x du monde, les colonnes le z, comme la couleur du quart.
fn quad_blend(tiles: &[forgelight::flora::FloraTile], a: i32, b: i32, q: usize) -> Option<(dds::Image, Vec<i32>)> {
    const T: usize = 64;
    let (ci, cj) = ((q >> 1) as i32, (q & 1) as i32);
    let mut px = vec![0u8; 4 * T * T * 4];
    let mut ecos = vec![-1i32; 4 * 5];
    let mut found = false;
    for t in tiles {
        // rang dans le chunk : x de la tuile suit le z du monde (a), y le x (b)
        let (tz, tx) = (t.x - a, t.y - b);
        if !(0..4).contains(&tx) || !(0..4).contains(&tz) || tx / 2 != ci || tz / 2 != cj {
            continue;
        }
        let (row, col) = ((tx % 2) as usize, (tz % 2) as usize);
        let slot = row * 2 + col;
        for (k, e) in t.ecos.iter().take(5).enumerate() {
            ecos[slot * 5 + k] = e.eco as i32;
        }
        found |= !t.ecos.is_empty();
        let used = t.ecos.len().saturating_sub(1).min(4);
        let img = (!t.blend_dds.is_empty()).then(|| dds::decode(&t.blend_dds).and_then(|i| i.to_rgba())).and_then(|r| r.ok());
        let Some(img) = img.filter(|i| i.width > 0 && i.height > 0) else { continue };
        let (w, h) = (img.width as usize, img.height as usize);
        let src = &img.mips[0];
        for y in 0..T {
            for x in 0..T {
                let (sx, sy) = (x * w / T, y * h / T);
                let o = (sy * w + sx) * 4;
                let d = ((row * T + y) * 2 * T + col * T + x) * 4;
                for k in 0..4 {
                    // (canal sans écosystème : l'alpha d'une DXT1 vaut 255)
                    px[d + k] = if k < used { src[o + k] } else { 0 };
                }
            }
        }
    }
    found.then(|| (dds::Image::rgba(2 * T as u32, 2 * T as u32, vec![px]), ecos))
}

/// Textures de détail des écosystèmes, en deux tableaux à couches de même taille (une couche
/// par texture) :
/// - couleur : RGB = couleur du détail rapportée à sa moyenne (×0,5, en linéaire : 0,5 = la
///   couleur du terrain inchangée), A = hauteur de mélange (canal B de `_sbny`) ;
/// - relief : rangé comme un `_N` (normale X dans A, Y dans G).
fn eco_layers(game: &GameData, ecos: &[&forgelight::zone::Eco]) -> Result<[dds::Image; 2]> {
    let lin: Vec<f32> = (0..256)
        .map(|v| {
            let c = v as f32 / 255.0;
            if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        })
        .collect();
    let loaded: Vec<Option<(dds::Image, dds::Image)>> = ecos
        .par_iter()
        .map(|e| {
            let r = (|| -> Result<_> {
                let mut c = game.load_texture(&e.color_nx)?.to_rgba()?;
                let mut s = game.load_texture(&e.spec_blend_ny)?.to_rgba()?;
                limit_mips(&mut c, MAX_TEXTURE);
                limit_mips(&mut s, MAX_TEXTURE);
                limit_mips(&mut s, c.width.max(c.height));
                limit_mips(&mut c, s.width.max(s.height));
                anyhow::ensure!((c.width, c.height) == (s.width, s.height) && c.width == c.height, "tailles différentes");
                Ok((c, s))
            })();
            r.map_err(|err| eprintln!("terrain : détail {} : {err:#}", e.name)).ok()
        })
        .collect();
    // taille commune : la plus petite
    let size = loaded.iter().flatten().map(|(c, _)| c.width).min().context(tr!("aucune texture de détail", "no detail texture"))?;
    let mut levels = usize::MAX;
    let mut layers: Vec<[Vec<Vec<u8>>; 2]> = Vec::new();
    for l in loaded {
        let (mut cm, mut nm): (Vec<Vec<u8>>, Vec<Vec<u8>>) = (Vec::new(), Vec::new());
        if let Some((mut c, mut s)) = l {
            limit_mips(&mut c, size);
            limit_mips(&mut s, size);
            // moyenne linéaire de chaque canal
            let m0 = &c.mips[0];
            let n = (m0.len() / 4).max(1) as f32;
            let mean: [f32; 3] =
                std::array::from_fn(|k| (m0.chunks_exact(4).map(|p| lin[p[k] as usize]).sum::<f32>() / n).max(1e-4));
            for (cl, sl) in c.mips.iter().zip(&s.mips).take_while(|(a, b)| a.len() == b.len()) {
                cm.push(
                    cl.chunks_exact(4)
                        .zip(sl.chunks_exact(4))
                        .flat_map(|(cp, sp)| {
                            let r = |k: usize| ((lin[cp[k] as usize] / mean[k]) * 0.5 * 255.0).clamp(0.0, 255.0) as u8;
                            [r(0), r(1), r(2), sp[2]]
                        })
                        .collect(),
                );
                nm.push(cl.chunks_exact(4).zip(sl.chunks_exact(4)).flat_map(|(cp, sp)| [128, sp[3], 128, cp[3]]).collect());
            }
        }
        if cm.is_empty() {
            // texture illisible : couche neutre (couleur du terrain, sol plat)
            let mut w = size as usize;
            loop {
                cm.push([128u8, 128, 128, 128].repeat(w * w));
                nm.push([128u8, 128, 128, 128].repeat(w * w));
                if w <= 1 {
                    break;
                }
                w /= 2;
            }
        }
        levels = levels.min(cm.len());
        layers.push([cm, nm]);
    }
    // (une seule couche serait lue comme une texture simple)
    if layers.len() == 1 {
        layers.push(layers[0].clone());
    }
    let count = layers.len() as u32;
    let build = |k: usize| {
        let mips = (0..levels).map(|lv| layers.iter().flat_map(|l| l[k][lv].iter().copied()).collect()).collect();
        let mut img = dds::Image::rgba(size, size, mips);
        img.layers = count;
        img
    };
    Ok([build(0), build(1)])
}

/// Ajoute une jupe verticale de `depth` mètres sous les arêtes de bord du maillage (celles qui
/// n'appartiennent qu'à un triangle). Mêmes normales et UV que le bord : invisible sous le sol,
/// elle ne se voit qu'à travers une fissure, de la couleur du terrain.
fn add_skirt(mesh: &mut forgelight::terrain::QuadMesh, depth: f32) {
    let mut edges: std::collections::HashMap<(u32, u32), u32> = Default::default();
    for t in mesh.indices.chunks_exact(3) {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            *edges.entry((a.min(b), a.max(b))).or_default() += 1;
        }
    }
    let mut lowered: std::collections::HashMap<u32, u32> = Default::default();
    let mut border: Vec<(u32, u32)> = edges.into_iter().filter(|e| e.1 == 1).map(|e| e.0).collect();
    border.sort_unstable();
    for (a, b) in border {
        let mut low = |i: u32| {
            *lowered.entry(i).or_insert_with(|| {
                let k = i as usize;
                mesh.positions.push(mesh.positions[k] - Vec3::Y * depth);
                mesh.normals.push(mesh.normals[k]);
                mesh.uvs.push(mesh.uvs[k]);
                (mesh.positions.len() - 1) as u32
            })
        };
        let (la, lb) = (low(a), low(b));
        mesh.indices.extend_from_slice(&[a, b, lb, a, lb, la]);
    }
}

/// Les normales des sommets sont calculées quart par quart : sur un bord, chaque côté n'a vu que
/// ses propres triangles. On moyenne celles des sommets communs, sans quoi le décalage le long de
/// la normale des ombres diffère d'un quart à l'autre et assombrit une bande le long du bord.
fn stitch_normals(quads: &mut [TerrainQuad]) {
    // position exacte : pas de 2 m (ou plus) en x et z, hauteurs en 1/32 m
    let key = |p: [f32; 3]| ((p[0] * 4.0).round() as i64, (p[1] * 32.0).round() as i64, (p[2] * 4.0).round() as i64);
    let mut sum: std::collections::HashMap<(i64, i64, i64), (Vec3, u32)> = Default::default();
    for q in quads.iter() {
        for v in &q.model.meshes[0].vertices {
            let e = sum.entry(key(v.position)).or_insert((Vec3::ZERO, 0));
            e.0 += Vec3::from(v.normal);
            e.1 += 1;
        }
    }
    for q in quads.iter_mut() {
        for v in &mut q.model.meshes[0].vertices {
            if let Some((n, c)) = sum.get(&key(v.position)).filter(|s| s.1 > 1) {
                v.normal = n.normalize_or(Vec3::from(v.normal) / *c as f32).to_array();
            }
        }
    }
}

/// Côtés de chaque quart proche où le voisin n'a pas le même détail (autre écosystème, terrain
/// lointain sans détail, ou rien) : le shader y estompe le détail, qui passe sans marche d'un
/// quart à l'autre.
fn mark_detail_edges(quads: &mut [TerrainQuad]) {
    let key = |q: &TerrainQuad| {
        let lo = q.model.bounds_min;
        ((lo.x / 128.0).round() as i32, (lo.z / 128.0).round() as i32)
    };
    let details: std::collections::HashMap<(i32, i32), Vec<String>> = quads
        .iter()
        .filter(|q| q.near)
        .map(|q| {
            let m = &q.model.meshes[0];
            // (le mélange au pixel près passe tout seul d'un quart à l'autre)
            let d: Vec<String> =
                if m.terrain_blend.is_some() { vec!["#mélange".into()] } else { m.terrain_detail.iter().map(|d| d.0.clone()).collect() };
            (key(q), d)
        })
        .collect();
    for q in quads.iter_mut().filter(|q| q.near) {
        let (i, j) = key(q);
        let own = &details[&(i, j)];
        let mut mask = 0u8;
        for (bit, n) in [(1u8, (i - 1, j)), (2, (i + 1, j)), (4, (i, j - 1)), (8, (i, j + 1))] {
            if details.get(&n) != Some(own) {
                mask |= bit;
            }
        }
        q.model.meshes[0].terrain_edges = mask;
    }
}

/// Modèle rigide d'un quart de terrain, texturé par sa couleur.
fn terrain_model(name: &str, texture: &str, normal: Option<&str>, mesh: forgelight::terrain::QuadMesh) -> Model {
    let vertices: Vec<forgelight::Vertex> = mesh
        .positions
        .iter()
        .zip(&mesh.normals)
        .zip(&mesh.uvs)
        .map(|((p, n), uv)| forgelight::Vertex {
            position: p.to_array(),
            normal: n.to_array(),
            uv: uv.to_array(),
            joints: [0; 4],
            weights: [1.0, 0.0, 0.0, 0.0],
            tangent: [0.0; 4],
            uv1: uv.to_array(),
            uv2: uv.to_array(),
        })
        .collect();
    let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for p in &mesh.positions {
        lo = lo.min(*p);
        hi = hi.max(*p);
    }
    Model {
        name: name.to_string(),
        meshes: vec![forgelight::Mesh {
            vertices,
            indices: mesh.indices,
            material_definition: "Terrain".into(),
            layout: "Terrain".into(),
            textures: vec![texture.to_string()],
            texture_slots: Vec::new(),
            diffuse: Some(texture.to_string()),
            normal_map: normal.map(String::from),
            specular: None,
            camo: None,
            decal: None,
            terrain_detail: Vec::new(),
            terrain_edges: 0,
            terrain_blend: None,
            terrain_ecos: Vec::new(),
            terrain_layers: None,
            color: None,
            detail_repeat: 1.0,
            water_tint: None,
        }],
        skeleton: Default::default(),
        bounds_min: lo,
        bounds_max: hi,
        warnings: Vec::new(),
        equip_slot: None,
        covered_slots: Vec::new(),
    }
}

/// Mélange au pixel près : une couche de détail par texture d'écosystème employée, rangées dans
/// deux tableaux (voir [`eco_layers`]) ; les numéros d'écosystème des quarts deviennent des
/// numéros de couche. Sans tableaux, les quarts gardent leur détail simple.
fn blend_layers(game: &GameData, zone: &Zone, zone_name: &str, out: &mut TerrainParts) {
    let tex = |e: &forgelight::zone::Eco| e.color_nx.to_ascii_lowercase();
    let mut names: std::collections::BTreeMap<String, &forgelight::zone::Eco> = Default::default();
    for q in &out.quads {
        for &(e, _) in &q.model.meshes[0].terrain_ecos {
            if let Some(eco) = zone.ecos.iter().find(|z| z.index as i32 == e) {
                names.entry(tex(eco)).or_insert(eco);
            }
        }
    }
    let ecos: Vec<&forgelight::zone::Eco> = names.values().copied().collect();
    let arrays = if ecos.is_empty() { None } else { eco_layers(game, &ecos).map_err(|e| eprintln!("terrain : mélange : {e:#}")).ok() };
    let Some([color, relief]) = arrays else {
        for q in &mut out.quads {
            let m = &mut q.model.meshes[0];
            m.terrain_blend = None;
            m.terrain_ecos.clear();
            q.blend = None;
        }
        return;
    };
    let layer: std::collections::HashMap<&String, i32> = names.keys().enumerate().map(|(i, n)| (n, i as i32)).collect();
    for q in &mut out.quads {
        for (e, _) in &mut q.model.meshes[0].terrain_ecos {
            *e = zone.ecos.iter().find(|z| z.index as i32 == *e).and_then(|z| layer.get(&tex(z)).copied()).unwrap_or(-1);
        }
    }
    // nom propre à cette liste de couches (les matériaux s'y réfèrent par numéro)
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&names.keys().collect::<Vec<_>>(), &mut h);
    let id = format!("{:x}", std::hash::Hasher::finish(&h));
    let (cn, rn) = (format!("terrain/{zone_name}_eco_{id}_c_mel.dds"), format!("terrain/{zone_name}_eco_{id}_r_mel.dds"));
    for q in &mut out.quads {
        if q.model.meshes[0].terrain_blend.is_some() {
            q.model.meshes[0].terrain_layers = Some([cn.clone(), rn.clone()]);
        }
    }
    out.details.push((cn, color));
    out.details.push((rn, relief));
}

/// Charge le terrain autour du point : `.cnk1` dans le rayon (borné à [`NEAR_TERRAIN_MAX`]),
/// `.cnk3` au-delà jusqu'à [`terrain_reach`]. Le terrain est découpé en cellules de 512 m qui sont entièrement d'un
/// niveau ou de l'autre (pas de recouvrement).
pub fn load_terrain(game: &GameData, zone: &Zone, s: &DecorSettings) -> Result<TerrainParts> {
    use forgelight::terrain::{chunk_name, chunk_tiles, Chunk};
    let h = &zone.header;
    let (x_start, z_start) = (h.start_x as f32 * h.tile_size, h.start_y as f32 * h.tile_size);
    let extent = zone.extent();
    let ncells = (extent / TERRAIN_CELL).ceil() as i32;
    let [cx, cz] = s.center;
    let far = terrain_reach(s);
    // distance du point à une cellule
    let dist = |i: i32, j: i32| {
        let (x0, z0) = (x_start + i as f32 * TERRAIN_CELL, z_start + j as f32 * TERRAIN_CELL);
        let dx = (x0 - cx).max(0.0).max(cx - (x0 + TERRAIN_CELL));
        let dz = (z0 - cz).max(0.0).max(cz - (z0 + TERRAIN_CELL));
        (dx * dx + dz * dz).sqrt()
    };
    // travaux : (niveau, a, b, quarts voulus)
    let mut jobs: std::collections::BTreeMap<(u32, i32, i32), Vec<usize>> = Default::default();
    for i in 0..ncells {
        for j in 0..ncells {
            let d = dist(i, j);
            if d > far {
                continue;
            }
            // coin minimal de la cellule, en tuiles
            let (bx, az) = (h.start_x + i * 8, h.start_y + j * 8);
            if d <= s.radius.min(NEAR_TERRAIN_MAX) {
                for (da, db) in [(0, 0), (0, 4), (4, 0), (4, 4)] {
                    jobs.entry((1, az + da, bx + db)).or_default().extend(0..4);
                }
            } else {
                let step = chunk_tiles(3);
                let a = h.start_y + (az - h.start_y).div_euclid(step) * step;
                let b = h.start_x + (bx - h.start_x).div_euclid(step) * step;
                let ci = ((bx - b) / 8) as usize;
                let cj = ((az - a) / 8) as usize;
                jobs.entry((3, a, b)).or_default().push(ci * 2 + cj);
            }
        }
    }
    let zone_name = &s.zone;
    let results: Vec<(TerrainParts, Option<f32>)> = jobs
        .par_iter()
        .map(|(&(lod, a, b), quads)| {
            let name = chunk_name(zone_name, a, b, lod);
            let chunk = match game.read(&name).and_then(|d| Chunk::parse(&d)) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("terrain {name} : {e:#}");
                    return (TerrainParts::default(), None);
                }
            };
            // carte de mélange des écosystèmes (tuiles du `.cnk0` de même nom), de près seulement
            let tiles = if lod == 1 {
                match game.read(&chunk_name(zone_name, a, b, 0)).and_then(|d| forgelight::flora::parse_cnk0(&d)) {
                    Ok(t) => t,
                    Err(e) => {
                        eprintln!("terrain {zone_name}_{a}_{b}.cnk0 : {e:#}");
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };
            let mut parts = TerrainParts::default();
            for &q in quads.iter().filter(|&&q| q < chunk.quads.len()) {
                let quad = &chunk.quads[q];
                if quad.indices.is_empty() {
                    continue;
                }
                let mut mesh = chunk.quad_mesh(a, b, q);
                // jupe sous les bords : bouche les fissures entre le terrain proche (un sommet
                // tous les 2 m) et le lointain (8 m), dont les bords ne tombent pas l'un sur l'autre
                add_skirt(&mut mesh, if lod == 1 { 12.0 } else { 40.0 });
                let base = format!("terrain/{zone_name}_{a}_{b}_lod{lod}_q{q}");
                let tex_name = format!("{base}_c.dds");
                let mut tex = match dds::decode(&quad.color_dds) {
                    Ok(t) => t,
                    Err(e) => {
                        eprintln!("terrain {name} : texture : {e:#}");
                        continue;
                    }
                };
                // pleine résolution (512 px pour 128 m) près du point seulement : la mémoire reste
                // raisonnable avec un grand rayon
                let (qx, qz) = forgelight::terrain::quad_origin(a, b, lod, q);
                let qs = 64.0 * forgelight::terrain::vertex_spacing(lod);
                let (dx, dz) = ((qx - cx).max(0.0).max(cx - qx - qs), (qz - cz).max(0.0).max(cz - qz - qs));
                let near_full = lod == 1 && (dx * dx + dz * dz).sqrt() < NEAR_FULL_TEXTURE;
                let max = if near_full { MAX_TEXTURE } else { FAR_TEXTURE };
                limit_mips(&mut tex, max);
                // relief fin des textures (un texel tous les 25 cm de près), plus détaillé que le
                // maillage (un sommet tous les 2 m, 8 m au loin)
                let normal = dds::decode(&quad.spec_dds).ok().and_then(|mut s| {
                    limit_mips(&mut s, max);
                    terrain_normals(&tex, &s).map(|n| (format!("{base}_n.dds"), n))
                });
                let mut model = terrain_model(&base, &tex_name, normal.as_ref().map(|n| n.0.as_str()), mesh);
                // détail des écosystèmes : seulement de près (au loin il n'est plus visible)
                let mut blend = None;
                if lod == 1 {
                    model.meshes[0].terrain_detail = quad_details(&zone.ecos, &quad.ecos);
                    // mélange au pixel près : écosystèmes en numéros du `.zone` (couches plus tard)
                    if let Some((img, ecos)) = quad_blend(&tiles, a, b, q) {
                        let mesh = &mut model.meshes[0];
                        let name = format!("{base}_mel.dds");
                        mesh.terrain_blend = Some(name.clone());
                        mesh.terrain_ecos = ecos
                            .iter()
                            .map(|&e| {
                                let rep = zone.ecos.iter().find(|z| z.index as i32 == e).map_or(1, |z| z.detail_repeat.max(1));
                                (e, 64.0 / rep as f32)
                            })
                            .collect();
                        blend = Some((name, img));
                    }
                }
                parts.quads.push(TerrainQuad { name: base, model, texture_name: tex_name, texture: tex, normal, blend, near: lod == 1 });
            }
            let ground = if lod == 1 { chunk.height_at(a, b, cx, cz) } else { None };
            (parts, ground)
        })
        .collect();
    let mut out = TerrainParts::default();
    for (p, g) in results {
        out.quads.extend(p.quads);
        out.ground = out.ground.or(g);
    }
    blend_layers(game, zone, &s.zone, &mut out);
    mark_detail_edges(&mut out.quads);
    stitch_normals(&mut out.quads);
    // textures de détail des écosystèmes utilisés
    let used: BTreeSet<&String> = out.quads.iter().flat_map(|q| q.model.meshes[0].terrain_detail.iter().map(|d| &d.0)).collect();
    // (après les tableaux du mélange, déjà rangés là)
    let details: Vec<(String, dds::Image)> = zone
        .ecos
        .par_iter()
        .filter(|e| used.contains(&eco_texture_name(e)))
        .map(|e| (eco_texture_name(e), e))
        .collect::<std::collections::BTreeMap<_, _>>()
        .into_par_iter()
        .filter_map(|(name, e)| match eco_detail(game, e) {
            Ok(img) => Some((name, img)),
            Err(err) => {
                eprintln!("terrain : détail {} : {err:#}", e.name);
                None
            }
        })
        .collect();
    out.details.extend(details);
    Ok(out)
}

/// Envoie le décor chargé au renderer (textures, modèles, instances).
pub fn upload(renderer: &mut crate::render::Renderer, data: &DecorData) {
    renderer.forget_prefix("terrain/");
    renderer.decor_terrain = data.has_terrain;
    for (t, img) in &data.textures {
        renderer.upload_texture(t, img);
    }
    for (a, m) in &data.models {
        if !renderer.has_model(a) {
            renderer.upload_model(a, m);
        }
    }
    renderer.set_decor(&data.instances, data.settings.ground.unwrap_or(0.0));
    renderer.set_decor_lights(&data.lights);
    renderer.decor_visible = data.settings.visible;
    renderer.set_sky(data.sky.as_ref());
    renderer.set_fog_settings(&data.settings);
}

/// Lit `ZONE:X,Z[,RAYON]` (option `--decor` du mode capture).
pub fn parse_arg(s: &str) -> Result<DecorSettings> {
    let (zone, rest) = s.split_once(':').context("--decor ZONE:X,Z[,RAYON]")?;
    let v: Vec<f32> = rest.split(',').map(|x| x.trim().parse()).collect::<Result<_, _>>()?;
    anyhow::ensure!(v.len() == 2 || v.len() == 3, "--decor ZONE:X,Z[,RAYON]");
    let name = ZONES
        .iter()
        .map(|z| z.0)
        .find(|z| z.eq_ignore_ascii_case(zone))
        .with_context(|| trf!("carte « {zone} » inconnue", "unknown map “{zone}”"))?;
    Ok(DecorSettings { zone: name.into(), center: [v[0], v[1]], radius: v.get(2).copied().unwrap_or(250.0), ..Default::default() })
}

/// Vue d'ensemble d'une carte pour le choix du point, avec les objets (hors végétation)
/// marqués en clair pour repérer les bases.
pub struct OverviewImage {
    pub overview: forgelight::zone::Overview,
    /// RGBA8 avec les objets marqués
    pub rgba: Vec<u8>,
}

pub fn build_overview(game: &GameData, zone_name: &str) -> Result<OverviewImage> {
    let zone = Zone::parse(&game.read(&format!("{zone_name}.zone"))?)?;
    let overview = zone
        .overview(zone_name, |t| game.load_texture(t).ok())
        .with_context(|| format!("pas de vue d'ensemble pour {zone_name}"))?;
    let mut rgba = overview.rgba.clone();
    let s = overview.size as i32;
    for o in zone.objects.iter().filter(|o| !is_flora(&o.actor)) {
        let rock = o.actor.to_ascii_lowercase().contains("rock");
        if rock {
            continue;
        }
        for i in &o.instances {
            let [u, v] = overview.to_uv(i.position.x, i.position.z);
            let (px, py) = ((u * s as f32) as i32, (v * s as f32) as i32);
            if px >= 0 && py >= 0 && px < s && py < s {
                let k = ((py * s + px) * 4) as usize;
                // éclaircit vers l'orangé
                for (c, t) in rgba[k..k + 3].iter_mut().zip([255u8, 190, 90]) {
                    *c = ((*c as u32 + t as u32 * 3) / 4) as u8;
                }
            }
        }
    }
    Ok(OverviewImage { overview, rgba })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mips_are_limited() {
        let mut img = dds::Image::rgba(2048, 1024, vec![vec![0; 2048 * 1024 * 4], vec![0; 1024 * 512 * 4], vec![0; 512 * 256 * 4]]);
        limit_mips(&mut img, 1024);
        assert_eq!((img.width, img.height, img.mips.len()), (1024, 512, 2));
        assert_eq!(img.mips[0].len(), 1024 * 512 * 4);
    }

    #[test]
    fn decor_arg() {
        let s = parse_arg("indar:139,-2376,300").unwrap();
        assert_eq!((s.zone.as_str(), s.center, s.radius), ("Indar", [139.0, -2376.0], 300.0));
        assert!(parse_arg("Nulle:0,0").is_err());
        assert!(is_flora("Indar_Flora_Shrub_01.adr") && !is_flora("Indar_Props_Rocks_AngularLarge.adr"));
    }
}
