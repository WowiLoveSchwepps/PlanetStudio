//! Ciel des cartes : le dôme peint par le jeu pour chaque continent, et son brouillard.
//!
//! Chaque carte a un fichier `sky_<carte>.xml` (absent de la namelist, retrouvé par son nom),
//! sauf Amerish, dont le jeu live affiche un autre ciel (voir [`LIVE_SKIES`]).
//! - La section `<Dome>` liste, heure par heure, la texture du dôme :
//!   `<Texture Time="13:00" File="sky_Indar_Dome_1200.tga" EvComp="1.2"/>`.
//!   La texture (128 × 256) est équirectangulaire : le haut de l'image est le zénith, le milieu
//!   l'horizon, le bas le nadir ; les colonnes font le tour de l'horizon.
//! - La section `<Fog>` règle le brouillard : couleur (`Color`, par heure), densité qui décroît
//!   au-dessus d'une altitude plancher (`Distribution` : `Density` par mètre, `Gradient` par
//!   mètre d'altitude, `Floor` en mètres du jeu, par heure), distance où il commence (`Range`)
//!   et distance où le décor se fond entièrement dans le ciel (`FadeRange`).
//! - La section `<Post>` donne le voile de l'air (`<Haze Distance Strength>`), qui pâlit le dôme
//!   et le lointain vers l'horizon.
//! - La section `<SunLight>` donne la lumière principale par heure : direction (`Heading`,
//!   `Pitch` en degrés) et couleur × `Brightness`. La nuit, c'est la lune (bleutée et faible).
//! - `<Includes>` cite deux autres fichiers (partagés entre cartes : Hossin, Oshur et le
//!   tutoriel reprennent les étoiles d'Indar) :
//!   - `<carte>_starfield.xml`, section `<Satellites>` : les astres dessinés devant le dôme. Chacun
//!     est un carré texturé (case `UVs` d'un atlas, `indar_planets.dds`…) placé dans une direction
//!     (`Orientation` : `Yaw` = cap, `Pitch` = hauteur, comme le soleil, et `Roll`), de largeur
//!     `Size` (en tangente de l'angle : 0,075 ≈ 4°), teinté par `Color` × `Brightness`. `<Sun/>`
//!     marque le disque du soleil (la lune la nuit). Les fonds d'étoiles sont des carrés immenses
//!     (`Size` 3) où la texture se répète (`UScale` 8). `Depth` donne l'ordre (le plus grand,
//!     le plus loin, d'abord) ;
//!   - `<carte>_Clouds.xml`, section `<Clouds>` : une texture de densité (`DensityTexture`) dont
//!     les canaux R, G, B sont trois formes de cumulus et A un voile de stratus, mêlés selon
//!     `TextureDensityWeights`, avec leur contraste (`CumulusContrast`), leurs répétitions
//!     (`CumulusTiling`, `StratusTiling`), leur défilement (`CumulusScroll`) et leur couleur. La
//!     nuit sur Esamir, le canal B (des rubans) teinté en vert fait une aurore.

use anyhow::{Context, Result};
use forgelight::{dds, GameData};

/// Heure du ciel par défaut (début d'après-midi, plein jour sur toutes les cartes).
pub const DEFAULT_HOUR: f32 = 13.0;

/// Carte dont on prend le ciel quand la carte demandée n'en a pas.
const FALLBACK_ZONE: &str = "Indar";

/// Ciels que le jeu live met sur une carte à la place de `sky_<carte>.xml` : (carte, hash du
/// fichier dans les archives, décalage horaire).
///
/// Amerish : la table des terrains (`ObjectTerrainData`) cite toujours `sky_amerish.xml`, un
/// ancien ciel (dômes d'Indar, brouillard gris-blanc dense, étalonnage neutre), mais le jeu live
/// affiche celui d'un fichier de `data_x64_0.pack2` au nom inconnu, identique à
/// `sky_searhus.xml` : dômes `sky_amerish_dome_*`, étoiles et lune de `amerish_starfield.xml`,
/// étalonnage `amerish_colorkey_*` (sarcelle la nuit), pas de brouillard le jour et un voile
/// léger. Les captures de nuit du jeu (lune bleutée, rouge écrasé) n'existent qu'avec lui. Son
/// cycle est décalé de 12 h : la lune est haute à 12 h du fichier, le soleil à 0 h ; l'heure du
/// studio (13 h = plein jour) y est donc avancée de 12 h.
///
/// Son ciel de nuit (dôme, nuages éclairés, lune et étoiles) sortait bien plus lumineux que sur
/// les captures du jeu live, alors que le sol éclairé par la lune leur correspondait : la couche
/// du ciel est assombrie la nuit (voir [`night_sky_scale`]).
const LIVE_SKIES: &[LiveSky] = &[LiveSky {
    zone: "Amerish",
    hash: 0x3490_e7b6_cf24_9cf5,
    offset: 12.0,
    night_dome: 0.1,
    night_horizon: 0.5,
    night_astres: 0.4,
}];

/// Ciel du jeu live (voir [`LIVE_SKIES`]).
struct LiveSky {
    zone: &'static str,
    /// hash du nom du fichier dans les archives (nom inconnu)
    hash: u64,
    /// heures ajoutées à celle du studio pour lire le fichier
    offset: f32,
    /// luminosité en pleine nuit (1 le jour) du dôme, de son horizon (brouillard, voile) et des
    /// nuages, puis des astres (lune, étoiles, planètes). Calées sur des captures du jeu live,
    /// lune haute : le haut du ciel n'y est que le noir relevé en sarcelle par
    /// `amerish_colorkey_night` (le dôme n'y compte presque pas), les nuages restent discrets et
    /// la lune claire. Écarts restants : ciel (3, 42, 76) contre (0, 35, 67), nuages
    /// (4, 44, 81) contre (5, 46, 85), lune (55, 170, 206) contre (70, 161, 194).
    night_dome: f32,
    /// comme `night_dome`, mais pour le bas du dôme (horizon), plus clair dans le jeu : lueur
    /// sarcelle au-dessus des crêtes, que le brouillard lointain rejoint. Le dôme passe de l'un à
    /// l'autre entre l'horizon et 37° de hauteur ; au-dessus des crêtes, (9, 54, 94) contre
    /// (12, 57, 83) sur une capture du jeu
    night_horizon: f32,
    night_astres: f32,
}

