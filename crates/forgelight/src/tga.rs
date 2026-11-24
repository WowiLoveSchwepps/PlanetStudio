//! Images TGA non compressées (type 2, 24 ou 32 bits) et compressées RLE (type 10), en RGBA8.
//! Le jeu s'en sert pour quelques petites textures d'ambiance (`ambient_*.tga`).

use crate::dds::Image;
use anyhow::{bail, Result};

pub fn decode(data: &[u8]) -> Result<Image> {
    if data.len() < 18 {
        bail!("TGA trop court");
    }
    let id_len = data[0] as usize;
    let cmap_type = data[1];
    let kind = data[2];
    let width = u16::from_le_bytes([data[12], data[13]]) as usize;
    let height = u16::from_le_bytes([data[14], data[15]]) as usize;
    let bpp = data[16] as usize;
    let top_down = data[17] & 0x20 != 0;
    if cmap_type != 0 || !matches!(kind, 2 | 10) || !matches!(bpp, 24 | 32) {
        bail!("TGA non géré (type {kind}, {bpp} bits, palette {cmap_type})");
    }
    let px = bpp / 8;
    let n = width * height;
    let mut src = &data[18 + id_len..];
    // pixels BGR(A) dans l'ordre du fichier
    let mut raw = Vec::with_capacity(n * px);
    if kind == 2 {
        if src.len() < n * px {
            bail!("TGA tronqué");
        }
        raw.extend_from_slice(&src[..n * px]);
    } else {
        while raw.len() < n * px {
            let (&h, rest) = src.split_first().ok_or_else(|| anyhow::anyhow!("TGA RLE tronqué"))?;
            let count = (h & 0x7f) as usize + 1;
            if h & 0x80 != 0 {
                let p = rest.get(..px).ok_or_else(|| anyhow::anyhow!("TGA RLE tronqué"))?;
                for _ in 0..count {
                    raw.extend_from_slice(p);
                }
                src = &rest[px..];
            } else {
                let p = rest.get(..count * px).ok_or_else(|| anyhow::anyhow!("TGA RLE tronqué"))?;
                raw.extend_from_slice(p);
                src = &rest[count * px..];
            }
        }
        raw.truncate(n * px);
    }
    let mut out = vec![0u8; n * 4];
    for y in 0..height {
        // par défaut, la première ligne du fichier est celle du bas
        let sy = if top_down { y } else { height - 1 - y };
        for x in 0..width {
            let s = &raw[(sy * width + x) * px..];
            let o = &mut out[(y * width + x) * 4..];
            o[0] = s[2];
            o[1] = s[1];
            o[2] = s[0];
            o[3] = if px == 4 { s[3] } else { 255 };
        }
    }
    Ok(Image::rgba(width as u32, height as u32, vec![out]))
}
