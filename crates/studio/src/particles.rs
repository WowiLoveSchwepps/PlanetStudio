//! Particules d'ambiance des continents : feuilles qui tombent sur Hossin, vent de sable sur
//! Indar, neige sur Esamir.
//!
//! Les textures sont celles des effets du jeu, et les ordres de grandeur (tailles, vitesses,
//! rotation des feuilles) en viennent aussi :
//! - Hossin : `Leaves_green_HossinTreeBig_30m_loop.xml` (`leaves_strip_02.dds`, 4 feuilles) ;
//! - Indar : `Sand_GroundCover.xml` (`Sand.dds`, nuages de poussière) et
//!   `Leaves_Desert_Blowing.xml` (`leaves_desert.dds`, brindilles sèches) ; les grains de sable
//!   reprennent les éclats de `snow_strip.dds`, teintés et étirés dans le sens du vent ;
//! - Esamir : `Snow_Blowing_01.xml` et `Nexus_Snow_Standard_01.xml` (`snow_strip.dds`, 8 flocons,
//!   et `smoke_mist.dds` pour la neige soufflée au ras du sol).
//!
//! Tout est calculé sur le GPU à partir du temps de la timeline : la position d'une particule ne
//! dépend que de son numéro et de l'instant. La lecture, le retour en arrière et l'export vidéo
//! montrent donc exactement les mêmes flocons, sans état à garder. Les particules vivent dans une
//! boîte autour de la caméra : celles qui en sortent reviennent par le côté opposé.

use serde::{Deserialize, Serialize};

/// Effet choisi. « Auto » prend celui du continent du décor chargé.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Auto,
    Aucune,
    Feuilles,
    Sable,
    Neige,
}

impl Kind {
    pub const ALL: [Kind; 5] = [Kind::Auto, Kind::Aucune, Kind::Feuilles, Kind::Sable, Kind::Neige];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Auto => "Auto (selon le continent)",
            Kind::Aucune => "Aucune",
            Kind::Feuilles => "🍃 Feuilles qui tombent (Hossin)",
            Kind::Sable => "🌪 Vent de sable (Indar)",
            Kind::Neige => "❄ Neige (Esamir)",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Kind::Auto => "auto",
            Kind::Aucune => "aucune",
            Kind::Feuilles => "feuilles",
            Kind::Sable => "sable",
            Kind::Neige => "neige",
        }
    }

    pub fn from_key(k: &str) -> Option<Kind> {
        let k = k.to_ascii_lowercase();
        Kind::ALL.into_iter().find(|c| c.key() == k).or(match k.as_str() {
            "aucun" | "none" | "off" => Some(Kind::Aucune),
            "hossin" | "leaves" => Some(Kind::Feuilles),
            "indar" | "sand" => Some(Kind::Sable),
            "esamir" | "snow" => Some(Kind::Neige),
            _ => None,
        })
    }

    /// Effet réellement affiché, d'après la carte du décor (`None` : pas de particules).
    pub fn resolve(self, zone: Option<&str>) -> Option<Kind> {
        match self {
            Kind::Aucune => None,
            Kind::Auto => match zone.map(|z| z.to_ascii_lowercase()).as_deref() {
                Some("hossin") => Some(Kind::Feuilles),
                Some("indar") => Some(Kind::Sable),
                Some("esamir") => Some(Kind::Neige),
                _ => None,
            },
            k => Some(k),
        }
    }

    /// Couches dessinées pour cet effet (vide pour Auto et Aucune).
    pub fn layers(self) -> &'static [Layer] {
        match self {
            Kind::Feuilles => &LEAVES,
            Kind::Sable => &SAND,
            Kind::Neige => &SNOW,
            Kind::Auto | Kind::Aucune => &[],
        }
    }

    /// Textures à charger pour cet effet.
    pub fn textures(self) -> Vec<&'static str> {
        let mut t: Vec<&str> = self.layers().iter().map(|l| l.texture).collect();
        t.sort_unstable();
        t.dedup();
        t
    }
}

