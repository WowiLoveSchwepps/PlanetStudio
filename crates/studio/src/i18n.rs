//! Langue de l'interface : français ou anglais.
//!
//! Les textes restent écrits dans le code, en paires : `tr!("Fichier", "File")` rend le texte de
//! la langue active (`&'static str`), et `trf!("{n} images", "{n} frames")` (ou avec des arguments,
//! `trf!("{} images", "{} frames", n)`) le formate en `String`. Les deux textes d'une paire
//! restent côte à côte : une phrase ajoutée en français se traduit au même endroit.
//!
//! La langue est globale : l'interface egui est redessinée à chaque image, et un changement de
//! langue s'applique donc tout de suite. Les tests tournent en français (valeur par défaut).

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Lang {
    #[serde(rename = "fr")]
    Fr,
    #[serde(rename = "en")]
    En,
}

impl Lang {
    pub const ALL: [Lang; 2] = [Lang::Fr, Lang::En];

    /// Nom de la langue, écrit dans cette langue (il se lit donc quelle que soit la langue active).
    pub fn native_name(self) -> &'static str {
        match self {
            Lang::Fr => "Français",
            Lang::En => "English",
        }
    }

    /// Langue du système : le français pour un système en français, l'anglais sinon.
    pub fn from_system() -> Self {
        if system_is_french() {
            Lang::Fr
        } else {
            Lang::En
        }
    }
}

static ENGLISH: AtomicBool = AtomicBool::new(false);

pub fn set(lang: Lang) {
    ENGLISH.store(lang == Lang::En, Ordering::Relaxed);
}

pub fn current() -> Lang {
    if en() {
        Lang::En
    } else {
        Lang::Fr
    }
}

/// Vrai quand l'interface est en anglais.
#[inline]
pub fn en() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}

/// Texte de la langue active : `tr!("français", "english")`.
macro_rules! tr {
    ($fr:expr, $en:expr $(,)?) => {
        if $crate::i18n::en() {
            $en
        } else {
            $fr
        }
    };
}

/// Texte formaté de la langue active : `trf!("{n} images", "{n} frames")`, ou
/// `trf!("{} images", "{} frames", n)`. Les deux formats reçoivent les mêmes arguments.
macro_rules! trf {
    ($fr:literal, $en:literal $(, $arg:expr)* $(,)?) => {
        if $crate::i18n::en() {
            format!($en $(, $arg)*)
        } else {
            format!($fr $(, $arg)*)
        }
    };
}

#[cfg(windows)]
fn system_is_french() -> bool {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetUserDefaultUILanguage() -> u16;
    }
    // identifiant de langue Windows : les 10 bits du bas donnent la langue (0x0C : français)
    (unsafe { GetUserDefaultUILanguage() } & 0x3ff) == 0x0c
}

#[cfg(not(windows))]
fn system_is_french() -> bool {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|v| std::env::var(v).ok().filter(|s| !s.is_empty()))
        .is_some_and(|s| s.starts_with("fr"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn pairs() {
        // les tests tournent en français : la langue n'est changée nulle part ailleurs
        assert_eq!(tr!("Fichier", "File"), "Fichier");
        let n = 3;
        assert_eq!(trf!("{n} images", "{n} frames"), "3 images");
        assert_eq!(trf!("{} sur {}", "{} of {}", 1, 2), "1 sur 2");
    }
}
