//! Ambiances : éclairage d'environnement tiré des panoramas du jeu.
//!
//! Le jeu contient, par continent, un panorama équirectangulaire du terrain lointain
//! (`<continent>_ambient_c.dds`, 256×128). Son alpha masque le ciel, laissé noir : on y place le
//! ciel du dôme `ambient_blue_dome.tga`, teinté près de l'horizon par la brume des reliefs
//! lointains (ce choix est le nôtre, le jeu ne fournit pas de ciel par continent).
//! Le panorama obtenu sert à deux choses :
//! - l'éclairage diffus, réduit à 9 harmoniques sphériques (convolution en cosinus) ;
//! - les reflets, lus dans des niveaux de mip de plus en plus flous selon la rugosité.

use anyhow::{Context, Result};
use forgelight::{dds, GameData};
use serde::{Deserialize, Serialize};
use crate::anim::{AmbState, SunState, DEFAULT_BACKGROUND};
use glam::Vec3;
use std::f32::consts::PI;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ambiance {
    /// éclairage fixe d'origine (dôme gris, lumière de remplissage)
    #[default]
    Studio,
    /// Indar : panorama des canyons, celui que la carte cite (`<Probe>` de `sky_Indar.xml`)
    Indar,
    /// Indar : panorama des hauts plateaux (savane, arbres), `indar_highlands_c.dds`
    IndarHighlands,
    Amerish,
    Esamir,
    Nexus,
    /// dôme ciel bleu / sol brun du jeu
    BlueDome,
    /// ambiance extérieure générique du jeu
    Outdoor,
}

impl Ambiance {
    pub const ALL: [Ambiance; 8] = [
        Self::Studio,
        Self::Indar,
        Self::IndarHighlands,
        Self::Amerish,
        Self::Esamir,
        Self::Nexus,
        Self::BlueDome,
        Self::Outdoor,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Studio => tr!("Studio (éclairage fixe)", "Studio (fixed lighting)"),
            Self::Indar => "Indar (canyons)",
            Self::IndarHighlands => tr!("Indar (savane)", "Indar (savanna)"),
            Self::Amerish => "Amerish",
            Self::Esamir => "Esamir",
            Self::Nexus => "Nexus",
            Self::BlueDome => tr!("Dôme ciel bleu", "Blue sky dome"),
            Self::Outdoor => tr!("Extérieur par défaut", "Default outdoor"),
        }
    }

    /// Nom court (ligne de commande).
    pub fn key(self) -> &'static str {
        match self {
            Self::Studio => "studio",
            Self::Indar => "indar",
            Self::IndarHighlands => "indar-savane",
            Self::Amerish => "amerish",
            Self::Esamir => "esamir",
            Self::Nexus => "nexus",
            Self::BlueDome => "dome",
            Self::Outdoor => "exterieur",
        }
    }

    pub fn from_key(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.key().eq_ignore_ascii_case(s))
    }

    fn terrain(self) -> Option<&'static str> {
        match self {
            // Indar n'a pas de `indar_ambient_c.dds` : son panorama porte le nom de la région
            Self::Indar => Some("indar_canyons_c.dds"),
            Self::IndarHighlands => Some("indar_highlands_c.dds"),
            Self::Amerish => Some("amerish_ambient_c.dds"),
            Self::Esamir => Some("esamir_ambient_c.dds"),
            Self::Nexus => Some("nexus_ambient_c.dds"),
            _ => None,
        }
    }
}

/// Réglages d'éclairage enregistrés avec le projet.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Lighting {
    pub ambiance: Ambiance,
    /// rotation du panorama autour de la verticale (degrés)
    pub rotation: f32,
    /// multiplicateur de l'ambiance (1 = même niveau moyen que le studio)
    pub intensity: f32,
    /// ombres portées du soleil (personnages et sol)
    pub shadows: bool,
    /// opacité de l'ombre sur le sol (0 = aucune)
    pub ground_shadow: f32,
    /// le décor (arbres, bâtiments) fait de l'ombre aux personnages ; décoché, ils restent
    /// éclairés par le soleil même sous la canopée (éclairage de cinéma)
    pub decor_shades_actors: bool,
    /// soleil : azimut (degrés, 0 = vers +Z, 90 = vers +X), hauteur au-dessus de l'horizon (degrés)
    pub sun_azimuth: f32,
    pub sun_elevation: f32,
    /// couleur du soleil (RVB 0..1, comme le sélecteur de couleur) et intensité (1 = studio)
    pub sun_color: [f32; 3],
    pub sun_intensity: f32,
    /// couleur de fond de la vue et des exports (linéaire)
    pub background: [f32; 3],
}

