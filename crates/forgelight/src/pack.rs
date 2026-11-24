//! Archives `.pack2` (format "PAK\x01").
//!
//! En-tête (little endian) :
//!   magic "PAK\x01" | asset_count u32 | file_length u64 | map_offset u64 | 0x100 u64
//! Table (à map_offset), 32 octets par asset :
//!   name_hash u64 | offset u64 | data_length u64 | zip_flag u32 | data_crc u32
//! Données compressées : magic A1 B2 C3 D4 | taille décompressée u32 (BE) | flux zlib.

use crate::hash::name_hash;
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug)]
pub struct AssetEntry {
    pub pack: u16,
    pub name_hash: u64,
    pub offset: u64,
    pub length: u64,
    pub zip_flag: u32,
}

pub struct Pack2 {
    pub path: PathBuf,
    pub entries: Vec<AssetEntry>,
}

impl Pack2 {
    pub fn open(path: &Path, pack_id: u16) -> Result<Self> {
        let mut f = File::open(path).with_context(|| format!("ouverture {}", path.display()))?;
        let mut hdr = [0u8; 32];
        f.read_exact(&mut hdr)?;
        if &hdr[0..4] != b"PAK\x01" {
            bail!("{} : pas un fichier pack2", path.display());
        }
        let count = u32::from_le_bytes(hdr[4..8].try_into().unwrap()) as usize;
        let map_offset = u64::from_le_bytes(hdr[16..24].try_into().unwrap());
        f.seek(SeekFrom::Start(map_offset))?;
        let mut map = vec![0u8; count * 32];
        f.read_exact(&mut map)?;
        let entries = map
            .chunks_exact(32)
            .map(|c| AssetEntry {
                pack: pack_id,
                name_hash: u64::from_le_bytes(c[0..8].try_into().unwrap()),
                offset: u64::from_le_bytes(c[8..16].try_into().unwrap()),
                length: u64::from_le_bytes(c[16..24].try_into().unwrap()),
                zip_flag: u32::from_le_bytes(c[24..28].try_into().unwrap()),
            })
            .collect();
        Ok(Self { path: path.to_owned(), entries })
    }
}

/// Décompresse un bloc d'asset si nécessaire.
pub fn decode_asset(raw: Vec<u8>) -> Result<Vec<u8>> {
    if raw.len() >= 8 && raw[0..4] == [0xA1, 0xB2, 0xC3, 0xD4] {
        let size = u32::from_be_bytes(raw[4..8].try_into().unwrap()) as usize;
        let mut out = Vec::with_capacity(size);
        flate2::read::ZlibDecoder::new(&raw[8..])
            .read_to_end(&mut out)
            .context("décompression zlib")?;
        Ok(out)
    } else {
        Ok(raw)
    }
}

/// Index de tous les assets de tous les .pack2 d'un dossier.
pub struct AssetIndex {
    pub packs: Vec<PathBuf>,
    pub entries: HashMap<u64, AssetEntry>,
    /// hash -> nom (rempli par la namelist)
    pub names: HashMap<u64, String>,
}

impl AssetIndex {
    /// Ouvre tous les `.pack2` du dossier (ex: `<jeu>/Resources/Assets`).
    pub fn open_dir(dir: &Path) -> Result<Self> {
        let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
            .with_context(|| format!("lecture du dossier {}", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pack2")))
            .collect();
        paths.sort();
        if paths.is_empty() {
            bail!("aucun fichier .pack2 dans {}", dir.display());
        }
        let mut entries = HashMap::new();
        for (i, p) in paths.iter().enumerate() {
            let pack = Pack2::open(p, i as u16)?;
            for e in pack.entries {
                // en cas de doublon, le dernier pack (ordre alphabétique) gagne
                entries.insert(e.name_hash, e);
            }
        }
        Ok(Self { packs: paths, entries, names: HashMap::new() })
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn read_raw(&self, e: &AssetEntry) -> Result<Vec<u8>> {
        let mut f = File::open(&self.packs[e.pack as usize])?;
        f.seek(SeekFrom::Start(e.offset))?;
        let mut buf = vec![0u8; e.length as usize];
        f.read_exact(&mut buf)?;
        Ok(buf)
    }

    pub fn read_entry(&self, e: &AssetEntry) -> Result<Vec<u8>> {
        decode_asset(self.read_raw(e)?)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.entries.contains_key(&name_hash(name))
    }

    /// Lit un asset par son nom (insensible à la casse).
    pub fn read(&self, name: &str) -> Result<Vec<u8>> {
        let e = self
            .entries
            .get(&name_hash(name))
            .with_context(|| format!("asset introuvable : {name}"))?;
        self.read_entry(e)
    }

    /// Ajoute des noms connus (seuls ceux présents dans les packs sont gardés).
    pub fn add_names<I: IntoIterator<Item = String>>(&mut self, names: I) -> usize {
        let mut n = 0;
        for name in names {
            let h = name_hash(&name);
            if self.entries.contains_key(&h) && !self.names.contains_key(&h) {
                self.names.insert(h, name);
                n += 1;
            }
        }
        n
    }

    /// Noms connus, triés, filtrés par extension (ex: "adr").
    pub fn names_with_ext(&self, ext: &str) -> Vec<String> {
        let suffix = format!(".{}", ext.to_ascii_lowercase());
        let mut v: Vec<String> = self
            .names
            .values()
            .filter(|n| n.to_ascii_lowercase().ends_with(&suffix))
            .cloned()
            .collect();
        v.sort_by_key(|s| s.to_ascii_lowercase());
        v
    }
}
