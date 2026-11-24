//! Construction de la liste des noms de fichiers.
//!
//! Les .pack2 ne stockent que le CRC64 des noms. On retrouve les noms en
//! cherchant, dans le contenu de tous les assets, des chaînes du type
//! `Quelque_Chose.ext`, puis on garde celles dont le hash existe.

use crate::pack::AssetIndex;
use anyhow::Result;
use rayon::prelude::*;
use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

const EXTS: &[&str] = &[
    "dme", "dma", "adr", "agr", "dds", "mrn", "xml", "txt", "fxo", "gfx", "zone", "lst", "apx",
    "efb", "fsb", "bnk", "wem", "ttf", "png", "jpg", "tga", "ini", "json", "def", "prsb", "cnk0",
    "cnk1", "cnk2", "cnk3", "cnk4", "cnk5", "gcnk", "tome", "mat", "fxd", "ini", "lua", "dat",
    "playerstudio", "ogg", "wav", "mp3", "bin", "crc", "csv", "nsa", "ani", "skel", "pss",
];

fn is_name_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'-' || c == b'.' || c == b' '
}

/// Extrait des candidats de noms de fichiers d'un bloc binaire.
pub fn scrape(data: &[u8], out: &mut HashSet<String>) {
    let n = data.len();
    let mut i = 0;
    while i < n {
        if data[i] != b'.' {
            i += 1;
            continue;
        }
        // extension : lettres/chiffres après le point
        let mut j = i + 1;
        while j < n && data[j].is_ascii_alphanumeric() && j - i <= 13 {
            j += 1;
        }
        let ext = &data[i + 1..j];
        if ext.is_empty() || ext.len() > 12 {
            i += 1;
            continue;
        }
        let ext_l = String::from_utf8_lossy(ext).to_ascii_lowercase();
        if !EXTS.contains(&ext_l.as_str()) {
            i = j;
            continue;
        }
        // remonter le nom
        let mut s = i;
        while s > 0 && is_name_char(data[s - 1]) && i - s < 200 {
            s -= 1;
        }
        // retirer espaces de tête
        while s < i && data[s] == b' ' {
            s += 1;
        }
        if i > s {
            let name = String::from_utf8_lossy(&data[s..j]).to_string();
            out.insert(name.clone());
            // un préfixe avec des espaces peut contenir du texte parasite
            if let Some(p) = name.rfind(' ') {
                out.insert(name[p + 1..].to_string());
            }
        }
        i = j;
    }
}

/// Parcourt tous les assets (en parallèle) et retourne les noms trouvés.
pub fn build(index: &AssetIndex, progress: &AtomicUsize) -> HashSet<String> {
    let entries: Vec<_> = index.entries.values().copied().collect();
    entries
        .par_iter()
        .fold(HashSet::new, |mut acc, e| {
            progress.fetch_add(1, Ordering::Relaxed);
            if e.length < 256 * 1024 * 1024 {
                if let Ok(data) = index.read_entry(e) {
                    // les DDS/sons ne contiennent pas de noms : on évite de les scanner
                    let skip = data.starts_with(b"DDS ")
                        || data.starts_with(b"FSB5")
                        || data.starts_with(b"RIFF")
                        || data.starts_with(b"OggS");
                    if !skip {
                        scrape(&data, &mut acc);
                    }
                }
            }
            acc
        })
        .reduce(HashSet::new, |mut a, b| {
            a.extend(b);
            a
        })
}

/// Variantes dérivées (LOD, etc.) pour trouver plus de fichiers.
pub fn derive_variants(names: &HashSet<String>) -> HashSet<String> {
    let mut out = HashSet::new();
    for n in names {
        let lower = n.to_ascii_lowercase();
        if let Some(stem) = lower.strip_suffix(".dme").map(|_| &n[..n.len() - 4]) {
            for lod in 0..4 {
                let base = stem.trim_end_matches(|c: char| c.is_ascii_digit());
                let base = base
                    .strip_suffix("_LOD")
                    .or_else(|| base.strip_suffix("_lod"))
                    .or_else(|| base.strip_suffix("_Lod"))
                    .unwrap_or(stem);
                out.insert(format!("{base}_LOD{lod}.dme"));
                out.insert(format!("{base}_LOD{lod}.dma"));
            }
            out.insert(format!("{stem}.adr"));
        }
        if let Some(stem) = lower.strip_suffix(".adr").map(|_| &n[..n.len() - 4]) {
            out.insert(format!("{stem}_LOD0.dme"));
            out.insert(format!("{stem}.agr"));
        }
    }
    out
}

