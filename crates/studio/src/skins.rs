//! Cosmétiques des soldats (« skins ») : pièces d'armure et casques du jeu.
//!
//! Une pièce d'armure se pose par-dessus le corps de base de la classe (fusion d'os), comme le
//! casque : `Armor_TR_Male_HeavyAssault_Base` est la combinaison, avec les bottes et les gants, et
//! `Armor_TR_Male_HeavyAssault_Look001Upgrade02` ou `Armor_TR_Skins_Male_HeavyAssault_PS_AVA`
//! ajoutent plastrons, épaulières et jambières. Le jeu n'a pas de liste de ces objets (définis côté
//! serveur) : on les retrouve par leur nom.
//!
//! - armures : `Armor_<faction>_<sexe>_<classe>_*` et `Armor_<faction>_Skins_<sexe>_<classe>_*`,
//!   robots NSO `Armor_Common_Male_Skins_<classe>_*` ;
//! - casques : `Helmet_<faction>_<sexe>_<classe ou All>_*`, robots NSO `Helmet_Common_Male_*` et
//!   `Helmet_NSO_Male_*`.
//!
//! Les noms de classe varient (`Medic` / `CombatMedic`, `Stealth` / `Infiltrator` / `Infil01`,
//! `Heavy01`, et même `HeavyAssualt`) : on compare donc des débuts de mots. Les corps `*Base` et
//! les modèles d'interface `*_UI` sont écartés.
//!
//! Les MAX (classe `Max`, hommes seulement) ont leur propre squelette : leur corps est
//! `Armor_<faction>_Male_Max_Look001` (robot NSO : `Armor_Common_Male_MAX_Base`), sur lequel se
//! posent les améliorations `Look001UpgradeNN` et les skins `Armor_<faction>_Skins_Male_MAX_*`. Ils
//! n'apparaissent que dans les listes de la classe `Max`.

/// Choix d'une pièce pour le créateur de soldat.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SkinChoice {
    /// pièce par défaut (casque Look001, armure de classe `_001` du robot NSO, rien pour l'armure
    /// d'un humain)
    #[default]
    Default,
    /// pas de pièce
    None,
    /// un acteur du jeu
    Named(String),
}

impl SkinChoice {
    pub fn label(&self, default: &str, none: &str) -> String {
        match self {
            SkinChoice::Default => default.to_string(),
            SkinChoice::None => none.to_string(),
            SkinChoice::Named(n) => display_name(n),
        }
    }
}

/// Débuts de mot qui désignent une classe dans les noms du jeu (en minuscules).
fn class_aliases(class: &str) -> &'static [&'static str] {
    match class {
        "HeavyAssault" => &["heavy"],
        "LightAssault" => &["light"],
        "CombatMedic" => &["combatmedic", "medic"],
        "Engineer" => &["engi"],
        "Stealth" => &["stealth", "infil"],
        "Max" => &["max"],
        _ => &[],
    }
}

const ALL_CLASSES: [&str; 5] = ["HeavyAssault", "LightAssault", "CombatMedic", "Engineer", "Stealth"];

/// Le mot `token` désigne-t-il une classe ? `None` : aucune ; `Some(true)` : la classe demandée
/// (toute classe d'infanterie si `class` est vide) ; `Some(false)` : une autre (le MAX n'est
/// retenu que pour la classe `Max`).
fn class_token(token: &str, class: &str) -> Option<bool> {
    let t = token.to_ascii_lowercase();
    if t.starts_with("max") {
        return Some(class == "Max");
    }
    let is = |c: &str| class_aliases(c).iter().any(|a| t.starts_with(a));
    if !ALL_CLASSES.iter().any(|c| is(c)) {
        return None;
    }
    Some(if class.is_empty() { true } else { is(class) })
}

fn stem(name: &str) -> &str {
    name.rsplit_once('.').map(|(a, _)| a).unwrap_or(name)
}

/// Mots qui suivent `prefix` (en minuscules) dans `name`, s'il commence par lui.
fn rest_after<'a>(name: &'a str, prefix: &str) -> Option<&'a str> {
    let s = stem(name);
    (s.len() > prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix)).then(|| &s[prefix.len()..])
}

fn first_word(rest: &str) -> &str {
    rest.split('_').next().unwrap_or("")
}

fn excluded(name: &str) -> bool {
    let l = stem(name).to_ascii_lowercase();
    l.ends_with("_ui") || l.contains("nullparent")
}

/// Le nom contient-il un mot `Max*` (`Helmet_Common_Male_All_MAX_001`) ?
fn has_max_word(name: &str) -> bool {
    stem(name).split('_').any(|w| w.to_ascii_lowercase().starts_with("max"))
}