/// Facteur de luminosité de la couche du ciel (dôme, nuages, astres) à une heure : `night` en
/// pleine nuit (soleil le plus faible du fichier), 1 au soleil de midi, proportionnel entre les
/// deux (aube, coucher).
fn night_sky_scale(xml: &str, hour: f32, noon_hour: f32, night: f32) -> f32 {
    if night >= 1.0 {
        return 1.0;
    }
    let (Some(now), Some(noon)) = (sun_at(xml, hour), sun_at(xml, noon_hour)) else { return 1.0 };
    let low = keyed(section(xml, "SunLight"), "Color", &["Brightness"])
        .iter()
        .map(|(_, v)| v[0] / SUN_REFERENCE)
        .fold(f32::MAX, f32::min);
    if noon.intensity <= low + 1e-3 {
        return 1.0;
    }
    let t = ((now.intensity - low) / (noon.intensity - low)).clamp(0.0, 1.0);
    night + (1.0 - night) * t
}

/// Brouillard d'une carte à une heure donnée.
#[derive(Clone, Debug, PartialEq)]
pub struct Fog {
    /// couleur (linéaire)
    pub color: [f32; 3],
    /// densité par mètre au plancher et en dessous
    pub density: f32,
    /// décroissance exponentielle de la densité par mètre au-dessus du plancher
    pub gradient: f32,
    /// altitude du plancher (mètres du jeu)
    pub floor: f32,
    /// distance où le brouillard commence (m)
    pub start: f32,
    /// le décor se fond dans le ciel entre ces deux distances (m)
    pub fade: [f32; 2],
    /// voile de l'air (`<Post><Haze>`) : force et distance caractéristique (m). Il pâlit le ciel
    /// bleu du dôme et les reliefs lointains vers l'horizon : 0,5 sur Indar (39 % du ciel voilé),
    /// 1,5 sur Esamir et Nexus (78 %). Force nulle (ou distance nulle, Koltyr) : pas de voile.
    pub haze: [f32; 2],
}

/// Luminosité du soleil du jeu qui correspond à l'intensité 1 du studio (plein jour ≈ 12 à 14).
const SUN_REFERENCE: f32 = 12.0;

/// Lumière principale d'une carte à une heure donnée (soleil ou lune).
#[derive(Clone, Debug, PartialEq)]
pub struct Sun {
    /// cap (degrés) et hauteur au-dessus de l'horizon (degrés, négative quand il est couché)
    pub heading: f32,
    pub pitch: f32,
    /// couleur (0..1, comme le sélecteur de couleur)
    pub color: [f32; 3],
    /// luminosité ramenée à l'échelle du studio (1 = plein jour)
    pub intensity: f32,
}

impl Sun {
    /// Réglage de soleil du studio correspondant.
    pub fn state(&self) -> crate::anim::SunState {
        crate::anim::SunState {
            azimuth: (self.heading + 180.0).rem_euclid(360.0) - 180.0,
            elevation: self.pitch.clamp(1.0, 90.0),
            color: self.color,
            intensity: self.intensity,
        }
    }

    /// Intensité d'ambiance qui va avec : sombre la nuit, normale le jour.
    pub fn ambient(&self) -> f32 {
        self.intensity.clamp(0.2, 1.2)
    }
}

/// Astre (`<Satellite>`) à une heure donnée.
#[derive(Clone, Debug, PartialEq)]
pub struct Satellite {
    pub texture: String,
    /// case de la texture : décalage u, v, puis échelle u, v (plus de 1 : la texture se répète)
    pub uv: [f32; 4],
    /// ordre de dessin : le plus grand d'abord
    pub depth: f32,
    /// cap, hauteur au-dessus de l'horizon et rotation sur lui-même (degrés)
    pub heading: f32,
    pub pitch: f32,
    pub roll: f32,
    /// largeur du carré, en tangente de l'angle
    pub size: f32,
    /// couleur (linéaire) × luminosité, à l'échelle du dôme
    pub color: [f32; 3],
    /// disque du soleil (ou de la lune) : suit la direction de la lumière du studio
    pub sun: bool,
}

impl Satellite {
    /// Fond d'étoiles (texture répétée) : ajouté à la lumière du ciel au lieu de le couvrir.
    pub fn additive(&self) -> bool {
        self.uv[2] > 1.0 || self.uv[3] > 1.0
    }
}

/// Couche de nuages (`<Clouds>`) à une heure donnée.
#[derive(Clone, Debug, PartialEq)]
pub struct Clouds {
    pub texture: String,
    /// poids des canaux : cumulus R, G, B et stratus A
    pub weights: [f32; 4],
    /// couleur (linéaire)
    pub color: [f32; 3],
    /// exposant appliqué aux cumulus (plus grand : nuages plus nets, plus rares)
    pub contrast: f32,
    /// répétitions des cumulus et des stratus
    pub tiling: [f32; 2],
    /// défilement (répétitions par seconde)
    pub scroll: [f32; 2],
}

/// Ciel d'une carte à une heure donnée.
pub struct Sky {
    /// texture du dôme
    pub file: String,
    pub image: dds::Image,
    pub fog: Option<Fog>,
    pub sun: Option<Sun>,
    /// astres, du plus lointain au plus proche
    pub satellites: Vec<Satellite>,
    pub clouds: Option<Clouds>,
    /// textures des astres et des nuages
    pub textures: Vec<(String, dds::Image)>,
    /// exposition et étalonnage du jeu à cette heure
    pub grading: Option<Grading>,
    /// luminosité du dôme à cette heure (`EvComp` du jeu, 1 = texture telle quelle)
    pub dome_exposure: f32,
    /// assombrissement de l'air la nuit (1 le jour, voir [`night_sky_scale`]) : il vaut aussi pour
    /// les rayons de la lune, qui faisaient sinon une plaque claire au bord de la carte d'ombre
    pub night_air: f32,
    /// luminosité du bas du dôme relativement au zénith (1 le jour) : la nuit sur Amerish,
    /// l'horizon est assombri moins que le zénith (voir `LiveSky::night_horizon`)
    pub horizon_lift: f32,
}