pub fn save(index: &AssetIndex, path: &Path) -> Result<()> {
    let mut names: Vec<&String> = index.names.values().collect();
    names.sort_by_key(|s| s.to_ascii_lowercase());
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    for n in names {
        writeln!(f, "{n}")?;
    }
    Ok(())
}

pub fn load(path: &Path) -> Result<Vec<String>> {
    let s = std::fs::read_to_string(path)?;
    Ok(s.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
}

/// Charge la namelist depuis le cache ou la reconstruit.
pub fn load_or_build(index: &mut AssetIndex, cache: &Path, progress: &AtomicUsize) -> Result<()> {
    let loaded = load(cache).map(|names| index.add_names(names)).unwrap_or(0) > 0;
    if !loaded {
        let mut found = build(index, progress);
        let variants = derive_variants(&found);
        found.extend(variants);
        index.add_names(found);
        if let Some(parent) = cache.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        save(index, cache)?;
    }
    load_or_guess_actors(index, &actors_cache_path(cache));
    Ok(())
}

/// Cache des noms d'acteurs devinés (à côté de la namelist).
fn actors_cache_path(cache: &Path) -> std::path::PathBuf {
    cache.with_file_name("namelist_acteurs.txt")
}

/// Ajoute les noms d'acteurs devinés, depuis leur cache ou en les cherchant (une seule fois).
fn load_or_guess_actors(index: &mut AssetIndex, path: &Path) {
    if let Ok(names) = load(path) {
        index.add_names(names);
        return;
    }
    let names = guess_actor_names(index);
    index.add_names(names.iter().cloned());
    let mut f = match std::fs::File::create(path) {
        Ok(f) => std::io::BufWriter::new(f),
        Err(_) => return,
    };
    for n in &names {
        let _ = writeln!(f, "{n}");
    }
}

/// Suffixes essayés après un préfixe du nom d'un modèle ou d'une texture de l'acteur : variantes
/// de faction (`_TR`, `_NC`, `_VS`…), d'état et de numéro.
const ACTOR_SUFFIXES: &[&str] = &[
    "", "_TR", "_NC", "_VS", "_NS", "_Neutral", "_Orange", "_Destroyed", "_Grey", "_Gray", "_Red", "_Blue",
    "_Purple", "_Yellow", "_Green", "_Off", "_On", "_Small", "_Large", "_Big", "_01", "_02", "_03", "_001",
    "_002", "_A", "_B",
];

/// Suffixes des textures (`_C`, `_N`…) retirés pour retrouver le nom de l'objet.
const TEXTURE_SUFFIXES: &[&str] = &["c", "n", "s", "ds", "dn", "dnm", "bm", "oc", "d", "tc"];

/// Radicaux des fichiers cités par un acteur : modèle, palette, textures, réseau d'animation,
/// sans extension ni suffixe de LOD ou de texture.
fn actor_stems(xml: &str) -> Vec<String> {
    let mut stems: Vec<String> = Vec::new();
    for attr in ["fileName=\"", "paletteName=\"", "textureName=\""] {
        for (i, _) in xml.match_indices(attr) {
            let rest = &xml[i + attr.len()..];
            let Some(end) = rest.find('"') else { continue };
            let Some((stem, ext)) = rest[..end].rsplit_once('.') else { continue };
            let lower = stem.to_ascii_lowercase();
            // `_LOD0`, `_Lod1`, `_LOD0_LODAuto`… : on coupe au premier `_lod<chiffre>`
            let cut = lower
                .match_indices("_lod")
                .find(|(j, _)| lower.as_bytes().get(j + 4).is_some_and(|c| c.is_ascii_digit()))
                .map(|(j, _)| j);
            let mut s = &stem[..cut.unwrap_or(stem.len())];
            if ext.eq_ignore_ascii_case("dds") {
                if let Some((head, suf)) = s.rsplit_once('_') {
                    if TEXTURE_SUFFIXES.contains(&suf.to_ascii_lowercase().as_str()) {
                        s = head;
                    }
                }
            }
            if s.contains('_') && !stems.iter().any(|t| t == s) {
                stems.push(s.to_string());
            }
        }
    }
    stems.sort_by_key(|s| std::cmp::Reverse(s.len()));
    stems
}

/// Devine le nom d'un acteur dont on connaît le hash, d'après les fichiers qu'il cite : un
/// préfixe d'un de leurs noms (découpé aux `_`), suivi d'un suffixe de [`ACTOR_SUFFIXES`].
pub fn guess_actor_name(hash: u64, xml: &str) -> Option<String> {
    for stem in actor_stems(xml) {
        let parts: Vec<&str> = stem.split('_').collect();
        for k in (2..=parts.len()).rev() {
            let prefix = parts[..k].join("_");
            for suf in ACTOR_SUFFIXES {
                let cand = format!("{prefix}{suf}.adr");
                if crate::hash::name_hash(&cand) == hash {
                    return Some(cand);
                }
            }
        }
    }
    None
}

/// Premiers octets décompressés d'un asset, lus à partir d'un début de son bloc brut.
fn head_bytes(raw_head: &[u8], n: usize) -> Vec<u8> {
    if raw_head.len() >= 8 && raw_head[0..4] == [0xA1, 0xB2, 0xC3, 0xD4] {
        let mut out = Vec::with_capacity(n);
        let mut dec = flate2::read::ZlibDecoder::new(&raw_head[8..]);
        let mut buf = [0u8; 64];
        while out.len() < n {
            match dec.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(k) => out.extend_from_slice(&buf[..k]),
            }
        }
        out
    } else {
        raw_head[..raw_head.len().min(n)].to_vec()
    }
}