/// Corps d'un MAX de la faction (voir l'en-tête du module).
pub fn max_body(faction: &str) -> String {
    if faction == "NSO" {
        "Armor_Common_Male_MAX_Base.adr".into()
    } else {
        format!("Armor_{faction}_Male_Max_Look001.adr")
    }
}

/// Armes par défaut d'un MAX (gauche, droite), posées sur les os `L_WEAPON` et `R_WEAPON`. Le
/// Defector NSO n'en a pas : il garde ses mains, comme dans le jeu.
pub fn max_weapons(faction: &str) -> Vec<String> {
    if faction == "NSO" {
        Vec::new()
    } else {
        ["Left", "Right"].iter().map(|side| format!("Weapon_{faction}_MaxAntiInfantry{side}001.adr")).collect()
    }
}

/// Pièces d'armure d'une faction (`TR`, `NC`, `VS`, `NSO`), d'un sexe (`Male`, `Female`) et d'une
/// classe (nom de [`ALL_CLASSES`], ou vide pour toutes), triées : armures de classe puis skins.
pub fn armors(names: &[String], faction: &str, sex: &str, class: &str) -> Vec<String> {
    let prefixes: Vec<String> = if faction == "NSO" {
        vec!["Armor_Common_Male_Skins_".into()]
    } else {
        vec![format!("Armor_{faction}_{sex}_"), format!("Armor_{faction}_Skins_{sex}_")]
    };
    let mut v: Vec<String> = names
        .iter()
        .filter(|n| {
            let l = stem(n).to_ascii_lowercase();
            !excluded(n) && !l.ends_with("base") && !l.ends_with("_max_look001")
        })
        .filter(|n| {
            prefixes
                .iter()
                .any(|p| rest_after(n, p).is_some_and(|r| class_token(first_word(r), class) == Some(true)))
        })
        .cloned()
        .collect();
    v.sort_by_key(|n| n.to_ascii_lowercase());
    v.dedup();
    v
}

/// Casques d'une faction, d'un sexe et d'une classe : ceux de la classe et ceux de toutes les
/// classes (`All`, éditions spéciales), triés.
pub fn helmets(names: &[String], faction: &str, sex: &str, class: &str) -> Vec<String> {
    let prefixes: Vec<String> = if faction == "NSO" {
        vec!["Helmet_Common_Male_".into(), "Helmet_NSO_Male_".into()]
    } else {
        vec![format!("Helmet_{faction}_{sex}_")]
    };
    let mut v: Vec<String> = names
        .iter()
        // casques MAX pour la classe `Max` seulement, et rien qu'eux
        .filter(|n| !excluded(n) && has_max_word(n) == (class == "Max"))
        .filter(|n| {
            prefixes.iter().any(|p| {
                rest_after(n, p).is_some_and(|r| class_token(first_word(r), class).unwrap_or(true))
            })
        })
        .cloned()
        .collect();
    v.sort_by_key(|n| n.to_ascii_lowercase());
    v.dedup();
    v
}

/// Nom lisible : `Armor_TR_Skins_Male_HeavyAssault_PS_AVA.adr` → `Skin HeavyAssault PS AVA`,
/// `Helmet_TR_Male_All_Look005Upgrade01.adr` → `Look005Upgrade01`.
pub fn display_name(name: &str) -> String {
    let s = stem(name);
    let words: Vec<&str> = s.split('_').filter(|w| !w.is_empty()).collect();
    let skip = |w: &str| {
        matches!(
            w.to_ascii_lowercase().as_str(),
            "armor" | "helmet" | "tr" | "nc" | "vs" | "nso" | "common" | "male" | "female" | "all" | "skins"
        )
    };
    let skin = words.iter().any(|w| w.eq_ignore_ascii_case("skins")) && !words.iter().any(|w| w.eq_ignore_ascii_case("common"));
    let rest: Vec<&str> = words.into_iter().filter(|w| !skip(w)).collect();
    let body = if rest.is_empty() { s.to_string() } else { rest.join(" ") };
    if skin {
        format!("Skin {body}")
    } else {
        body
    }
}

/// Faction, sexe et classe d'un soldat d'après son corps de base (`Armor_TR_Male_HeavyAssault_Base`,
/// `Armor_VS_Female_StealthBase`, `Armor_NC_Male_Base`, robot `Armor_Common_Male_Skins_Base`). La
/// classe vaut `""` pour la tenue « Base » et pour le robot NSO, dont le corps ne dit pas la
/// classe. `None` si ce n'est pas un corps de soldat.
pub fn soldier_kind(body: &str) -> Option<(&'static str, &'static str, &'static str)> {
    let s = stem(body).to_ascii_lowercase();
    if s == "armor_common_male_skins_base" {
        return Some(("NSO", "Male", ""));
    }
    if s == "armor_common_male_max_base" {
        return Some(("NSO", "Male", "Max"));
    }
    let rest = s.strip_prefix("armor_")?;
    let faction = ["TR", "NC", "VS"].into_iter().find(|f| rest.starts_with(&format!("{}_", f.to_ascii_lowercase())))?;
    let rest = &rest[3..];
    let sex = ["Female", "Male"].into_iter().find(|x| rest.starts_with(&format!("{}_", x.to_ascii_lowercase())))?;
    let rest = &rest[sex.len() + 1..];
    let class = match rest {
        "base" => "",
        "max_look001" => "Max",
        "stealthbase" | "infiltrator_base" => "Stealth",
        _ => {
            let c = rest.strip_suffix("_base")?;
            ALL_CLASSES.into_iter().find(|k| k.eq_ignore_ascii_case(c))?
        }
    };
    Some((faction, sex, class))
}