/// Correction d'exposition du dôme (`EvComp`, en IL) qui correspond à sa luminosité d'origine
/// dans le studio. Le jeu éclaircit ou assombrit le dôme selon l'heure : −2 IL à 7 h (le dôme de
/// midi en ciel du matin), +1,2 IL à 13 h, +2 IL à 21 h (le dôme rose du coucher, lumineux).
/// À 0,9, le ciel de plein jour (×1,23) et celui du coucher (×2,1) se rapprochent des captures
/// du jeu sans trop éclaircir le brouillard, qui prend la couleur de l'horizon du dôme.
const DOME_EV_REFERENCE: f32 = 0.9;

/// Facteur de luminosité du dôme à une heure : `EvComp` interpolé entre les clés de `<Dome>`.
///
/// Les ciels d'Esamir et de Nexus (plus récents) ont une autre échelle : 5,5 et 4,3 IL à 13 h,
/// contre 1,2 pour tous les autres, alors que leurs dômes ne sont pas plus sombres (horizon de
/// midi d'Esamir : luminance 0,27, Indar 0,45). Pris tels quels, le ciel d'Esamir sortait ×24
/// (Indar ×1,23) et le brouillard et le voile, qui prennent la couleur de son horizon, noyaient
/// le décor dans le blanc. L'échelle de ces cartes est donc décalée pour que leur midi tombe sur
/// celui d'Indar ; les écarts entre heures (aube, coucher, nuit) restent ceux du jeu.
#[cfg(test)]
fn dome_exposure_at(xml: &str, hour: f32) -> f32 {
    dome_exposure_with_noon(xml, hour, DEFAULT_HOUR)
}

/// Comme [`dome_exposure_at`], avec le midi à une autre heure du fichier (ciel décalé).
fn dome_exposure_with_noon(xml: &str, hour: f32, noon_hour: f32) -> f32 {
    let keys = keyed(section(xml, "Dome"), "Texture", &["EvComp"]);
    let Some(ev) = interp(&keys, hour).map(|v| v[0]) else { return 1.0 };
    let noon = interp(&keys, noon_hour).map_or(NOON_EV, |v| v[0]);
    2f32.powf(ev - (noon - NOON_EV).max(0.0) - DOME_EV_REFERENCE)
}

/// `EvComp` de midi des ciels d'Indar (et d'Amerish, Hossin, Oshur, VR, Koltyr).
const NOON_EV: f32 = 1.2;

/// Côté d'une table d'étalonnage du jeu (`colorkey`) : 16 tranches de 16 × 16.
pub const LUT_SIZE: usize = 16;

/// Exposition et étalonnage du jeu (`<Post>`) à une heure donnée.
#[derive(Clone, Debug, PartialEq)]
pub struct Grading {
    /// point blanc du rendu des tons (`<Exposure WhitePoint>`) : 11,3 en plein jour sur Indar,
    /// 4,8 à l'aube et au coucher, 4,5 la nuit
    pub white_point: f32,
    /// les deux tables qui encadrent l'heure (RGBA, 256 × 16 : x = tranche de bleu × 16 + rouge,
    /// y = vert) et la part de la seconde
    pub luts: Option<(Vec<u8>, Vec<u8>, f32)>,
}

/// Point blanc de `<Post><Exposure>` à une heure donnée.
pub fn white_point_at(xml: &str, hour: f32) -> Option<f32> {
    interp(&keyed(section(xml, "Post"), "Exposure", &["WhitePoint"]), hour).map(|v| v[0].max(0.1))
}

/// Tables de `<ColorGrading>` qui encadrent l'heure : (avant, après, part de « après »).
/// Une seule table : elle vaut pour toute la journée.
pub fn color_grading_at(xml: &str, hour: f32) -> Option<(String, String, f32)> {
    let mut keys: Vec<(f32, String)> = tags(section(xml, "ColorGrading"), "Texture")
        .into_iter()
        .filter_map(|t| Some((attr(t, "Time").map_or(Some(0.0), parse_hour)?, attr(t, "File")?.to_string())))
        .collect();
    keys.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (first, last) = (keys.first()?, keys.last()?);
    let hour = hour.rem_euclid(24.0);
    let (a, b, ha, hb) = match keys.iter().position(|k| k.0 > hour) {
        Some(0) => (last, first, last.0 - 24.0, first.0),
        Some(i) => (&keys[i - 1], &keys[i], keys[i - 1].0, keys[i].0),
        None => (last, first, last.0, first.0 + 24.0),
    };
    let t = if hb > ha { ((hour - ha) / (hb - ha)).clamp(0.0, 1.0) } else { 0.0 };
    Some((a.1.clone(), b.1.clone(), t))
}

/// Table d'étalonnage du jeu en RGBA (256 × 16), `None` si elle n'a pas ce format.
fn load_lut(game: &GameData, name: &str) -> Option<Vec<u8>> {
    let img = game.load_texture(name).map_err(|e| eprintln!("étalonnage : {e:#}")).ok()?;
    let n = LUT_SIZE * LUT_SIZE;
    (img.width as usize == n && img.height as usize == LUT_SIZE && img.mips[0].len() == n * LUT_SIZE * 4)
        .then(|| img.mips[0].clone())
}

/// Exposition et étalonnage d'une carte à une heure (`None` si la carte n'en donne pas).
fn grading_at(game: &GameData, xml: &str, hour: f32) -> Option<Grading> {
    let white_point = white_point_at(xml, hour)?;
    let luts = color_grading_at(xml, hour).and_then(|(a, b, t)| {
        let la = load_lut(game, &a)?;
        let lb = if b.eq_ignore_ascii_case(&a) { la.clone() } else { load_lut(game, &b)? };
        Some((la, lb, t))
    });
    Some(Grading { white_point, luts })
}

/// Contenu de la section `<nom>…</nom>` (vide si absente).
fn section<'a>(xml: &'a str, name: &str) -> &'a str {
    let Some(start) = xml.find(&format!("<{name}>")) else { return "" };
    let end = xml[start..].find(&format!("</{name}>")).map(|e| start + e).unwrap_or(xml.len());
    &xml[start..end]
}

/// Balises `<tag …>` d'un extrait (le texte de chaque balise, sans le nom).
fn tags<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let open = format!("<{tag}");
    xml.split(open.as_str())
        .skip(1)
        // `<Color` ne doit pas prendre `<ColorGrading`
        .filter(|t| t.starts_with(|c: char| c.is_whitespace() || c == '/' || c == '>'))
        .map(|t| &t[..t.find('>').unwrap_or(t.len())])
        .collect()
}