/// Retrouve le nom des acteurs (`.adr`) que rien ne cite dans les fichiers du client : ceux que
/// le serveur fait apparaître (boucliers des bases aux couleurs des factions, portes IFF, tubes
/// de gravité, points de capture…). Leur nom se déduit des fichiers qu'ils citent eux-mêmes,
/// vérifié par le hash de l'index.
pub fn guess_actor_names(index: &AssetIndex) -> Vec<String> {
    use std::io::{Seek, SeekFrom};
    let mut by_pack: Vec<Vec<crate::pack::AssetEntry>> = vec![Vec::new(); index.packs.len()];
    for (h, e) in &index.entries {
        // un acteur est un petit XML
        if !index.names.contains_key(h) && e.length < 1 << 20 {
            by_pack[e.pack as usize].push(*e);
        }
    }
    let mut names: Vec<String> = by_pack
        .into_par_iter()
        .enumerate()
        .flat_map_iter(|(p, mut entries)| {
            entries.sort_by_key(|e| e.offset);
            let mut out = Vec::new();
            let Ok(mut f) = std::fs::File::open(&index.packs[p]) else { return out };
            for e in entries {
                let mut head = vec![0u8; (e.length as usize).min(96)];
                if f.seek(SeekFrom::Start(e.offset)).is_err() || f.read_exact(&mut head).is_err() {
                    continue;
                }
                let start = head_bytes(&head, 16);
                let start = start.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&start);
                if !start.starts_with(b"<ActorRuntime") {
                    continue;
                }
                let Ok(data) = index.read_entry(&e) else { continue };
                if let Some(n) = guess_actor_name(e.name_hash, &String::from_utf8_lossy(&data)) {
                    out.push(n);
                }
            }
            out
        })
        .collect();
    names.sort_by_key(|s| s.to_ascii_lowercase());
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Le nom d'une variante de faction se retrouve à partir de son modèle et de sa palette.
    #[test]
    fn guess_faction_actor() {
        let xml = r#"<ActorRuntime>
    <Base fileName="Common_Props_IFFDoor_SpawnRoomShield_OneWay_LOD0.dme" paletteName="Common_Props_IFFDoor_SpawnRoomShield_OneWay_TR_Lod0.dma" />
    <Alias aliasName="Default0" textureName="grey_reducespec25_OC.dds" />
</ActorRuntime>"#;
        let want = "Common_Props_IFFDoor_SpawnRoomShield_OneWay_TR.adr";
        assert_eq!(guess_actor_name(crate::hash::name_hash(want), xml).as_deref(), Some(want));
        assert_eq!(guess_actor_name(crate::hash::name_hash("Autre_Chose.adr"), xml), None);
    }
}
