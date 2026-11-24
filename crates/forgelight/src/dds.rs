//! Décodage des textures DDS (BC1/BC2/BC3/BC4/BC5/BC7, RGBA non compressé) en RGBA8.
//!
//! [`decode_compressed`] garde au contraire les blocs BC1/BC2/BC3/BC7 tels quels, pour les
//! envoyer compressés au GPU (comme le jeu) : 4 à 8 fois moins de mémoire, donc la pleine
//! résolution reste abordable.

use anyhow::{bail, Result};

pub struct Image {
    pub width: u32,
    pub height: u32,
    /// niveaux de mip, dans l'encodage `encoding` (RGBA8 sauf pour [`decode_compressed`]) ;
    /// chaque niveau contient les `layers` couches à la suite
    pub mips: Vec<Vec<u8>>,
    pub encoding: Encoding,
    /// couches : 1, ou 6 pour une cubemap lue par [`decode_compressed`] (faces +X, −X, +Y, −Y,
    /// +Z, −Z). Les textures de détail du jeu (`_DN`, `detail_cube`) rangent ainsi six motifs.
    pub layers: u32,
}

/// Encodage des niveaux de mip d'une [`Image`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Rgba8,
    Bc1,
    Bc2,
    Bc3,
    Bc7,
}

impl Encoding {
    /// Taille en octets d'un niveau de `w` × `h` pixels.
    pub fn level_size(self, w: u32, h: u32) -> usize {
        match self {
            Encoding::Rgba8 => Fmt::Rgba8.level_size(w, h),
            Encoding::Bc1 => Fmt::Bc1.level_size(w, h),
            Encoding::Bc2 => Fmt::Bc2.level_size(w, h),
            Encoding::Bc3 => Fmt::Bc3.level_size(w, h),
            Encoding::Bc7 => Fmt::Bc7.level_size(w, h),
        }
    }

    fn fmt(self) -> Fmt {
        match self {
            Encoding::Rgba8 => Fmt::Rgba8,
            Encoding::Bc1 => Fmt::Bc1,
            Encoding::Bc2 => Fmt::Bc2,
            Encoding::Bc3 => Fmt::Bc3,
            Encoding::Bc7 => Fmt::Bc7,
        }
    }
}

impl Image {
    pub fn rgba(width: u32, height: u32, mips: Vec<Vec<u8>>) -> Self {
        Self { width, height, mips, encoding: Encoding::Rgba8, layers: 1 }
    }