/// Valeur d'un attribut `nom="valeur"` dans une balise.
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let pat = format!("{name}=\"");
    let mut from = 0;
    while let Some(k) = tag[from..].find(&pat) {
        let i = from + k;
        // nom entier : `R` ne doit pas être la fin de `Floor`
        if tag[..i].ends_with(char::is_whitespace) {
            let v = i + pat.len();
            let j = tag[v..].find('"')?;
            return Some(&tag[v..v + j]);
        }
        from = i + pat.len();
    }
    None
}

/// « 13:00 » → 13,0 ; « 22:30 » → 22,5.
fn parse_hour(s: &str) -> Option<f32> {
    let (h, m) = s.split_once(':').unwrap_or((s, "0"));
    Some(h.trim().parse::<f32>().ok()? + m.trim().parse::<f32>().ok()? / 60.0)
}

/// Valeurs numériques d'une balise répétée par heure : (heure, valeurs), triées par heure.
/// Une balise sans `Time` vaut pour toute la journée.
fn keyed(xml: &str, tag: &str, names: &[&str]) -> Vec<(f32, Vec<f32>)> {
    let mut out: Vec<(f32, Vec<f32>)> = tags(xml, tag)
        .into_iter()
        .filter_map(|t| {
            let hour = attr(t, "Time").map_or(Some(0.0), parse_hour)?;
            let v = names.iter().map(|n| attr(t, n)?.trim().parse().ok()).collect::<Option<Vec<f32>>>()?;
            Some((hour, v))
        })
        .collect();
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out
}

/// Interpolation linéaire entre les clés qui encadrent l'heure, en bouclant sur 24 h.
fn interp(keys: &[(f32, Vec<f32>)], hour: f32) -> Option<Vec<f32>> {
    interp_with(keys, hour, false)
}

/// `angle` : le premier nombre est un angle en degrés, interpolé par le plus court chemin.
fn interp_with(keys: &[(f32, Vec<f32>)], hour: f32, angle: bool) -> Option<Vec<f32>> {
    let hour = hour.rem_euclid(24.0);
    let (first, last) = (keys.first()?, keys.last()?);
    let next = keys.iter().position(|k| k.0 > hour);
    let (a, b, ha, hb) = match next {
        Some(0) => (last, first, last.0 - 24.0, first.0),
        Some(i) => (&keys[i - 1], &keys[i], keys[i - 1].0, keys[i].0),
        None => (last, first, last.0, first.0 + 24.0),
    };
    let t = if hb > ha { ((hour - ha) / (hb - ha)).clamp(0.0, 1.0) } else { 0.0 };
    Some(
        a.1.iter()
            .zip(&b.1)
            .enumerate()
            .map(|(i, (x, y))| {
                let d = if angle && i == 0 { (y - x + 180.0).rem_euclid(360.0) - 180.0 } else { y - x };
                x + d * t
            })
            .collect(),
    )
}