/// Réglages enregistrés avec le projet.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Particles {
    pub kind: Kind,
    /// multiplicateur du nombre de particules (1 = celui de l'effet)
    pub density: f32,
    /// multiplicateur de la force du vent (1 = celle de l'effet ; 0 = air calme)
    pub wind: f32,
    /// direction vers laquelle souffle le vent, en degrés dans le plan du sol (0 = +X, 90 = +Z)
    pub wind_dir: f32,
    /// multiplicateur de la taille des particules
    pub size: f32,
}

impl Default for Particles {
    fn default() -> Self {
        Self { kind: Kind::Auto, density: 1.0, wind: 1.0, wind_dir: 30.0, size: 1.0 }
    }
}

impl Particles {
    /// Vent horizontal unitaire × force (m/s par m/s de l'effet).
    pub fn wind_vector(&self) -> [f32; 2] {
        let a = self.wind_dir.to_radians();
        [a.cos() * self.wind.max(0.0), a.sin() * self.wind.max(0.0)]
    }
}

/// Une couche de particules (un appel de dessin instancié).
#[derive(Clone, Copy, Debug)]
pub struct Layer {
    pub texture: &'static str,
    /// images dans la bande de la texture (côte à côte)
    pub frames: u32,
    /// nombre de particules à la densité 1
    pub count: u32,
    /// demi-côtés de la boîte qui suit la caméra (m)
    pub half: [f32; 3],
    /// la boîte repose sur le sol (y = 0) au lieu d'être centrée sur la caméra
    pub ground: bool,
    /// répartition en hauteur : 1 = uniforme, plus grand = plus dense près du bas de la boîte
    pub height_bias: f32,
    /// vitesse de chute (m/s)
    pub fall: f32,
    /// vitesse emportée par le vent (m/s, à la force 1)
    pub wind: f32,
    /// écart de vitesse d'une particule à l'autre (fraction)
    pub speed_var: f32,
    /// balancement : amplitude (m) et fréquence (Hz)
    pub flutter: [f32; 2],
    /// vitesse de rotation à l'écran (degrés/s, min et max, dans un sens ou l'autre)
    pub spin: [f32; 2],
    /// la particule bascule sur elle-même (feuille vue tantôt de face, tantôt de profil)
    pub tumble: bool,
    /// côté du carré (m), min et max
    pub size: [f32; 2],
    /// flou de mouvement : la particule est étirée sur le chemin parcouru pendant ce temps (s),
    /// comme un obturateur ouvert ; 0 = pas de flou (et la particule tourne sur elle-même)
    pub stretch: f32,
    /// teinte (linéaire) et opacité
    pub color: [f32; 3],
    pub alpha: f32,
    /// durée de vie (s) : la particule apparaît et disparaît en fondu, puis renaît ailleurs
    pub life: f32,
    /// part de la lumière du soleil reçue (les particules n'ont pas de normale)
    pub sun: f32,
}

const SNOW_STRIP: &str = "snow_strip.dds";

/// Hossin : feuilles vertes qui tombent en tournoyant (deux couches : proche et lointaine).
const LEAVES: [Layer; 2] = [
    Layer {
        texture: "leaves_strip_02.dds",
        frames: 4,
        count: 1500,
        half: [22.0, 12.0, 22.0],
        ground: false,
        height_bias: 1.0,
        fall: 0.9,
        wind: 1.2,
        speed_var: 0.35,
        flutter: [0.7, 0.35],
        spin: [120.0, 420.0],
        tumble: true,
        size: [0.08, 0.15],
        stretch: 0.0,
        color: [0.8, 0.8, 0.8],
        alpha: 1.0,
        life: 14.0,
        sun: 0.6,
    },
    Layer {
        texture: "leaves_strip_02.dds",
        frames: 4,
        count: 4000,
        half: [60.0, 22.0, 60.0],
        ground: false,
        height_bias: 1.0,
        fall: 0.9,
        wind: 1.2,
        speed_var: 0.35,
        flutter: [0.9, 0.3],
        spin: [120.0, 420.0],
        tumble: true,
        size: [0.1, 0.18],
        stretch: 0.0,
        color: [0.8, 0.8, 0.8],
        alpha: 1.0,
        life: 16.0,
        sun: 0.6,
    },
];

/// couleur du sable d'Indar (`Sand_GroundCover.xml` : 0,73 0,68 0,56 → 0,64 0,56 0,45, en sRGB)
const SAND_TINT: [f32; 3] = [0.48, 0.38, 0.24];