impl Default for Lighting {
    fn default() -> Self {
        // azimut et hauteur donnent la lumière d'origine du studio, (-0,4 ; -0,8 ; -0,45)
        Self {
            ambiance: Ambiance::Studio,
            rotation: 0.0,
            intensity: 1.0,
            shadows: true,
            ground_shadow: 0.6,
            decor_shades_actors: true,
            sun_azimuth: 41.6,
            sun_elevation: 53.0,
            sun_color: [1.0, 1.0, 1.0],
            sun_intensity: 1.0,
            background: DEFAULT_BACKGROUND,
        }
    }
}

/// Réglages de soleil prédéfinis : (nom, azimut, hauteur, couleur, intensité).
pub const SUN_PRESETS: [(&str, f32, f32, [f32; 3], f32); 5] = [
    ("Studio", 41.6, 53.0, [1.0, 1.0, 1.0], 1.0),
    ("Midi", 20.0, 75.0, [1.0, 0.97, 0.92], 1.15),
    ("Matin", 70.0, 20.0, [1.0, 0.85, 0.65], 0.9),
    ("Coucher", -60.0, 8.0, [1.0, 0.55, 0.3], 0.85),
    ("Nuit (lune)", 150.0, 35.0, [0.55, 0.65, 1.0], 0.3),
];

/// Nom affiché d'un réglage de soleil prédéfini (`SUN_PRESETS`, dont les noms servent aussi de clés).
pub fn sun_preset_label(name: &str) -> &str {
    if !crate::i18n::en() {
        return name;
    }
    match name {
        "Midi" => "Noon",
        "Matin" => "Morning",
        "Coucher" => "Sunset",
        "Nuit (lune)" => "Night (moon)",
        n => n,
    }
}

/// Heure du ciel de la carte qui va avec un réglage de soleil prédéfini (`None` : « Studio »).
pub fn preset_sky_hour(i: usize) -> Option<f32> {
    match SUN_PRESETS.get(i)?.0 {
        "Midi" => Some(13.0),
        "Matin" => Some(7.0),
        "Coucher" => Some(20.0),
        "Nuit (lune)" => Some(0.0),
        _ => None,
    }
}

impl Lighting {
    /// Direction vers laquelle le soleil éclaire (du soleil vers la scène).
    pub fn light_dir(&self) -> Vec3 {
        let (az, el) = (self.sun_azimuth.to_radians(), self.sun_elevation.clamp(1.0, 90.0).to_radians());
        -Vec3::new(el.cos() * az.sin(), el.sin(), el.cos() * az.cos())
    }

    /// Couleur × intensité du soleil, prête pour le shader (le studio éclairait à 0,95).
    pub fn sun_radiance(&self) -> Vec3 {
        Vec3::from(self.sun_color.map(|c| c.max(0.0))) * self.sun_intensity.max(0.0) * 0.95
    }

    pub fn apply_preset(&mut self, i: usize) {
        if let Some(s) = preset_state(i) {
            self.set_sun_state(s);
        }
    }

    /// Soleil fixe des réglages.
    pub fn sun_state(&self) -> SunState {
        SunState {
            azimuth: self.sun_azimuth,
            elevation: self.sun_elevation,
            color: self.sun_color,
            intensity: self.sun_intensity,
        }
    }

    pub fn set_sun_state(&mut self, s: SunState) {
        self.sun_azimuth = s.azimuth;
        self.sun_elevation = s.elevation;
        self.sun_color = s.color;
        self.sun_intensity = s.intensity;
    }

    /// Mêmes réglages avec un autre soleil (soleil animé à une image donnée).
    pub fn with_sun(&self, s: SunState) -> Self {
        let mut l = self.clone();
        l.set_sun_state(s);
        l
    }

    /// Partie animable de l'ambiance (rotation, intensité, fond).
    pub fn amb_state(&self) -> AmbState {
        AmbState { rotation: self.rotation, intensity: self.intensity, background: self.background }
    }

    pub fn set_amb_state(&mut self, s: AmbState) {
        self.rotation = s.rotation;
        self.intensity = s.intensity;
        self.background = s.background;
    }
}

/// État d'un préréglage de soleil.
pub fn preset_state(i: usize) -> Option<SunState> {
    SUN_PRESETS
        .get(i)
        .map(|&(_, azimuth, elevation, color, intensity)| SunState { azimuth, elevation, color, intensity })
}

