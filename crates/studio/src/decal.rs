//! Décalcomanies des personnages (emblèmes de tenue, logos).
//!
//! Le matériau de personnage a un emplacement de décalcomanie (`forgelight::slot::DECAL`), lu avec
//! le 3e jeu d'UV : une projection plane dont le carré 0..1 tombe à l'endroit prévu sur l'armure.
//! Les `.adr` placent tous la décalcomanie au centre (0,5 ; 0,5), échelle 1, sans rotation
//! (`DecalTintTranslations`). La texture se pose par son alpha.

use crate::camo::{display_name as camo_display_name, TextureChoice};
use forgelight::GameData;
use serde::{Deserialize, Serialize};

/// Décalcomanie d'un acteur (enregistrée avec le projet).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Decal {
    pub choice: TextureChoice,
    /// 0 = invisible, 1 = opaque
    pub opacity: f32,
}

impl Default for Decal {
    fn default() -> Self {
        Self { choice: TextureChoice::None, opacity: 1.0 }
    }
}

impl Decal {
    pub fn is_active(&self) -> bool {
        self.choice != TextureChoice::None && self.opacity > 0.0
    }

    /// Texture à poser sur un maillage dont la décalcomanie par défaut est `mesh_default`.
    pub fn texture_for<'a>(&'a self, mesh_default: Option<&'a str>) -> Option<&'a str> {
        match &self.choice {
            TextureChoice::None => None,
            TextureChoice::Default => mesh_default.filter(|t| !is_blank(t)),
            TextureChoice::Texture(t) => Some(t.as_str()),
        }
    }

    pub fn label(&self) -> String {
        match &self.choice {
            TextureChoice::None => "Aucune".into(),
            TextureChoice::Default => "Par défaut de l'armure".into(),
            TextureChoice::Texture(t) => display_name(t),
        }
    }

    /// Option de ligne de commande : `aucune`, `defaut` ou un nom de texture.
    pub fn choice_from_arg(s: &str) -> TextureChoice {
        match s.to_ascii_lowercase().as_str() {
            "aucune" | "aucun" | "none" => TextureChoice::None,
            "defaut" | "défaut" | "default" => TextureChoice::Default,
            l if l.ends_with(".dds") => TextureChoice::Texture(s.to_string()),
            _ => TextureChoice::Texture(format!("{s}.dds")),
        }
    }
}

/// `white.dds` (VS, casques) occupe l'emplacement quand il n'y a pas de décalcomanie : ce carré
/// blanc opaque n'est pas à dessiner.
pub fn is_blank(texture: &str) -> bool {
    texture.eq_ignore_ascii_case("white.dds")
}

/// Nom lisible (`Decal_AGONcyan_128.dds` → `AGONcyan`).
pub fn display_name(texture: &str) -> String {
    let stem = texture.rsplit_once('.').map(|(a, _)| a).unwrap_or(texture);
    let words: Vec<&str> = stem
        .split('_')
        .filter(|w| !w.is_empty())
        .filter(|w| !w.eq_ignore_ascii_case("decal"))
        .filter(|w| !is_size(w))
        .collect();
    if words.is_empty() {
        camo_display_name(texture)
    } else {
        words.join(" ")
    }
}

/// `128`, `64x64`…
fn is_size(w: &str) -> bool {
    let w = w.to_ascii_lowercase();
    let mut parts = w.split('x');
    let ok = |p: Option<&str>| p.is_some_and(|p| matches!(p, "32" | "64" | "128" | "256"));
    match (parts.next(), parts.next(), parts.next()) {
        (a, None, None) => ok(a),
        (a, b, None) => ok(a) && ok(b),
        _ => false,
    }
}

/// Décalcomanies du jeu (`Decal_*.dds`), sans les déclinaisons 32 et 64 pixels des icônes.
pub fn catalog(gd: &GameData) -> Vec<String> {
    let mut v: Vec<String> = gd
        .index
        .names
        .values()
        .filter(|n| {
            let l = n.to_ascii_lowercase();
            l.starts_with("decal_")
                && l.ends_with(".dds")
                && !["_32.dds", "_64.dds", "_32x32.dds", "_64x64.dds"].iter().any(|s| l.ends_with(s))
        })
        .cloned()
        .collect();
    v.sort_by_key(|n| display_name(n).to_ascii_lowercase());
    v.dedup();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_choice() {
        assert_eq!(display_name("Decal_AGONcyan_128.dds"), "AGONcyan");
        assert_eq!(display_name("Decal_AnthemOutfit_001_128x128.dds"), "AnthemOutfit 001");
        assert_eq!(display_name("Decal_NS_Logo.dds"), "NS Logo");
        let d = Decal { choice: TextureChoice::Default, opacity: 1.0 };
        assert_eq!(d.texture_for(Some("white.dds")), None);
        assert_eq!(d.texture_for(Some("PowerHouse_3Bomb.dds")), Some("PowerHouse_3Bomb.dds"));
        assert_eq!(Decal::choice_from_arg("Decal_00"), TextureChoice::Texture("Decal_00.dds".into()));
        assert_eq!(serde_json::from_str::<Decal>("{}").unwrap(), Decal::default());
    }
}
