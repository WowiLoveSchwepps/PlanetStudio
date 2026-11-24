//! Qualité graphique : réglages de rendu (dans l'esprit des options du jeu) et préréglages.
//!
//! Ce sont des préférences de la machine, pas du projet : elles sont enregistrées avec la
//! configuration du studio. Les exports (photos et vidéos) peuvent utiliser le préréglage
//! « Rendu final » quelle que soit la qualité de la vue.

use crate::render::Renderer;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShadowQuality {
    Off,
    Low,
    Medium,
    High,
    Ultra,
}

impl ShadowQuality {
    pub const ALL: [Self; 5] = [Self::Off, Self::Low, Self::Medium, Self::High, Self::Ultra];

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Désactivées",
            Self::Low => "Basse (1024, bords nets)",
            Self::Medium => "Moyenne (2048, 3×3)",
            Self::High => "Haute (4096, 3×3)",
            Self::Ultra => "Ultra (4096, 5×5)",
        }
    }

    /// (côté de la carte d'ombre, rayon du filtrage)
    fn params(self) -> Option<(u32, u32)> {
        match self {
            Self::Off => None,
            Self::Low => Some((1024, 0)),
            Self::Medium => Some((2048, 1)),
            Self::High => Some((4096, 1)),
            Self::Ultra => Some((4096, 2)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextureQuality {
    Low,
    Medium,
    High,
    Ultra,
}

impl TextureQuality {
    pub const ALL: [Self; 4] = [Self::Low, Self::Medium, Self::High, Self::Ultra];

    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "Basse (1/8)",
            Self::Medium => "Moyenne (1/4)",
            Self::High => "Haute (1/2)",
            Self::Ultra => "Ultra (pleine résolution)",
        }
    }

    /// niveaux de mip ignorés
    fn lod(self) -> f32 {
        match self {
            Self::Low => 3.0,
            Self::Medium => 2.0,
            Self::High => 1.0,
            Self::Ultra => 0.0,
        }
    }
}

/// Détail des écosystèmes du terrain (sol, roche) : distance jusqu'où il est visible.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerrainQuality {
    Low,
    Medium,
    High,
    Ultra,
}

impl TerrainQuality {
    pub const ALL: [Self; 4] = [Self::Low, Self::Medium, Self::High, Self::Ultra];

    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "Basse (détail jusqu'à 60 m)",
            Self::Medium => "Moyenne (110 m)",
            Self::High => "Haute (200 m)",
            Self::Ultra => "Ultra (350 m)",
        }
    }

    /// distance (m) où le détail a disparu
    pub fn distance(self) -> f32 {
        match self {
            Self::Low => 60.0,
            Self::Medium => 110.0,
            Self::High => 200.0,
            Self::Ultra => 350.0,
        }
    }
}

/// Nombre de particules d'ambiance (neige, sable, feuilles), comme « Particles » dans le jeu.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParticleQuality {
    Low,
    Medium,
    High,
    Ultra,
}

impl ParticleQuality {
    pub const ALL: [Self; 4] = [Self::Low, Self::Medium, Self::High, Self::Ultra];

    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "Basse (×0,5)",
            Self::Medium => "Moyenne (×0,75)",
            Self::High => "Haute (×1)",
            Self::Ultra => "Ultra (×1,5)",
        }
    }

    pub fn factor(self) -> f32 {
        match self {
            Self::Low => 0.5,
            Self::Medium => 0.75,
            Self::High => 1.0,
            Self::Ultra => 1.5,
        }
    }
}

/// Préréglage d'ensemble, comme « Overall Quality » dans le jeu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Preview,
    Balanced,
    Final,
}

impl Preset {
    pub const ALL: [Self; 3] = [Self::Preview, Self::Balanced, Self::Final];