/// Préréglage dont le nom commence par `name` (sans tenir compte de la casse).
pub fn preset_by_name(name: &str) -> Option<usize> {
    let n = name.to_lowercase();
    SUN_PRESETS.iter().position(|p| p.0.to_lowercase().starts_with(&n))
}

/// Panorama prêt pour le GPU.
pub struct EnvMap {
    pub width: u32,
    pub height: u32,
    /// niveaux de mip, RGBA8 sRGB (flou croissant)
    pub mips: Vec<Vec<u8>>,
    /// éclairement diffus divisé par π (9 coefficients RGB), voir [`sh_basis`]
    pub sh: [[f32; 3]; 9],
    /// exposition commune à toutes les ambiances (le dôme du jeu y a le niveau moyen du studio)
    pub scale: f32,
}

const W: usize = 256;
const H: usize = 128;
/// éclairement moyen visé, égal à celui de l'éclairage studio (dôme de 0,22 à 0,42)
const TARGET_MEAN: f32 = 0.32;

fn srgb_to_linear(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f32) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
    (s * 255.0).round() as u8
}

/// Image linéaire RGBA (alpha brut entre 0 et 1).
struct Pano {
    w: usize,
    h: usize,
    px: Vec<[f32; 4]>,
}

impl Pano {
    fn from_image(img: &dds::Image) -> Self {
        let (w, h) = (img.width as usize, img.height as usize);
        let px = img.mips[0]
            .chunks_exact(4)
            .map(|p| [srgb_to_linear(p[0]), srgb_to_linear(p[1]), srgb_to_linear(p[2]), p[3] as f32 / 255.0])
            .collect();
        Self { w, h, px }
    }

    /// Lecture bilinéaire ; u boucle, v est borné.
    fn sample(&self, u: f32, v: f32) -> [f32; 4] {
        let x = u * self.w as f32 - 0.5;
        let y = (v * self.h as f32 - 0.5).clamp(0.0, (self.h - 1) as f32);
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let xi = |x: f32| (x as i64).rem_euclid(self.w as i64) as usize;
        let (xa, xb) = (xi(x0), xi(x0 + 1.0));
        let (ya, yb) = (y0 as usize, (y0 as usize + 1).min(self.h - 1));
        let p = |x: usize, y: usize| self.px[y * self.w + x];
        let mut out = [0.0; 4];
        for (i, o) in out.iter_mut().enumerate() {
            let top = p(xa, ya)[i] * (1.0 - fx) + p(xb, ya)[i] * fx;
            let bot = p(xa, yb)[i] * (1.0 - fx) + p(xb, yb)[i] * fx;
            *o = top * (1.0 - fy) + bot * fy;
        }
        out
    }

    fn resized(&self, w: usize, h: usize) -> Self {
        let mut px = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                px.push(self.sample((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32));
            }
        }
        Self { w, h, px }
    }
}

/// Direction d'un texel : u = 0,5 regarde vers +Z, v = 0 est le zénith (+Y).
/// Le shader fait l'inverse avec `atan2(x, z)` et `acos(y)`.
fn direction(u: f32, v: f32) -> [f32; 3] {
    let phi = (u - 0.5) * 2.0 * PI;
    let theta = v * PI;
    [theta.sin() * phi.sin(), theta.cos(), theta.sin() * phi.cos()]
}

/// Base des harmoniques sphériques réelles jusqu'à l'ordre 2 (même formule dans le shader).
pub fn sh_basis([x, y, z]: [f32; 3]) -> [f32; 9] {
    [
        0.282_095,
        0.488_603 * y,
        0.488_603 * z,
        0.488_603 * x,
        1.092_548 * x * y,
        1.092_548 * y * z,
        0.315_392 * (3.0 * z * z - 1.0),
        1.092_548 * x * z,
        0.546_274 * (x * x - y * y),
    ]
}

/// Construit le panorama d'une ambiance (`None` pour le studio).
pub fn build(gd: &GameData, ambiance: Ambiance) -> Result<Option<EnvMap>> {
    let load = |name: &str| -> Result<Pano> {
        Ok(Pano::from_image(&gd.load_texture(name).with_context(|| format!("ambiance : {name}"))?))
    };
    if ambiance == Ambiance::Studio {
        return Ok(None);
    }
    let dome = load("ambient_blue_dome.tga")?.resized(W, H);
    // une seule exposition pour toutes les ambiances, calée sur le dôme : un continent enneigé
    // éclaire plus qu'une forêt sombre
    let scale = exposure(&dome);
    let pano = match ambiance {
        Ambiance::Studio => unreachable!(),
        Ambiance::BlueDome => dome,
        Ambiance::Outdoor => load("ambient_outdoordefault.tga")?.resized(W, H),
        a => with_sky(&load(a.terrain().unwrap())?.resized(W, H), &dome),
    };
    Ok(Some(finish(&pano, scale)))
}