    /// Copie décompressée en RGBA8 (l'image elle-même si elle l'est déjà).
    pub fn to_rgba(&self) -> Result<Image> {
        let fmt = self.encoding.fmt();
        let (mut w, mut h) = (self.width, self.height);
        let mut mips = Vec::with_capacity(self.mips.len());
        for m in &self.mips {
            if fmt == Fmt::Rgba8 {
                mips.push(m.clone());
            } else {
                let size = fmt.level_size(w, h);
                let mut level = Vec::with_capacity((w * h * 4 * self.layers) as usize);
                for layer in m.chunks_exact(size) {
                    level.extend(decode_level(fmt, layer, w, h)?);
                }
                mips.push(level);
            }
            w = (w / 2).max(1);
            h = (h / 2).max(1);
        }
        Ok(Image { layers: self.layers, ..Image::rgba(self.width, self.height, mips) })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Fmt {
    Bc1,
    Bc2,
    Bc3,
    Bc4,
    Bc5,
    Bc7,
    Bgra8,
    Rgba8,
}

impl Fmt {
    fn level_size(self, w: u32, h: u32) -> usize {
        let bw = w.div_ceil(4) as usize;
        let bh = h.div_ceil(4) as usize;
        match self {
            Fmt::Bc1 | Fmt::Bc4 => bw * bh * 8,
            Fmt::Bc2 | Fmt::Bc3 | Fmt::Bc5 | Fmt::Bc7 => bw * bh * 16,
            Fmt::Bgra8 | Fmt::Rgba8 => (w * h * 4) as usize,
        }
    }
}

fn u32_at(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(d[o..o + 4].try_into().unwrap())
}

/// Cubemap complète (six faces) ?
fn is_cubemap(data: &[u8]) -> bool {
    data.len() >= 128 && u32_at(data, 112) & 0xFE00 == 0xFE00
}

/// En-tête : (largeur, hauteur, nombre de mips, format, début des données).
fn header(data: &[u8]) -> Result<(u32, u32, u32, Fmt, usize)> {
    if data.len() < 128 || &data[0..4] != b"DDS " {
        bail!("pas un fichier DDS");
    }
    let height = u32_at(data, 12);
    let width = u32_at(data, 16);
    let mip_count = u32_at(data, 28).max(1);
    let pf_flags = u32_at(data, 80);
    let fourcc = &data[84..88];
    let bitcount = u32_at(data, 88);
    let rmask = u32_at(data, 92);
    let mut offset = 128;
    let fmt = if pf_flags & 0x4 != 0 {
        match fourcc {
            b"DXT1" => Fmt::Bc1,
            b"DXT2" | b"DXT3" => Fmt::Bc2,
            b"DXT4" | b"DXT5" => Fmt::Bc3,
            b"ATI1" | b"BC4U" => Fmt::Bc4,
            b"ATI2" | b"BC5U" => Fmt::Bc5,
            b"DX10" => {
                let dxgi = u32_at(data, 128);
                offset += 20;
                match dxgi {
                    71 | 72 => Fmt::Bc1,
                    74 | 75 => Fmt::Bc2,
                    77 | 78 => Fmt::Bc3,
                    80 | 81 => Fmt::Bc4,
                    83 | 84 => Fmt::Bc5,
                    98 | 99 => Fmt::Bc7,
                    28 | 29 => Fmt::Rgba8,
                    87 | 91 => Fmt::Bgra8,
                    n => bail!("format DXGI {n} non supporté"),
                }
            }
            f => bail!("FourCC {:?} non supporté", String::from_utf8_lossy(f)),
        }
    } else if bitcount == 32 {
        if rmask == 0x00ff0000 { Fmt::Bgra8 } else { Fmt::Rgba8 }
    } else {
        bail!("format DDS non compressé {bitcount} bits non supporté");
    };
    Ok((width, height, mip_count, fmt, offset))
}

/// Niveaux de mip bruts (tant que les données suffisent).
fn raw_levels(data: &[u8], width: u32, height: u32, mip_count: u32, fmt: Fmt, mut offset: usize) -> Result<Vec<&[u8]>> {
    let mut levels = Vec::new();
    let (mut w, mut h) = (width, height);
    for _ in 0..mip_count {
        let size = fmt.level_size(w, h);
        if offset + size > data.len() {
            break;
        }
        levels.push(&data[offset..offset + size]);
        offset += size;
        if w == 1 && h == 1 {
            break;
        }
        w = (w / 2).max(1);
        h = (h / 2).max(1);
    }
    if levels.is_empty() {
        bail!("DDS tronqué");
    }
    Ok(levels)
}

/// Décode en RGBA8.
pub fn decode(data: &[u8]) -> Result<Image> {
    let (width, height, mip_count, fmt, offset) = header(data)?;
    let levels = raw_levels(data, width, height, mip_count, fmt, offset)?;
    let (mut w, mut h) = (width, height);
    let mut mips = Vec::with_capacity(levels.len());
    for src in levels {
        mips.push(decode_level(fmt, src, w, h)?);
        w = (w / 2).max(1);
        h = (h / 2).max(1);
    }
    Ok(Image::rgba(width, height, mips))
}

/// Garde les blocs BC1/BC2/BC3/BC7 compressés (dimensions multiples de 4) ; décode le reste
/// en RGBA8. BC4/BC5 sont décodés : le GPU les lirait en R / RG seuls, pas comme le reste du
/// rendu (gris répété, relief DXT5nm dans G et A).
pub fn decode_compressed(data: &[u8]) -> Result<Image> {
    let (width, height, mip_count, fmt, offset) = header(data)?;
    let encoding = match fmt {
        Fmt::Bc1 => Encoding::Bc1,
        Fmt::Bc2 => Encoding::Bc2,
        Fmt::Bc3 => Encoding::Bc3,
        Fmt::Bc7 => Encoding::Bc7,
        _ => return decode(data),
    };
    if width % 4 != 0 || height % 4 != 0 {
        return decode(data);
    }
    let levels = raw_levels(data, width, height, mip_count, fmt, offset)?;
    if !is_cubemap(data) {
        return Ok(Image { width, height, mips: levels.into_iter().map(<[u8]>::to_vec).collect(), encoding, layers: 1 });
    }
    // cubemap : chaque face a sa chaîne de mips complète, l'une après l'autre ; on regroupe les
    // six faces niveau par niveau
    let face: usize = (0..mip_count as usize)
        .map(|l| fmt.level_size((width >> l).max(1), (height >> l).max(1)))
        .sum();
    if levels.len() != mip_count as usize || offset + 6 * face > data.len() {
        bail!("cubemap tronquée");
    }
    let mut mips = Vec::with_capacity(levels.len());
    let mut at = 0;
    for l in 0..levels.len() {
        let size = fmt.level_size((width >> l).max(1), (height >> l).max(1));
        let mut level = Vec::with_capacity(6 * size);
        for f in 0..6 {
            let o = offset + f * face + at;
            level.extend_from_slice(&data[o..o + size]);
        }
        mips.push(level);
        at += size;
    }
    Ok(Image { width, height, mips, encoding, layers: 6 })
}

fn decode_level(fmt: Fmt, src: &[u8], w: u32, h: u32) -> Result<Vec<u8>> {
    let (wu, hu) = (w as usize, h as usize);
    let mut out = vec![0u8; wu * hu * 4];
    match fmt {
        Fmt::Rgba8 => out.copy_from_slice(&src[..wu * hu * 4]),
        Fmt::Bgra8 => {
            for (o, s) in out.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                o.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
            }
        }
        _ => {
            let mut px = vec![0u32; wu * hu];
            let r = match fmt {
                Fmt::Bc1 => texture2ddecoder::decode_bc1(src, wu, hu, &mut px),
                Fmt::Bc2 => texture2ddecoder::decode_bc2(src, wu, hu, &mut px),
                Fmt::Bc3 => texture2ddecoder::decode_bc3(src, wu, hu, &mut px),
                Fmt::Bc4 => texture2ddecoder::decode_bc4(src, wu, hu, &mut px),
                Fmt::Bc5 => texture2ddecoder::decode_bc5(src, wu, hu, &mut px),
                Fmt::Bc7 => texture2ddecoder::decode_bc7(src, wu, hu, &mut px),
                _ => unreachable!(),
            };
            if let Err(e) = r {
                bail!("décodage BC : {e}");
            }
            // texture2ddecoder produit des u32 BGRA (0xAARRGGBB)
            for (o, p) in out.chunks_exact_mut(4).zip(px) {
                let [b, g, r, a] = p.to_le_bytes();
                o.copy_from_slice(&[r, g, b, a]);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DDS DXT1 de `w` × `h`, `mips` niveaux ; cubemap si `cube` (face f remplie de l'octet f).
    fn dds(w: u32, h: u32, mips: u32, cube: bool) -> Vec<u8> {
        let mut d = vec![0u8; 128];
        d[0..4].copy_from_slice(b"DDS ");
        d[12..16].copy_from_slice(&h.to_le_bytes());
        d[16..20].copy_from_slice(&w.to_le_bytes());
        d[28..32].copy_from_slice(&mips.to_le_bytes());
        d[80..84].copy_from_slice(&4u32.to_le_bytes());
        d[84..88].copy_from_slice(b"DXT1");
        if cube {
            d[112..116].copy_from_slice(&0xFE00u32.to_le_bytes());
        }
        let face: usize = (0..mips).map(|l| Fmt::Bc1.level_size((w >> l).max(1), (h >> l).max(1))).sum();
        for f in 0..if cube { 6 } else { 1 } {
            d.extend(std::iter::repeat_n(f as u8, face));
        }
        d
    }

    #[test]
    fn compressed_cubemap_layers() {
        let img = decode_compressed(&dds(8, 8, 2, true)).unwrap();
        assert_eq!((img.encoding, img.layers, img.mips.len()), (Encoding::Bc1, 6, 2));
        // niveau 0 : 32 octets par face, niveau 1 : 8 ; faces à la suite
        assert_eq!((img.mips[0].len(), img.mips[1].len()), (6 * 32, 6 * 8));
        assert_eq!((img.mips[0][32], img.mips[0][5 * 32], img.mips[1][8]), (1, 5, 1));
        // décompression : quatre octets par pixel et par couche
        let rgba = img.to_rgba().unwrap();
        assert_eq!((rgba.layers, rgba.mips[0].len()), (6, 6 * 8 * 8 * 4));
        // une texture simple reste à une couche ; `decode` ne garde que la première face
        assert_eq!(decode_compressed(&dds(8, 8, 2, false)).unwrap().layers, 1);
        assert_eq!(decode(&dds(8, 8, 2, true)).unwrap().layers, 1);
    }
}