/// grains : plus sombres que les nuages (vus un à un, sur le sol clair)
const SAND_GRAIN: [f32; 3] = [0.34, 0.26, 0.16];

/// Indar : grains de sable filant au ras du sol, nuages de poussière, quelques brindilles sèches.
const SAND: [Layer; 3] = [
    Layer {
        texture: "Sand.dds",
        frames: 1,
        count: 90,
        half: [45.0, 2.2, 45.0],
        ground: true,
        height_bias: 1.6,
        fall: 0.0,
        wind: 4.5,
        speed_var: 0.3,
        flutter: [0.6, 0.08],
        spin: [4.0, 15.0],
        tumble: false,
        size: [5.0, 9.0],
        stretch: 0.0,
        color: SAND_TINT,
        alpha: 1.0,
        life: 9.0,
        sun: 0.8,
    },
    Layer {
        texture: SNOW_STRIP,
        frames: 8,
        count: 6000,
        half: [24.0, 2.0, 24.0],
        ground: true,
        height_bias: 2.5,
        fall: 0.0,
        wind: 7.0,
        speed_var: 0.35,
        flutter: [0.25, 1.2],
        spin: [0.0, 0.0],
        tumble: false,
        size: [0.008, 0.018],
        stretch: 0.03,
        color: SAND_GRAIN,
        alpha: 0.9,
        life: 2.5,
        sun: 0.8,
    },
    Layer {
        texture: "leaves_desert.dds",
        frames: 4,
        count: 60,
        half: [20.0, 1.2, 20.0],
        ground: true,
        height_bias: 2.0,
        fall: 0.0,
        wind: 5.0,
        speed_var: 0.4,
        flutter: [0.35, 0.9],
        spin: [120.0, 420.0],
        tumble: true,
        size: [0.12, 0.22],
        stretch: 0.0,
        color: [0.6, 0.6, 0.6],
        alpha: 1.0,
        life: 6.0,
        sun: 0.8,
    },
];

/// Esamir : flocons proches et lointains, et neige soufflée au ras du sol.
const SNOW: [Layer; 3] = [
    Layer {
        texture: SNOW_STRIP,
        frames: 8,
        count: 20000,
        half: [20.0, 12.0, 20.0],
        ground: false,
        height_bias: 1.0,
        fall: 1.3,
        wind: 2.0,
        speed_var: 0.3,
        flutter: [0.35, 0.6],
        spin: [180.0, 360.0],
        tumble: false,
        size: [0.03, 0.06],
        stretch: 0.0,
        color: [1.0, 1.0, 1.0],
        alpha: 0.9,
        life: 10.0,
        sun: 0.7,
    },
    Layer {
        texture: SNOW_STRIP,
        frames: 8,
        count: 30000,
        half: [70.0, 30.0, 70.0],
        ground: false,
        height_bias: 1.0,
        fall: 1.3,
        wind: 2.0,
        speed_var: 0.3,
        flutter: [0.5, 0.5],
        spin: [180.0, 360.0],
        tumble: false,
        size: [0.05, 0.09],
        stretch: 0.0,
        color: [1.0, 1.0, 1.0],
        alpha: 0.9,
        life: 12.0,
        sun: 0.7,
    },
    Layer {
        texture: "smoke_mist.dds",
        frames: 1,
        count: 60,
        half: [40.0, 1.5, 40.0],
        ground: true,
        height_bias: 1.5,
        fall: 0.0,
        wind: 3.0,
        speed_var: 0.3,
        flutter: [0.5, 0.1],
        spin: [5.0, 20.0],
        tumble: false,
        size: [4.0, 7.0],
        stretch: 0.0,
        color: [0.85, 0.88, 0.95],
        alpha: 0.5,
        life: 7.0,
        sun: 0.8,
    },
];

/// Charge sur le GPU les textures d'un effet qui y manquent.
pub fn prepare(renderer: &mut crate::render::Renderer, game: &forgelight::GameData, kind: Option<Kind>) {
    for t in kind.map(Kind::textures).unwrap_or_default() {
        if renderer.has_particle_texture(t) {
            continue;
        }
        match game.load_texture(t) {
            Ok(img) => renderer.upload_particle_texture(t, &bleed(&img)),
            Err(e) => eprintln!("particules : {e:#}"),
        }
    }
}