fn luminance(c: [f32; 4]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// Complète un panorama de terrain (ciel masqué) avec le ciel du dôme.
///
/// Le bas du ciel prend la teinte de la brume d'horizon, estimée sur les reliefs lointains du
/// panorama, puis rejoint la couleur du dôme au zénith. La luminosité du dôme est conservée.
fn with_sky(terrain: &Pano, dome: &Pano) -> Pano {
    // brume : moyenne des premiers texels de terrain sous la ligne d'horizon de chaque colonne
    let (mut haze, mut n) = ([0.0f32; 4], 0.0);
    for x in 0..terrain.w {
        let Some(top) = (0..terrain.h).find(|&y| terrain.px[y * terrain.w + x][3] > 0.5) else { continue };
        for y in top..(top + 6).min(terrain.h) {
            let c = terrain.px[y * terrain.w + x];
            for k in 0..3 {
                haze[k] += c[k];
            }
            n += 1.0;
        }
    }
    let haze_lum = luminance(haze);
    let px = terrain
        .px
        .iter()
        .zip(&dome.px)
        .enumerate()
        .map(|(i, (t, s))| {
            let v = (i / terrain.w) as f32 / terrain.h as f32;
            let mut sky = *s;
            if n > 0.0 && haze_lum > 1e-5 {
                // 0 au zénith, 1 à l'horizon et en dessous
                let low = (v / 0.5).clamp(0.0, 1.0).powi(3);
                let k = luminance(*s) / haze_lum;
                for c in 0..3 {
                    sky[c] += (haze[c] * k - sky[c]) * low;
                }
            }
            let a = t[3];
            [sky[0] + (t[0] - sky[0]) * a, sky[1] + (t[1] - sky[1]) * a, sky[2] + (t[2] - sky[2]) * a, 1.0]
        })
        .collect();
    Pano { w: terrain.w, h: terrain.h, px }
}

/// Facteur qui ramène l'éclairement moyen d'un panorama au niveau du studio.
fn exposure(p: &Pano) -> f32 {
    let (mut sum, mut wsum) = (0.0f32, 0.0f32);
    for y in 0..p.h {
        let w = (((y as f32 + 0.5) / p.h as f32) * PI).sin();
        for x in 0..p.w {
            sum += luminance(p.px[y * p.w + x]) * w;
            wsum += w;
        }
    }
    let mean = sum / wsum.max(1e-6);
    if mean > 1e-4 {
        TARGET_MEAN / mean
    } else {
        1.0
    }
}

fn finish(p: &Pano, scale: f32) -> EnvMap {
    // projection sur les harmoniques sphériques
    let mut l = [[0.0f32; 3]; 9];
    let d_omega = (2.0 * PI / p.w as f32) * (PI / p.h as f32);
    for y in 0..p.h {
        let v = (y as f32 + 0.5) / p.h as f32;
        let w = d_omega * (v * PI).sin();
        for x in 0..p.w {
            let u = (x as f32 + 0.5) / p.w as f32;
            let basis = sh_basis(direction(u, v));
            let c = p.px[y * p.w + x];
            for (li, b) in l.iter_mut().zip(basis) {
                for k in 0..3 {
                    li[k] += c[k] * b * w;
                }
            }
        }
    }
    // convolution en cosinus (Ramamoorthi et Hanrahan), divisée par π
    let band = [1.0, 2.0 / 3.0, 2.0 / 3.0, 2.0 / 3.0, 0.25, 0.25, 0.25, 0.25, 0.25];
    for (li, f) in l.iter_mut().zip(band) {
        for c in li.iter_mut() {
            *c *= f;
        }
    }
    // niveaux de mip moyennés en linéaire
    let mut levels = vec![(p.w, p.h, p.px.clone())];
    while let Some((w, h, px)) = levels.last().filter(|(w, h, _)| *w > 1 || *h > 1) {
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        let mut out = vec![[0.0f32; 4]; nw * nh];
        for y in 0..nh {
            for x in 0..nw {
                let mut acc = [0.0; 4];
                let mut n = 0.0;
                for sy in (y * 2)..((y * 2 + 2).min(*h)) {
                    for sx in (x * 2)..((x * 2 + 2).min(*w)) {
                        let c = px[sy * w + sx];
                        for k in 0..4 {
                            acc[k] += c[k];
                        }
                        n += 1.0;
                    }
                }
                out[y * nw + x] = acc.map(|c| c / n);
            }
        }
        levels.push((nw, nh, out));
    }
    let mips = levels
        .iter()
        .map(|(_, _, px)| {
            px.iter()
                .flat_map(|c| [linear_to_srgb(c[0]), linear_to_srgb(c[1]), linear_to_srgb(c[2]), 255])
                .collect()
        })
        .collect();
    EnvMap { width: p.w as u32, height: p.h as u32, mips, sh: l, scale }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Un environnement uniforme doit donner un éclairement égal à sa couleur, quelle que soit la normale.
    #[test]
    fn uniform_environment() {
        let p = Pano { w: 64, h: 32, px: vec![[0.5, 0.25, 0.1, 1.0]; 64 * 32] };
        let env = finish(&p, 1.0);
        // l'exposition ramène cet environnement au niveau moyen du studio
        let lum = luminance([0.5, 0.25, 0.1, 1.0]);
        assert!((exposure(&p) * lum - TARGET_MEAN).abs() < 1e-3);
        for n in [[0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, -1.0]] {
            let b = sh_basis(n);
            let e: Vec<f32> = (0..3).map(|k| (0..9).map(|i| env.sh[i][k] * b[i]).sum()).collect();
            assert!((e[0] - 0.5).abs() < 0.01 && (e[1] - 0.25).abs() < 0.01 && (e[2] - 0.1).abs() < 0.01, "{e:?}");
        }
    }

    /// Les réglages se relisent, et un projet ancien (champ absent ou partiel) garde des valeurs sûres.
    #[test]
    fn lighting_serde() {
        let l = Lighting {
            ambiance: Ambiance::Esamir,
            rotation: 45.0,
            intensity: 1.5,
            shadows: false,
            ground_shadow: 0.3,
            decor_shades_actors: false,
            sun_azimuth: -60.0,
            sun_elevation: 8.0,
            sun_color: [1.0, 0.5, 0.3],
            sun_intensity: 0.8,
            background: [0.0, 0.0, 0.05],
        };
        let back: Lighting = serde_json::from_str(&serde_json::to_string(&l).unwrap()).unwrap();
        assert_eq!(back, l);
        let partial: Lighting = serde_json::from_str(r#"{"ambiance":"nexus"}"#).unwrap();
        assert_eq!(partial, Lighting { ambiance: Ambiance::Nexus, ..Default::default() });
        assert_eq!(Ambiance::from_key("Amerish"), Some(Ambiance::Amerish));
        assert_eq!(Ambiance::from_key("indar-savane"), Some(Ambiance::IndarHighlands));
        let indar: Lighting = serde_json::from_str(r#"{"ambiance":"indar_highlands"}"#).unwrap();
        assert_eq!(indar.ambiance, Ambiance::IndarHighlands);
    }

    /// Le soleil par défaut reproduit la lumière d'origine du studio.
    #[test]
    fn default_sun_matches_studio() {
        let l = Lighting::default();
        let old = Vec3::new(-0.4, -0.8, -0.45).normalize();
        assert!(l.light_dir().angle_between(old).to_degrees() < 0.5, "{}", l.light_dir());
        assert!((l.sun_radiance() - Vec3::splat(0.95)).length() < 1e-5);
        let mut p = l.clone();
        p.apply_preset(0);
        assert_eq!(p, l);
        // hauteur bornée : jamais sous l'horizon
        p.sun_elevation = -30.0;
        assert!(p.light_dir().y < 0.0);
    }

    /// Ciel blanc et sol noir : une normale vers le haut reçoit plus qu'une normale vers le bas.
    #[test]
    fn sky_lights_from_above() {
        let (w, h) = (64, 32);
        let px = (0..w * h).map(|i| if i / w < h / 2 { [1.0; 4] } else { [0.0, 0.0, 0.0, 1.0] }).collect();
        let env = finish(&Pano { w, h, px }, 1.0);
        let e = |n| {
            let b = sh_basis(n);
            (0..9).map(|i| env.sh[i][0] * b[i]).sum::<f32>()
        };
        let (up, side, down) = (e([0.0, 1.0, 0.0]), e([1.0, 0.0, 0.0]), e([0.0, -1.0, 0.0]));
        assert!(up > 0.9 && (side - 0.5).abs() < 0.05 && down < 0.1, "{up} {side} {down}");
    }
}