/// Composante sRGB de 0 à 255 → linéaire de 0 à 1.
pub fn srgb_to_linear(c: f32) -> f32 {
    let c = (c / 255.0).clamp(0.0, 1.0);
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Brouillard de la section `<Fog>` à une heure donnée (`None` sans densité).
pub fn fog_at(xml: &str, hour: f32) -> Option<Fog> {
    let fog = section(xml, "Fog");
    let dist = interp(&keyed(fog, "Distribution", &["Density", "Gradient", "Floor"]), hour)?;
    let color = interp(&keyed(fog, "Color", &["R", "G", "B"]), hour).unwrap_or(vec![200.0, 200.0, 200.0]);
    let range = interp(&keyed(fog, "Range", &["Near", "Far"]), hour).unwrap_or(vec![0.0, 0.0]);
    let fade = interp(&keyed(fog, "FadeRange", &["Near", "Far"]), hour).unwrap_or(vec![2000.0, 3000.0]);
    // `<Haze>` de `<Post>` (celui de `<Lens>`, sans `Distance`, ne compte pas)
    let haze = interp(&keyed(section(xml, "Post"), "Haze", &["Strength", "Distance"]), hour).unwrap_or(vec![0.0, 1000.0]);
    Some(Fog {
        color: [srgb_to_linear(color[0]), srgb_to_linear(color[1]), srgb_to_linear(color[2])],
        density: dist[0].max(0.0),
        gradient: dist[1].max(0.0),
        floor: dist[2],
        start: range[0].max(0.0),
        fade: [fade[0].min(fade[1]), fade[1]],
        haze: if haze[1] > 0.0 { [haze[0].max(0.0), haze[1]] } else { [0.0, 1000.0] },
    })
}

/// Soleil de la section `<SunLight>` à une heure donnée.
pub fn sun_at(xml: &str, hour: f32) -> Option<Sun> {
    let sl = section(xml, "SunLight");
    let dir = interp_angles(&keyed(sl, "Direction", &["Heading", "Pitch"]), hour)?;
    let c = interp(&keyed(sl, "Color", &["R", "G", "B", "Brightness"]), hour)?;
    Some(Sun {
        heading: dir[0],
        pitch: dir[1],
        color: [c[0], c[1], c[2]].map(|v| (v / 255.0).clamp(0.0, 1.0)),
        intensity: (c[3] / SUN_REFERENCE).max(0.0),
    })
}

/// Comme [`interp`], mais le premier nombre est un cap : il tourne par le plus court chemin
/// (de 182° à 140°, il recule de 42°, il ne fait pas le tour).
fn interp_angles(keys: &[(f32, Vec<f32>)], hour: f32) -> Option<Vec<f32>> {
    interp_with(keys, hour, true)
}

/// Luminosité d'un astre qui correspond à la couleur telle quelle sur le dôme (le soleil et les
/// planètes valent 14 à 24 le jour, les étoiles 12 la nuit et 2 le jour).
const SATELLITE_REFERENCE: f32 = 12.0;

/// Fichiers cités par `<Includes>`.
pub fn includes(xml: &str) -> Vec<String> {
    tags(section(xml, "Includes"), "Include").into_iter().filter_map(|t| attr(t, "File").map(str::to_string)).collect()
}

/// Astres de la section `<Satellites>` à une heure donnée, du plus lointain au plus proche.
pub fn satellites_at(xml: &str, hour: f32) -> Vec<Satellite> {
    let mut out: Vec<Satellite> = section(xml, "Satellites")
        .split("<Satellite>")
        .skip(1)
        .filter_map(|block| {
            let block = &block[..block.find("</Satellite>").unwrap_or(block.len())];
            let texture = tags(block, "Texture").first().and_then(|t| attr(t, "File"))?.to_string();
            let uvs = tags(block, "UVs")
                .first()
                .map(|t| ["UOffset", "VOffset", "UScale", "VScale"].map(|n| attr(t, n).and_then(|v| v.trim().parse().ok())));
            let uv = match uvs {
                Some([Some(a), Some(b), Some(c), Some(d)]) => [a, b, c, d],
                _ => [0.0, 0.0, 1.0, 1.0],
            };
            let one = |tag: &str, name: &str| interp(&keyed(block, tag, &[name]), hour).map(|v| v[0]);
            let o = interp_angles(&keyed(block, "Orientation", &["Yaw", "Pitch", "Roll"]), hour).unwrap_or(vec![0.0; 3]);
            let c = interp(&keyed(block, "Color", &["Red", "Green", "Blue"]), hour).unwrap_or(vec![255.0; 3]);
            let mut b = one("Brightness", "Value").unwrap_or(SATELLITE_REFERENCE) / SATELLITE_REFERENCE;
            // étoiles : le jeu les noie de jour dans l'exposition du ciel clair ; ici, le dôme est
            // affiché tel quel, donc on les éteint plus franchement (2 / 12 → 3 %)
            if uv[2] > 1.0 || uv[3] > 1.0 {
                b *= b;
            }
            Some(Satellite {
                texture,
                uv,
                depth: one("Depth", "Value").unwrap_or(0.0),
                heading: o[0],
                pitch: o[1],
                roll: o[2],
                size: one("Size", "Value").unwrap_or(0.1).max(0.0),
                color: [c[0], c[1], c[2]].map(|v| srgb_to_linear(v) * b.max(0.0)),
                sun: block.contains("<Sun/>") || block.contains("<Sun />"),
            })
        })
        .collect();
    // fonds d'étoiles toujours derrière les autres astres : Amerish et Nexus leur donnent
    // `Depth` −2 (5 sur Indar et Esamir), ce qui les aurait dessinés par-dessus la lune
    out.sort_by(|a, b| b.additive().cmp(&a.additive()).then(b.depth.total_cmp(&a.depth)));
    out
}

/// Couche de nuages de la section `<Clouds>` à une heure donnée (`None` sans texture ou sans
/// nuage à cette heure).
pub fn clouds_at(xml: &str, hour: f32) -> Option<Clouds> {
    let c = section(xml, "Clouds");
    let texture = tags(c, "DensityTexture").first().and_then(|t| attr(t, "File"))?.to_string();
    let w = interp(&keyed(c, "TextureDensityWeights", &["R-Cumulus1", "G-Cumulus2", "B-Cumulus3", "A-Stratus"]), hour)?;
    if w.iter().all(|v| *v <= 0.0) {
        return None;
    }
    let one = |tag: &str, name: &str, d: f32| interp(&keyed(c, tag, &[name]), hour).map_or(d, |v| v[0]);
    let col = interp(&keyed(c, "Color", &["R", "G", "B"]), hour).unwrap_or(vec![160.0; 3]);
    let scroll = interp(&keyed(c, "CumulusScroll", &["U", "V"]), hour).unwrap_or(vec![0.0, 0.0]);
    Some(Clouds {
        texture,
        weights: [w[0], w[1], w[2], w[3]].map(|v| v.max(0.0)),
        color: [col[0], col[1], col[2]].map(srgb_to_linear),
        contrast: one("CumulusContrast", "Power", 1.0).max(0.1),
        tiling: [one("CumulusTiling", "Frequency", 3.0), one("StratusTiling", "Frequency", 1.0)],
        scroll: [scroll[0], scroll[1]],
    })
}

/// Textures du dôme d'une carte : (heure, fichier), triées par heure.
pub fn dome_schedule(xml: &str) -> Vec<(f32, String)> {
    let mut out: Vec<(f32, String)> = tags(section(xml, "Dome"), "Texture")
        .into_iter()
        .filter_map(|t| Some((parse_hour(attr(t, "Time")?)?, attr(t, "File")?.to_string())))
        .collect();
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out
}

/// Texture en vigueur à une heure : la dernière commencée (celle de la veille avant la première).
pub fn dome_at(schedule: &[(f32, String)], hour: f32) -> Option<&str> {
    let hour = hour.rem_euclid(24.0);
    schedule.iter().rev().find(|(h, _)| *h <= hour).or(schedule.last()).map(|(_, f)| f.as_str())
}

/// Lit le ciel d'une carte à une heure donnée (ciel d'Indar si la carte n'a pas le sien).
pub fn load(game: &GameData, zone: &str, hour: f32) -> Result<Sky> {
    let (xml, offset, [night_dome, night_horizon, night_astres]) = sky_xml(game, zone).with_context(|| format!("pas de ciel pour {zone}"))?;
    let hour = (hour + offset).rem_euclid(24.0);
    let noon = (DEFAULT_HOUR + offset).rem_euclid(24.0);
    let file = dome_at(&dome_schedule(&xml), hour).with_context(|| format!("pas de dôme pour {zone}"))?.to_string();
    let image = game.load_texture(&file).with_context(|| format!("ciel de {zone}"))?;
    // astres et nuages : dans les fichiers inclus
    let extra: String = includes(&xml)
        .iter()
        .filter_map(|f| game.read(f).ok())
        .map(|d| String::from_utf8_lossy(&d).into_owned())
        .collect::<Vec<_>>()
        .join("\n");
    let mut satellites = satellites_at(&extra, hour);
    let clouds = clouds_at(&extra, hour);
    let mut textures: Vec<(String, dds::Image)> = Vec::new();
    let names: Vec<String> = satellites.iter().map(|s| s.texture.clone()).chain(clouds.iter().map(|c| c.texture.clone())).collect();
    for name in names {
        if textures.iter().any(|(n, _)| n.eq_ignore_ascii_case(&name)) {
            continue;
        }
        match game.load_texture_gpu(&name) {
            Ok(img) => textures.push((name, img)),
            Err(e) => eprintln!("ciel de {zone} : {e:#}"),
        }
    }
    let loaded = |t: &str| textures.iter().any(|(n, _)| n.eq_ignore_ascii_case(t));
    satellites.retain(|s| loaded(&s.texture));
    let mut clouds = clouds.filter(|c| loaded(&c.texture));
    let grading = grading_at(game, &xml, hour);
    // ciel de nuit assombri : dôme et son horizon (brouillard, voile), nuages ; astres à part
    let k = night_sky_scale(&xml, hour, noon, night_dome);
    let ks = night_sky_scale(&xml, hour, noon, night_astres);
    let horizon_lift = night_sky_scale(&xml, hour, noon, night_horizon) / k.max(1e-4);
    for s in &mut satellites {
        s.color = s.color.map(|c| c * ks);
    }
    if let Some(c) = &mut clouds {
        c.color = c.color.map(|v| v * k);
    }
    let dome_exposure = dome_exposure_with_noon(&xml, hour, noon) * k;
    Ok(Sky { file, image, fog: fog_at(&xml, hour), sun: sun_at(&xml, hour), satellites, clouds, textures, grading, dome_exposure, night_air: k, horizon_lift })
}

/// Fichier de ciel d'une carte, son décalage horaire et sa luminosité de nuit (voir
/// [`LIVE_SKIES`]) ; ciel d'Indar si
/// la carte n'a pas le sien.
/// Balancement des plantes de la carte : `<Wind><Sway …/>` de son ciel (« Flora Wind » ; le
/// `<Wind X Y Z Scale>` qui l'entoure est le vent des particules). Le shader du jeu n'est pas
/// dans les archives : le studio s'en sert ainsi (voir `sway_offset` dans `render.rs`) :
/// des fronts de rafales espacés de `spacing` m avancent à `speed` m/s et penchent les plantes,
/// `smoothing` adoucit (> 1) ou durcit (< 1) les rafales, `boil` fait frémir les feuilles et
/// `strength` règle l'amplitude. Indar, Amerish, Hossin : 90 m, 14 m/s, force 1,09 ; Esamir :
/// 150 m, 30 m/s, force 1 ; VR : frémissement 0,2.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sway {
    pub spacing: f32,
    pub speed: f32,
    pub boil: f32,
    pub smoothing: f32,
    pub strength: f32,
}