/// Les pixels transparents des textures du jeu ont une couleur quelconque (souvent noire) : en
/// réduisant la texture (mips, particules lointaines), elle déborde sur la forme et fait un liseré
/// sombre. On la remplace par la couleur moyenne de la forme, puis on refait les mips en pondérant
/// la couleur par l'alpha.
pub fn bleed(img: &forgelight::dds::Image) -> forgelight::dds::Image {
    let (w, h) = (img.width as usize, img.height as usize);
    let src = &img.mips[0][..w * h * 4];
    let (mut sum, mut wsum) = ([0.0f64; 3], 0.0f64);
    for px in src.chunks_exact(4) {
        let a = px[3] as f64;
        for c in 0..3 {
            sum[c] += px[c] as f64 * a;
        }
        wsum += a;
    }
    let avg = sum.map(|v| if wsum > 0.0 { (v / wsum) as f32 } else { 255.0 });
    let mut level: Vec<u8> = src
        .chunks_exact(4)
        .flat_map(|px| {
            let k = (px[3] as f32 / 64.0).min(1.0);
            let c = |i: usize| (avg[i] + (px[i] as f32 - avg[i]) * k).round().clamp(0.0, 255.0) as u8;
            [c(0), c(1), c(2), px[3]]
        })
        .collect();
    let mut mips = Vec::new();
    let (mut lw, mut lh) = (w, h);
    loop {
        mips.push(level.clone());
        if lw == 1 && lh == 1 {
            break;
        }
        let (nw, nh) = ((lw / 2).max(1), (lh / 2).max(1));
        let mut next = vec![0u8; nw * nh * 4];
        for y in 0..nh {
            for x in 0..nw {
                let (mut c, mut a) = ([0.0f32; 3], 0.0f32);
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let (sx, sy) = ((x * 2 + dx).min(lw - 1), (y * 2 + dy).min(lh - 1));
                    let p = &level[(sy * lw + sx) * 4..][..4];
                    let pa = p[3] as f32 + 1.0;
                    for i in 0..3 {
                        c[i] += p[i] as f32 * pa;
                    }
                    a += pa;
                }
                let o = &mut next[(y * nw + x) * 4..][..4];
                for i in 0..3 {
                    o[i] = (c[i] / a).round() as u8;
                }
                o[3] = ((a - 4.0) / 4.0).round() as u8;
            }
        }
        level = next;
        (lw, lh) = (nw, nh);
    }
    forgelight::dds::Image::rgba(img.width, img.height, mips)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_follows_the_continent() {
        assert_eq!(Kind::Auto.resolve(Some("Hossin")), Some(Kind::Feuilles));
        assert_eq!(Kind::Auto.resolve(Some("Indar")), Some(Kind::Sable));
        assert_eq!(Kind::Auto.resolve(Some("Esamir")), Some(Kind::Neige));
        assert_eq!(Kind::Auto.resolve(Some("Amerish")), None);
        assert_eq!(Kind::Auto.resolve(None), None);
        assert_eq!(Kind::Neige.resolve(None), Some(Kind::Neige));
        assert_eq!(Kind::Aucune.resolve(Some("Esamir")), None);
    }

    #[test]
    fn bleed_fills_transparent_pixels_and_mips() {
        // 2×1 : un pixel vert opaque, un pixel noir transparent
        let img = forgelight::dds::Image::rgba(2, 1, vec![vec![0, 200, 0, 255, 0, 0, 0, 0]]);
        let out = bleed(&img);
        assert_eq!(&out.mips[0][4..8], &[0, 200, 0, 0]);
        assert_eq!(out.mips.len(), 2);
        // la réduction garde le vert (pas de noir mêlé)
        assert_eq!(&out.mips[1][..3], &[0, 200, 0]);
    }

    #[test]
    fn keys_round_trip() {
        for k in Kind::ALL {
            assert_eq!(Kind::from_key(k.key()), Some(k));
        }
        assert_eq!(Kind::from_key("Esamir"), Some(Kind::Neige));
    }
}
