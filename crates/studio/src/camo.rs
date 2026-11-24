//! Camouflages des personnages.
//!
//! Dans les matériaux de personnage du jeu, le camouflage est une texture répétée, lue avec le
//! 2e jeu d'UV (un dépliage complet du corps), et appliquée là où le canal B de la carte de relief
//! `_N` est blanc : toute la tenue sauf les bottes, gants, ceinture, genouillères, emblèmes… Chaque
//! armure référence son camouflage par défaut (TR : `Tint_Digital_Camo`, NC : `AfricanForest_TC`,
//! VS : `ForestGreyscale_TC`).
//!
//! Le mélange exact du jeu est inconnu (son shader n'est pas dans les archives) : PlanetStudio
//! remplace la couleur par celle du motif en gardant l'ombrage et les plis peints dans `_C`.

use forgelight::GameData;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextureChoice {
    /// couleurs d'origine de la texture `_C`
    #[default]
    None,
    /// camouflage par défaut de chaque matériau
    Default,
    /// une texture de camouflage du jeu
    Texture(String),
}

/// Camouflage d'un acteur (enregistré avec le projet).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Camo {
    pub choice: TextureChoice,
    /// répétitions du motif sur le dépliage du corps
    pub scale: f32,
    /// 0 = couleurs d'origine, 1 = motif seul
    pub strength: f32,
}

impl Default for Camo {
    fn default() -> Self {
        Self { choice: TextureChoice::None, scale: 4.0, strength: 1.0 }
    }
}

impl Camo {
    pub fn is_active(&self) -> bool {
        self.choice != TextureChoice::None && self.strength > 0.0
    }

    /// Texture à utiliser pour un maillage dont le camouflage par défaut est `mesh_default`.
    pub fn texture_for<'a>(&'a self, mesh_default: Option<&'a str>) -> Option<&'a str> {
        match &self.choice {
            TextureChoice::None => None,
            TextureChoice::Default => mesh_default,
            TextureChoice::Texture(t) => Some(t.as_str()),
        }
    }

    pub fn label(&self) -> String {
        match &self.choice {
            TextureChoice::None => tr!("Aucun (couleurs d'origine)", "None (original colors)").into(),
            TextureChoice::Default => tr!("Par défaut de l'armure", "Armor default").into(),
            TextureChoice::Texture(t) => display_name(t),
        }
    }

    /// Lit une option de ligne de commande : `aucun`, `defaut` ou un nom de texture.
    pub fn choice_from_arg(s: &str) -> TextureChoice {
        match s.to_ascii_lowercase().as_str() {
            "aucun" | "none" => TextureChoice::None,
            "defaut" | "défaut" | "default" => TextureChoice::Default,
            _ if s.to_ascii_lowercase().ends_with(".dds") => TextureChoice::Texture(s.to_string()),
            _ => TextureChoice::Texture(format!("{s}.dds")),
        }
    }
}

/// Nom lisible d'une texture de camouflage (`DesertScrub_TC_02.dds` → `DesertScrub 02`).
pub fn display_name(texture: &str) -> String {
    let stem = texture.rsplit_once('.').map(|(a, _)| a).unwrap_or(texture);
    let mut words: Vec<&str> = stem
        .split(['_', '-'])
        .filter(|w| !w.is_empty())
        .filter(|w| !matches!(w.to_ascii_lowercase().as_str(), "tc" | "ps" | "camo" | "tint"))
        .collect();
    if words.is_empty() {
        words.push(stem);
    }
    words.join(" ")
}

/// Textures de camouflage présentes dans le jeu, triées par nom lisible.
///
/// Le jeu n'a pas de liste (les objets sont définis côté serveur) : on retient les `.dds` dont le
/// nom contient « camo » ou finit par `_TC`, sauf les icônes, affiches et textures de décor qui
/// suivent la même convention.
pub fn catalog(gd: &GameData) -> Vec<String> {
    let decor = ["icon_", "billboards", "common_structures_", "desolation_", "esamir_", "indar_", "amerish_", "hossin_"];
    let mut v: Vec<String> = gd
        .index
        .names
        .values()
        .filter(|n| {
            let l = n.to_ascii_lowercase();
            l.ends_with(".dds")
                && !decor.iter().any(|p| l.starts_with(p))
                && !(l.ends_with("_32.dds") || l.ends_with("_64.dds") || l.ends_with("_128.dds"))
                && (l.contains("camo") || is_tc(&l))
        })
        .cloned()
        .collect();
    v.sort_by_key(|n| display_name(n).to_ascii_lowercase());
    v.dedup();
    v
}

/// `…_tc.dds` ou `…_tc_02.dds`
fn is_tc(lower: &str) -> bool {
    let stem = lower.trim_end_matches(".dds");
    let stem = stem.trim_end_matches(|c: char| c.is_ascii_digit()).trim_end_matches('_');
    stem.ends_with("_tc")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(display_name("DesertScrub_TC_02.dds"), "DesertScrub 02");
        assert_eq!(display_name("Tint_Digital_Camo.dds"), "Digital");
        assert_eq!(display_name("Camo_TR_Triangulate_PS.dds"), "TR Triangulate");
        assert!(is_tc("africanforest_tc.dds") && is_tc("desertcamo_tc_02.dds") && !is_tc("tactical.dds"));
    }

    #[test]
    fn serde_and_choice() {
        let c = Camo { choice: TextureChoice::Texture("Zebra_TC.dds".into()), scale: 6.0, strength: 0.5 };
        let back: Camo = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back, c);
        assert_eq!(serde_json::from_str::<Camo>("{}").unwrap(), Camo::default());
        assert_eq!(Camo::choice_from_arg("defaut"), TextureChoice::Default);
        assert_eq!(Camo::choice_from_arg("Zebra_TC"), TextureChoice::Texture("Zebra_TC.dds".into()));
        assert_eq!(c.texture_for(Some("x.dds")), Some("Zebra_TC.dds"));
        assert_eq!(Camo { choice: TextureChoice::Default, ..c }.texture_for(Some("x.dds")), Some("x.dds"));
    }
}