impl Default for Sway {
    /// valeurs d'Indar, d'Amerish et d'Hossin
    fn default() -> Self {
        Self { spacing: 90.0, speed: 14.0, boil: 0.049, smoothing: 1.481, strength: 1.091 }
    }
}

/// Lit `<Sway>` dans le texte d'un ciel.
fn parse_sway(xml: &str) -> Option<Sway> {
    let tag = *tags(xml, "Sway").first()?;
    // (`Speed ="16"` dans le ciel de la VR : espace avant le signe égal)
    let tag = tag.replace(" =", "=");
    let f = |n: &str, d: f32| attr(&tag, n).and_then(|v| v.trim().parse().ok()).unwrap_or(d);
    let d = Sway::default();
    Some(Sway {
        spacing: f("Spacing", d.spacing),
        speed: f("Speed", d.speed),
        boil: f("Boil", d.boil),
        smoothing: f("Smoothing", d.smoothing),
        strength: f("Strength", d.strength),
    })
}

/// Balancement des plantes d'une carte (celui d'Indar si son ciel n'en donne pas).
pub fn sway(game: &GameData, zone: &str) -> Sway {
    sky_xml(game, zone).and_then(|(xml, ..)| parse_sway(&xml)).unwrap_or_default()
}

fn sky_xml(game: &GameData, zone: &str) -> Option<(String, f32, [f32; 3])> {
    let text = |d: Vec<u8>| String::from_utf8_lossy(&d).into_owned();
    let valid = |x: &String| !dome_schedule(x).is_empty();
    let live = LIVE_SKIES.iter().filter(|l| l.zone.eq_ignore_ascii_case(zone)).find_map(|l| {
        let e = game.index.entries.get(&l.hash)?;
        Some((text(game.index.read_entry(e).ok()?), l.offset, [l.night_dome, l.night_horizon, l.night_astres])).filter(|(x, ..)| valid(x))
    });
    let named = |z: &str| game.read(&format!("sky_{z}.xml")).ok().map(text).filter(valid).map(|x| (x, 0.0, [1.0; 3]));
    live.or_else(|| named(zone)).or_else(|| named(FALLBACK_ZONE))
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<Sky><Post><Texture File="colorkey.tga" Time="0:00"/>
		<Haze Distance="1075" Strength="0.500000"/>
		<ColorGrading><Texture File="x.tga" Time="0:00"/></ColorGrading></Post>
	<Lens Enabled="true"><Haze SunStr=".03" SkyStr="0.008"/></Lens>
	<Dome>
		<Texture Time="0:00" File="sky_Indar_Dome_00.tga" EvComp="-0.500000"/>
		<Texture Time="4:00" File="sky_Indar_Dome_600.tga" EvComp="-0.000000"/>
		<Texture Time="7:00" File="sky_Indar_Dome_1200.tga" EvComp="-2.000000"/>
		<Texture Time="21:00" File="sky_Indar_Dome_1600.tga" EvComp="2.000000"/>
		<Texture Time="22:30" File="sky_Indar_Dome_00.tga" EvComp="-0.500000"/>
	</Dome>
	<Fog>
	    <Color Time="13:00" R="240" G="120" B="50" />
	    <Distribution Time="0:00" Density="0.0015" Gradient="0.004" Floor="200" />
	    <Distribution Time="7:00" Density="0.00085" Gradient="0.004" Floor="50" />
	    <Distribution Time="19:00" Density="0.00085" Gradient="0.004" Floor="50" />
	    <Distribution Time="23:00" Density="0.0015" Gradient="0.004" Floor="200" />
	    <Range Near="100" Far="900" />
	    <FadeRange Near="2200" Far="2500" />
	</Fog></Sky>"#;

    #[test]
    fn schedule() {
        let s = dome_schedule(XML);
        assert_eq!(s.len(), 5, "les textures hors <Dome> sont ignorées");
        assert_eq!(s[4], (22.5, "sky_Indar_Dome_00.tga".into()));
        assert_eq!(dome_at(&s, 13.0), Some("sky_Indar_Dome_1200.tga"));
        assert_eq!(dome_at(&s, 4.0), Some("sky_Indar_Dome_600.tga"));
        assert_eq!(dome_at(&s, 21.5), Some("sky_Indar_Dome_1600.tga"));
        assert_eq!(dome_at(&s, 23.9), Some("sky_Indar_Dome_00.tga"));
        assert_eq!(dome_at(&s, 24.0 + 13.0), Some("sky_Indar_Dome_1200.tga"));
        assert_eq!(dome_at(&[], 12.0), None);
        // EvComp interpolé : 21 h → +2 IL ; 14 h (entre 7 h, −2, et 21 h, +2) → 0 IL
        assert!((dome_exposure_at(XML, 21.0) - 2f32.powf(2.0 - DOME_EV_REFERENCE)).abs() < 1e-4);
        assert!((dome_exposure_at(XML, 14.0) - 2f32.powf(-DOME_EV_REFERENCE)).abs() < 1e-4);
        assert_eq!(dome_exposure_at("<Sky/>", 12.0), 1.0);
        // ciel décalé (Amerish live) : le midi du studio est pris à une autre heure du fichier
        let shifted = r#"<Dome><Texture Time="1:00" File="a.tga" EvComp="4.2"/><Texture Time="13:00" File="b.tga" EvComp="0"/></Dome>"#;
        assert!((dome_exposure_with_noon(shifted, 1.0, 1.0) - 2f32.powf(1.2 - DOME_EV_REFERENCE)).abs() < 1e-4);
        assert!((dome_exposure_with_noon(shifted, 13.0, 1.0) - 2f32.powf(-3.0 - DOME_EV_REFERENCE)).abs() < 1e-4);
        // échelle d'Esamir (5,5 IL à 13 h) : ramenée à celle d'Indar, écarts entre heures gardés
        let esamir = r#"<Dome><Texture Time="0:00" EvComp="2"/><Texture Time="13:00" EvComp="5.5"/></Dome>"#;
        assert!((dome_exposure_at(esamir, 13.0) - 2f32.powf(1.2 - DOME_EV_REFERENCE)).abs() < 1e-4);
        assert!((dome_exposure_at(esamir, 0.0) - 2f32.powf(1.2 - 3.5 - DOME_EV_REFERENCE)).abs() < 1e-4);
    }

    #[test]
    fn schedule_unsorted_starts_late() {
        // la première texture commence après minuit : avant elle, c'est celle de la veille
        let s = dome_schedule(r#"<Dome><Texture Time="6:00" File="a.tga"/><Texture Time="20:00" File="b.tga"/></Dome>"#);
        assert_eq!(dome_at(&s, 2.0), Some("b.tga"));
        assert_eq!(dome_at(&s, 6.0), Some("a.tga"));
    }

    #[test]
    fn exposure_and_grading() {
        let xml = r#"<Post>
            <Exposure Time="0:00" WhitePoint="4.5"/>
            <Exposure Time="13:00" WhitePoint="11.3"/>
            <Exposure Time="19:00" WhitePoint="11.3"/>
            <Exposure Time="21:00" WhitePoint="4.8"/>
            <ColorGrading>
                <Texture File="colorkey_identity.tga" Time="4:00"/>
                <Texture File="colorkey_mildcontrast.tga" Time="8:00"/>
                <Texture File="colorkey_mildcontrast.tga" Time="19:30"/>
                <Texture File="colorkey_identity.tga" Time="20:00"/>
            </ColorGrading></Post>"#;
        assert_eq!(white_point_at(xml, 13.0), Some(11.3));
        assert!((white_point_at(xml, 20.0).unwrap() - 8.05).abs() < 1e-4);
        assert_eq!(white_point_at("<Sky/>", 12.0), None);
        // 19 h 45 : à mi-chemin entre le contraste léger et l'identité
        let (a, b, t) = color_grading_at(xml, 19.75).unwrap();
        assert_eq!((a.as_str(), b.as_str()), ("colorkey_mildcontrast.tga", "colorkey_identity.tga"));
        assert!((t - 0.5).abs() < 1e-4);
        // minuit : entre 20 h (veille) et 4 h, identité des deux côtés
        let (a, b, _) = color_grading_at(xml, 0.0).unwrap();
        assert_eq!((a.as_str(), b.as_str()), ("colorkey_identity.tga", "colorkey_identity.tga"));
        assert_eq!(color_grading_at(xml, 12.0).unwrap().0, "colorkey_mildcontrast.tga");
    }

    #[test]
    fn fog() {
        let f = fog_at(XML, 13.0).unwrap();
        assert!((f.density - 0.00085).abs() < 1e-7 && f.floor == 50.0 && f.gradient == 0.004);
        assert_eq!((f.start, f.fade), (100.0, [2200.0, 2500.0]));
        assert!(f.color[0] > f.color[1] && f.color[1] > f.color[2], "{:?}", f.color);
        assert_eq!(f.haze, [0.5, 1075.0], "voile de <Post>, pas celui de <Lens>");
        // 21 h : à mi-chemin entre 19 h et 23 h
        let f = fog_at(XML, 21.0).unwrap();
        assert!((f.density - 0.001175).abs() < 1e-7 && (f.floor - 125.0).abs() < 1e-4, "{f:?}");
        // 3 h : entre 23 h (veille) et 7 h, en passant minuit
        let f = fog_at(XML, 3.0).unwrap();
        assert!((f.floor - (200.0 - 150.0 * 3.0 / 7.0)).abs() < 1e-3, "{f:?}");
        assert_eq!(fog_at("<Sky/>", 12.0), None);
    }

    #[test]
    fn night_sky() {
        let xml = r#"<SunLight>
		<Direction Time="0:00" Heading="52" Pitch="52"/>
		<Color Time="0:00" R="255" G="255" B="255" Brightness="14"/>
		<Color Time="6:00" R="255" G="200" B="150" Brightness="9"/>
		<Color Time="12:00" R="102" G="179" B="255" Brightness="4"/>
		</SunLight>"#;
        assert_eq!(night_sky_scale(xml, 12.0, 0.0, 0.1), 0.1);
        assert_eq!(night_sky_scale(xml, 0.0, 0.0, 0.1), 1.0);
        let dusk = night_sky_scale(xml, 6.0, 0.0, 0.1);
        assert!((dusk - (0.1 + 0.9 * 0.5)).abs() < 1e-4, "{dusk}");
        assert_eq!(night_sky_scale(xml, 12.0, 0.0, 1.0), 1.0);
        // horizon moins assombri que le zénith la nuit, identique le jour
        let lift = |h: f32| night_sky_scale(xml, h, 0.0, 0.5) / night_sky_scale(xml, h, 0.0, 0.1);
        assert!((lift(12.0) - 5.0).abs() < 1e-4 && (lift(0.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn sun() {
        let xml = r#"<SunLight>
		<Direction Time="0:00" Heading="140" Pitch="50"/>
		<Direction Time="13:00" Heading="92" Pitch="60"/>
		<Direction Time="20:00" Heading="170" Pitch="21"/>
		<Direction Time="22:00" Heading="182" Pitch="-15"/>
		<Color Time="0:00" R="53" G="151" B="255" Brightness="2.4"/>
		<Color Time="13:00" R="255" G="243" B="206" Brightness="12"/>
		</SunLight>"#;
        let s = sun_at(xml, 13.0).unwrap();
        assert_eq!((s.heading, s.pitch, s.intensity), (92.0, 60.0, 1.0));
        assert!((s.color[0] - 1.0).abs() < 1e-6);
        // la nuit : lune faible et bleutée, ambiance réduite
        let n = sun_at(xml, 0.0).unwrap();
        assert!(n.intensity < 0.25 && n.color[2] > n.color[0] && n.ambient() <= 0.25, "{n:?}");
        // couché : la hauteur du studio reste au-dessus de l'horizon
        assert_eq!(sun_at(xml, 22.0).unwrap().state().elevation, 1.0);
        // 23 h : entre 182° (22 h) et 140° (minuit), sans faire le tour
        let h = sun_at(xml, 23.0).unwrap().heading;
        assert!((h - 161.0).abs() < 1e-3, "{h}");
        // cap ramené à -180..180
        let st = Sun { heading: 250.0, pitch: 10.0, color: [1.0; 3], intensity: 1.0 }.state();
        assert_eq!(st.azimuth, -110.0);
    }

    #[test]
    fn satellites() {
        let xml = r#"<Sky><Satellites>
        <Satellite><Texture File="stars.dds" /><UVs UOffset="0" VOffset="0" UScale="8" VScale="8" />
            <Depth Value="5" /><Orientation Time="0:00" Yaw="1" Pitch="45" Roll="0" />
            <Brightness Time="0:00" Value="12" /><Brightness Time="6:00" Value="2" />
            <Brightness Time="20:00" Value="2" /><Size Time="0:00" Value="3" /></Satellite>
        <Satellite><Texture File="planets.dds" /><UVs UOffset="0.5" VOffset="0.25" UScale="0.5" VScale="0.25" />
            <Depth Value="4" /><Orientation Time="0:00" Yaw="250" Pitch="2" Roll="0" />
            <Orientation Time="5:00" Yaw="-40" Pitch="10" Roll="0" />
            <Color Time="0:00" Red="255" Green="255" Blue="255" /><Size Time="0:00" Value="0.075" /><Sun/></Satellite>
        </Satellites></Sky>"#;
        let s = satellites_at(xml, 12.0);
        assert_eq!(s.len(), 2);
        // le plus lointain d'abord
        assert_eq!(s[0].texture, "stars.dds");
        assert!(s[0].additive() && !s[0].sun && (s[0].color[0] - (2.0f32 / 12.0).powi(2)).abs() < 1e-5);
        assert!(s[1].sun && !s[1].additive() && s[1].uv == [0.5, 0.25, 0.5, 0.25] && s[1].size == 0.075);
        // 2 h 30 : à mi-chemin entre 250° et -40° (320°) par le plus court chemin
        let h = satellites_at(xml, 2.5)[1].heading;
        assert!((h.rem_euclid(360.0) - 285.0).abs() < 1e-3, "{h}");
        let inc = includes(r#"<Includes><Include File="a.xml" /><Include File="b.xml" /></Includes>"#);
        assert_eq!(inc, ["a.xml", "b.xml"]);
    }

    #[test]
    fn clouds() {
        let xml = r#"<Sky><Clouds><DensityTexture Time="12:00" File="c.dds" />
            <TextureDensityWeights Time="0:00" R-Cumulus1="0.0" G-Cumulus2="0.0" B-Cumulus3="0.85" A-Stratus="0.0" />
            <TextureDensityWeights Time="12:00" R-Cumulus1="0.1" G-Cumulus2="0.2" B-Cumulus3="0.0" A-Stratus="0.1" />
            <CumulusContrast Time="12:00" Power="2.0" /><Color Time="12:00" R="255" G="0" B="0" /></Clouds></Sky>"#;
        let c = clouds_at(xml, 12.0).unwrap();
        assert_eq!(c.weights, [0.1, 0.2, 0.0, 0.1]);
        assert_eq!((c.contrast, c.color), (2.0, [1.0, 0.0, 0.0]));
        assert_eq!(clouds_at("<Sky/>", 12.0), None);
    }

    #[test]
    fn sway_of_sky() {
        let xml = r#"<Sky><Wind X="1.0" Y="0.0" Z="0.0" Scale="2.5"> <!-- Particle Wind -->
		<Sway Spacing="90" WavefrontScale="4" Speed ="16" Boil="0.2" Dir="180" Smoothing="1" Strength="1.3"/> <!-- Flora Wind -->
	</Wind></Sky>"#;
        assert_eq!(parse_sway(xml), Some(Sway { spacing: 90.0, speed: 16.0, boil: 0.2, smoothing: 1.0, strength: 1.3 }));
        assert_eq!(parse_sway("<Sky></Sky>"), None);
    }

    #[test]
    fn attributes() {
        assert_eq!(attr(r#" Floor="50" R="7""#, "R"), Some("7"));
        assert_eq!(attr(r#" Floor="50""#, "R"), None);
        assert_eq!(tags("<ColorGrading><Color R=\"1\"/>", "Color").len(), 1);
    }
}