    pub fn label(self) -> &'static str {
        match self {
            Self::Preview => "Aperçu",
            Self::Balanced => "Équilibré",
            Self::Final => "Rendu final",
        }
    }

    pub fn from_key(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "apercu" | "aperçu" | "preview" => Some(Self::Preview),
            "equilibre" | "équilibré" | "balanced" => Some(Self::Balanced),
            "final" | "rendu-final" => Some(Self::Final),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Quality {
    /// résolution de la vue de travail et du moniteur (1 = taille de l'écran)
    pub render_scale: f32,
    /// anticrénelage : 1 (aucun), 2, 4 ou 8 échantillons par pixel (ramené à ce que la carte accepte)
    pub msaa: u32,
    /// FXAA : lissage des contours sur l'image finale (s'ajoute au MSAA, ou le remplace)
    pub fxaa: bool,
    /// anticrénelage des reflets : moins de scintillement des reflets du soleil et du relief
    pub specular_aa: bool,
    pub shadows: ShadowQuality,
    pub textures: TextureQuality,
    /// filtrage bicubique des textures agrandies (gros plans), en textures « Ultra »
    pub texture_hq: bool,
    /// netteté des textures dans les images suréchantillonnées (0 à 1 : biais de mip jusqu'à −1)
    pub texture_sharpness: f32,
    /// relief des textures (normal maps)
    pub normal_maps: bool,
    /// lumières des textures (émissif)
    pub emissive: bool,
    /// intensité des lumières des textures (1 = défaut)
    pub emissive_strength: f32,
    /// luminosité finale (1 = neutre)
    pub brightness: f32,
    /// suréchantillonnage des exports seulement : rendu N fois plus grand puis réduit
    pub supersample: u32,
    /// halo autour des zones très lumineuses (0 = désactivé)
    pub bloom: f32,
    /// occlusion ambiante : assombrissement des creux (0 = désactivée)
    pub ao: f32,
    /// rayon de l'occlusion ambiante (m)
    pub ao_radius: f32,
    /// détail des écosystèmes du terrain
    pub terrain: TerrainQuality,
    /// flore de qualité haute : bords des feuillages lissés (avec le MSAA), feuillage garni au loin
    pub flora_hq: bool,
    /// nombre de particules d'ambiance
    pub particles: ParticleQuality,
    /// eau de qualité haute : houle animée, reflet du ciel de la carte
    pub water_hq: bool,
}

/// Valeurs par défaut des effets.
pub const DEFAULT_BLOOM: f32 = 1.0;
pub const DEFAULT_AO: f32 = 0.8;
pub const DEFAULT_AO_RADIUS: f32 = 0.3;

/// Suréchantillonnage des exports par défaut.
const DEFAULT_SUPERSAMPLE: u32 = 2;

impl Default for Quality {
    fn default() -> Self {
        Self::preset(Preset::Balanced)
    }
}

impl Quality {
    /// « Équilibré » est le rendu d'origine du studio.
    pub fn preset(p: Preset) -> Self {
        let base = Self {
            render_scale: 1.0,
            msaa: 4,
            fxaa: false,
            specular_aa: true,
            shadows: ShadowQuality::High,
            textures: TextureQuality::Ultra,
            texture_hq: true,
            texture_sharpness: 0.5,
            normal_maps: true,
            emissive: true,
            emissive_strength: 1.0,
            brightness: 1.0,
            supersample: DEFAULT_SUPERSAMPLE,
            bloom: DEFAULT_BLOOM,
            ao: DEFAULT_AO,
            ao_radius: DEFAULT_AO_RADIUS,
            terrain: TerrainQuality::High,
            flora_hq: true,
            particles: ParticleQuality::High,
            water_hq: true,
        };
        match p {
            // l'aperçu se passe d'occlusion ambiante, l'effet le plus coûteux
            Preset::Preview => Self {
                render_scale: 0.75,
                shadows: ShadowQuality::Medium,
                textures: TextureQuality::High,
                texture_hq: false,
                ao: 0.0,
                terrain: TerrainQuality::Medium,
                particles: ParticleQuality::Medium,
                ..base
            },
            Preset::Balanced => base,
            // MSAA 4× : les exports sont déjà suréchantillonnés (8× sur une image 3 fois plus
            // grande demanderait des gigaoctets de mémoire graphique)
            Preset::Final => Self {
                shadows: ShadowQuality::Ultra,
                terrain: TerrainQuality::Ultra,
                ..base
            },
        }
    }

    /// Applique un préréglage en gardant la luminosité et le suréchantillonnage, réglés à part.
    pub fn with_preset(&self, p: Preset) -> Self {
        Self { brightness: self.brightness, supersample: self.supersample, ..Self::preset(p) }
    }

    /// Préréglage correspondant (luminosité et suréchantillonnage mis à part).
    pub fn matching_preset(&self) -> Option<Preset> {
        Preset::ALL.into_iter().find(|p| self.with_preset(*p) == *self)
    }

    /// Qualité des exports : « Rendu final », avec la luminosité et le suréchantillonnage choisis.
    pub fn for_export(&self) -> Self {
        self.with_preset(Preset::Final)
    }

    /// Envoie les réglages au rendu ; ne recrée que ce qui change.
    pub fn apply(&self, r: &mut Renderer, shadows_wanted: bool) {
        r.set_msaa(self.msaa);
        r.post_settings.fxaa = self.fxaa;
        r.specular_aa = self.specular_aa;
        r.foliage_aa = self.flora_hq;
        r.terrain_detail = self.terrain.distance();
        r.water_quality = self.water_hq;
        r.particle_quality = self.particles.factor();
        r.set_texture_lod(self.textures.lod());
        // le bicubique lit le niveau 0 : seulement en pleine résolution
        r.texture_hq = self.texture_hq && self.textures == TextureQuality::Ultra;
        r.texture_sharpness = self.texture_sharpness.clamp(0.0, 1.0);
        r.normal_maps = self.normal_maps;
        r.emissive = if self.emissive { self.emissive_strength.max(0.0) } else { 0.0 };
        r.brightness = self.brightness;
        r.post_settings.bloom = self.bloom.max(0.0);
        r.post_settings.ao = self.ao.clamp(0.0, 1.0);
        r.post_settings.ao_radius = self.ao_radius.max(0.02);
        match self.shadows.params() {
            Some((size, filter)) => {
                r.set_shadow_size(size);
                r.shadow_filter = filter;
                r.shadows = shadows_wanted;
            }
            None => r.shadows = false,
        }
    }

    /// Suréchantillonnage des exports (1 à 4) : les grandes images sont rendues par tuiles, la
    /// taille maximale de la carte graphique ne le limite plus.
    pub fn export_supersample(&self) -> u32 {
        self.supersample.clamp(1, 4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets() {
        let q = Quality::default();
        assert_eq!(q.matching_preset(), Some(Preset::Balanced));
        let f = Quality { brightness: 1.3, supersample: 3, ..Quality::default() }.for_export();
        assert_eq!((f.brightness, f.supersample), (1.3, 3));
        assert_eq!(f.matching_preset(), Some(Preset::Final));
        assert_eq!(f.export_supersample(), 3);
        assert_eq!(Quality { supersample: 9, ..f }.export_supersample(), 4);
        assert_eq!(Quality { msaa: 1, ..q }.matching_preset(), None);
        // anciennes configurations : champs manquants pris au défaut
        let partial: Quality = serde_json::from_str(r#"{"msaa":1}"#).unwrap();
        assert_eq!(partial, Quality { msaa: 1, ..Quality::default() });
        // configuration d'avant la flore, l'eau, le terrain : réglages « Équilibré »
        let old: Quality = serde_json::from_str(r#"{"msaa":4,"shadows":"Ultra","ao":1.0}"#).unwrap();
        assert!(old.flora_hq && old.water_hq && old.specular_aa && !old.fxaa);
        assert_eq!(old.terrain, TerrainQuality::High);
        assert_eq!(Preset::from_key("Aperçu"), Some(Preset::Preview));
    }
}