/// Pièce d'armure (`Armor_*`) ou casque (`Helmet_*`) fusionnable sur un soldat.
pub fn is_armor_piece(asset: &str) -> bool {
    asset.to_ascii_lowercase().starts_with("armor_") && soldier_kind(asset).is_none()
}

pub fn is_helmet(asset: &str) -> bool {
    asset.to_ascii_lowercase().starts_with("helmet_")
}

/// Indice pseudo-aléatoire dans `0..n` (pour le bouton « au hasard »).
pub fn random_index(n: usize) -> usize {
    use std::hash::{BuildHasher, Hasher};
    if n == 0 {
        return 0;
    }
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    (h.finish() % n as u64) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Vec<String> {
        [
            "Armor_TR_Male_Base.adr",
            "Armor_TR_Male_HeavyAssault_Base.adr",
            "Armor_TR_Male_HeavyAssault_Base_UI.adr",
            "Armor_TR_Male_HeavyAssault_Look001.adr",
            "Armor_TR_Male_HeavyAssault_Look001Upgrade02_Auraxium.adr",
            "Armor_TR_Male_Max_Look001.adr",
            "Armor_TR_Male_Max_Look001Upgrade02.adr",
            "Armor_Common_Male_MAX_Base.adr",
            "Armor_TR_Male_StealthBase.adr",
            "Armor_TR_Male_Stealth_Look001.adr",
            "Armor_TR_Male_CombatMedic_Look001.adr",
            "Armor_TR_Skins_Male_HeavyAssault_PS_AVA.adr",
            "Armor_TR_Skins_Male_HeavyAssualt_Typo.adr",
            "Armor_TR_Skins_Male_Medic_Arbiter.adr",
            "Armor_TR_Skins_Male_Infiltrator_PS_AVA.adr",
            "Armor_TR_Skins_Male_MAX_Havoc.adr",
            "Armor_TR_Skins_Female_HeavyAssault_PS_AVA.adr",
            "Armor_NC_Male_HeavyAssault_Look001.adr",
            "Armor_Common_Male_Skins_Base.adr",
            "Armor_Common_Male_Skins_HeavyAssault_001.adr",
            "Armor_Common_Male_Skins_Heavy01_Aurax.adr",
            "Armor_Common_Male_Skins_Medic_001.adr",
            "Armor_Common_Male_Skins_MAX01.adr",
            "Helmet_TR_Male_All_Look005.adr",
            "Helmet_TR_Male_HeavyAssault_Look001.adr",
            "Helmet_TR_Male_Engineer_Look001.adr",
            "Helmet_TR_Male_Max_Look001.adr",
            "Helmet_TR_Male_Max_PS_Avalon.adr",
            "Helmet_TR_Male_Anni6.adr",
            "Helmet_TR_Female_All_Look005.adr",
            "Helmet_Common_Male_All_Look001.adr",
            "Helmet_Common_Male_All_MAX_001.adr",
            "Helmet_Common_Male_Bot_PS_Operative.adr",
            "Helmet_NSO_Male_All_7thAnni.adr",
            "Helmet_Common_Female_All_7thAnni.adr",
            "Helmet_Male_NullParent.adr",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    #[test]
    fn armor_lists() {
        let n = names();
        assert_eq!(
            armors(&n, "TR", "Male", "HeavyAssault"),
            [
                "Armor_TR_Male_HeavyAssault_Look001.adr",
                "Armor_TR_Male_HeavyAssault_Look001Upgrade02_Auraxium.adr",
                "Armor_TR_Skins_Male_HeavyAssault_PS_AVA.adr",
                "Armor_TR_Skins_Male_HeavyAssualt_Typo.adr",
            ]
        );
        assert_eq!(
            armors(&n, "TR", "Male", "Stealth"),
            ["Armor_TR_Male_Stealth_Look001.adr", "Armor_TR_Skins_Male_Infiltrator_PS_AVA.adr"]
        );
        assert_eq!(
            armors(&n, "TR", "Male", "CombatMedic"),
            ["Armor_TR_Male_CombatMedic_Look001.adr", "Armor_TR_Skins_Male_Medic_Arbiter.adr"]
        );
        // classe « Base » : toutes les classes d'infanterie, sans MAX ni corps de base
        assert_eq!(armors(&n, "TR", "Male", "").len(), 8);
        assert_eq!(armors(&n, "TR", "Female", "HeavyAssault"), ["Armor_TR_Skins_Female_HeavyAssault_PS_AVA.adr"]);
        assert_eq!(
            armors(&n, "NSO", "Male", "HeavyAssault"),
            ["Armor_Common_Male_Skins_Heavy01_Aurax.adr", "Armor_Common_Male_Skins_HeavyAssault_001.adr"]
        );
    }

    #[test]
    fn max_lists() {
        let n = names();
        // le corps Look001 n'est pas une pièce : seules les améliorations et les skins
        assert_eq!(armors(&n, "TR", "Male", "Max"), ["Armor_TR_Male_Max_Look001Upgrade02.adr", "Armor_TR_Skins_Male_MAX_Havoc.adr"]);
        assert_eq!(armors(&n, "NSO", "Male", "Max"), ["Armor_Common_Male_Skins_MAX01.adr"]);
        assert_eq!(helmets(&n, "TR", "Male", "Max"), ["Helmet_TR_Male_Max_Look001.adr", "Helmet_TR_Male_Max_PS_Avalon.adr"]);
        assert_eq!(helmets(&n, "NSO", "Male", "Max"), ["Helmet_Common_Male_All_MAX_001.adr"]);
        assert_eq!(max_body("VS"), "Armor_VS_Male_Max_Look001.adr");
        assert_eq!(max_weapons("NC")[1], "Weapon_NC_MaxAntiInfantryRight001.adr");
        assert!(max_weapons("NSO").is_empty());
    }

    #[test]
    fn helmet_lists() {
        let n = names();
        assert_eq!(
            helmets(&n, "TR", "Male", "HeavyAssault"),
            ["Helmet_TR_Male_All_Look005.adr", "Helmet_TR_Male_Anni6.adr", "Helmet_TR_Male_HeavyAssault_Look001.adr"]
        );
        assert_eq!(helmets(&n, "TR", "Male", "").len(), 4);
        assert_eq!(
            helmets(&n, "NSO", "Male", "Engineer"),
            [
                "Helmet_Common_Male_All_Look001.adr",
                "Helmet_Common_Male_Bot_PS_Operative.adr",
                "Helmet_NSO_Male_All_7thAnni.adr"
            ]
        );
    }

    #[test]
    fn labels() {
        assert_eq!(display_name("Armor_TR_Skins_Male_HeavyAssault_PS_AVA.adr"), "Skin HeavyAssault PS AVA");
        assert_eq!(display_name("Armor_TR_Male_HeavyAssault_Look001Upgrade02_Auraxium.adr"), "HeavyAssault Look001Upgrade02 Auraxium");
        assert_eq!(display_name("Helmet_TR_Male_All_Look005.adr"), "Look005");
        assert_eq!(display_name("Armor_Common_Male_Skins_Heavy01_Aurax.adr"), "Heavy01 Aurax");
        assert_eq!(SkinChoice::None.label("d", "aucun"), "aucun");
        assert_eq!(soldier_kind("Armor_TR_Male_HeavyAssault_Base.adr"), Some(("TR", "Male", "HeavyAssault")));
        assert_eq!(soldier_kind("Armor_VS_Female_StealthBase.adr"), Some(("VS", "Female", "Stealth")));
        assert_eq!(soldier_kind("Armor_NC_Female_CombatMedic_Base.adr"), Some(("NC", "Female", "CombatMedic")));
        assert_eq!(soldier_kind("Armor_NC_Male_Base.adr"), Some(("NC", "Male", "")));
        assert_eq!(soldier_kind("Armor_Common_Male_Skins_Base.adr"), Some(("NSO", "Male", "")));
        assert_eq!(soldier_kind("Armor_TR_Male_HeavyAssault_Look001.adr"), None);
        assert_eq!(soldier_kind("Armor_TR_Male_Max_Base.adr"), None);
        assert_eq!(soldier_kind("Armor_TR_Male_Max_Look001.adr"), Some(("TR", "Male", "Max")));
        assert_eq!(soldier_kind("Armor_Common_Male_MAX_Base.adr"), Some(("NSO", "Male", "Max")));
        assert!(!is_armor_piece("Armor_VS_Male_Max_Look001.adr"));
        assert!(is_armor_piece("Armor_TR_Skins_Male_HeavyAssault_PS_AVA.adr") && !is_armor_piece("Armor_TR_Male_Base.adr"));
        assert!(random_index(5) < 5);
    }
}
