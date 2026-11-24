//! Flore du sol (herbes, fougères, quenouilles, nénuphars…) : instances déjà placées par le jeu,
//! rangées dans les chunks `<carte>_<a>_<b>.cnk0` (même enveloppe que les autres niveaux : en-tête
//! `CNK0`, puis LZHAM alpha dans une enveloppe zlib).
//!
//! Contenu décompressé : un nombre de tuiles de 64 m (u32, 16 : 4 × 4), puis pour chacune :
//! - x, y (i32 : rang de la tuile dans le chunk) et deux u32 inconnus ;
//! - ses écosystèmes (u32), chacun : numéro (u32, celui de [`crate::zone::Eco::index`]), puis
//!   une liste par couche de flore de l'écosystème (u32 + instances de 8 octets) ;
//! - un index (u32), une image (u32 ≠ 0 : taille u32 + DDS, la carte de mélange des
//!   écosystèmes de la tuile, pas encore lue), puis un dernier bloc (taille u32 + données).
//!
//! Vient ensuite le maillage fin du terrain (un lot de triangles par tuile, voir
//! [`parse_decompressed`]), sur lequel les plantes sont posées. Le reste n'est pas lu.
//!
//! Une instance : position compactée (u32 : triangle et coordonnées barycentriques) et teinte
//! (octets B, V, R, A, comme une couleur Direct3D). Voir [`FloraInstance`] et
//! [`FloraTile::place`]. Repère vérifié par `flcli flore` contre le terrain du `.cnk1`.

use anyhow::{ensure, Result};

/// Une plante posée par le jeu.
#[derive(Clone, Copy, Debug)]
pub struct FloraInstance {
    /// bits 0 à 13 : hauteur (1/32 m) ; bits 14 à 22 et 23 à 31 : position dans la tuile
    /// (pas de 64/512 m)
    pub packed: u32,
    /// teinte (octets B, V, R, A)
    pub tint: [u8; 4],
}

impl FloraInstance {
    /// Triangle du maillage fin de la tuile (bits 0 à 13).
    pub fn triangle(&self) -> usize {
        (self.packed & 0x3fff) as usize
    }
    /// Coordonnées barycentriques dans ce triangle (bits 14 à 22 et 23 à 31, sur 512 ; leur
    /// somme ne dépasse pas 1 : leurs histogrammes décroissent en ligne droite, corrélation −0,5).
    pub fn barycentric(&self) -> (f32, f32) {
        (((self.packed >> 14) & 0x1ff) as f32 / 512.0, (self.packed >> 23) as f32 / 512.0)
    }
}

/// Plantes d'un écosystème sur une tuile : une liste par couche de flore de l'écosystème
/// (dans l'ordre de ses couches, voir [`crate::zone::FloraLayer`]).
#[derive(Clone, Debug)]
pub struct TileEco {
    pub eco: u32,
    pub layers: Vec<Vec<FloraInstance>>,
}

/// Une tuile de 64 m du chunk.
#[derive(Clone, Debug)]
pub struct FloraTile {
    /// rang absolu de la tuile (en tuiles de 64 m) : `x` suit le z du monde, `y` le x
    pub x: i32,
    pub y: i32,
    pub ecos: Vec<TileEco>,
    /// maillage fin du terrain de la tuile (un sommet par mètre au plus près) : sommets (x, z
    /// dans la tuile de 0 à 64 m, hauteur en m) et triangles, que les plantes désignent
    pub vertices: Vec<[f32; 3]>,
    pub triangles: Vec<[u32; 3]>,
    /// carte de mélange des écosystèmes de la tuile (DDS), vide si absente
    pub blend_dds: Vec<u8>,
    /// dernier bloc de la tuile (non interprété)
    pub tail: Vec<u8>,
}

impl FloraTile {
    /// Position monde (x, hauteur, z, en m du jeu) d'une plante de cette tuile, sur son
    /// triangle ; `None` si le triangle n'existe pas.
    pub fn place(&self, p: &FloraInstance) -> Option<[f32; 3]> {
        let t = self.triangles.get(p.triangle())?;
        let [a, b, c] = t.map(|i| self.vertices.get(i as usize).copied());
        let (a, b, c) = (a?, b?, c?);
        let (u, v) = p.barycentric();
        let w = 1.0 - u - v;
        let local = [0, 1, 2].map(|k| a[k] * w + b[k] * u + c[k] * v);
        Some([self.y as f32 * 64.0 + local[0], local[2], self.x as f32 * 64.0 + local[1]])
    }
}

struct Reader<'a> {
    d: &'a [u8],
    p: usize,
}

impl Reader<'_> {
    fn u32(&mut self) -> Result<u32> {
        ensure!(self.p + 4 <= self.d.len(), "cnk0 tronqué à {:#x}", self.p);
        let v = u32::from_le_bytes(self.d[self.p..self.p + 4].try_into().unwrap());
        self.p += 4;
        Ok(v)
    }
    fn skip(&mut self, n: usize) -> Result<()> {
        ensure!(self.p + n <= self.d.len(), "cnk0 tronqué à {:#x}", self.p);
        self.p += n;
        Ok(())
    }
}

/// Lit un `.cnk0` complet (avec son en-tête).
pub fn parse_cnk0(file: &[u8]) -> Result<Vec<FloraTile>> {
    ensure!(file.len() >= 16 && &file[..4] == b"CNK0", "signature CNK0 absente");
    let out_len = u32::from_le_bytes(file[8..12].try_into().unwrap()) as usize;
    let comp_len = u32::from_le_bytes(file[12..16].try_into().unwrap()) as usize;
    ensure!(16 + comp_len <= file.len(), "cnk0 tronqué");
    let data = crate::lzham::decompress_zlib(&file[16..16 + comp_len], out_len)?;
    parse_decompressed(&data)
}

pub fn parse_decompressed(d: &[u8]) -> Result<Vec<FloraTile>> {
    let mut r = Reader { d, p: 0 };
    let n = r.u32()? as usize;
    ensure!(n <= 1024, "{n} tuiles");
    let mut tiles = Vec::with_capacity(n);
    for _ in 0..n {
        let x = r.u32()? as i32;
        let y = r.u32()? as i32;
        r.skip(8)?;
        let ne = r.u32()? as usize;
        ensure!(ne <= 256, "{ne} écosystèmes");
        let mut ecos = Vec::with_capacity(ne);
        for _ in 0..ne {
            let eco = r.u32()?;
            let nl = r.u32()? as usize;
            ensure!(nl <= 256, "{nl} couches de flore");
            let mut layers = Vec::with_capacity(nl);
            for _ in 0..nl {
                let k = r.u32()? as usize;
                ensure!(r.p + k * 8 <= d.len(), "{k} plantes : cnk0 tronqué");
                let inst = (0..k)
                    .map(|i| {
                        let o = r.p + i * 8;
                        FloraInstance {
                            packed: u32::from_le_bytes(d[o..o + 4].try_into().unwrap()),
                            tint: d[o + 4..o + 8].try_into().unwrap(),
                        }
                    })
                    .collect();
                r.p += k * 8;
                layers.push(inst);
            }
            ecos.push(TileEco { eco, layers });
        }
        let _index = r.u32()?;
        let mut blend_dds = Vec::new();
        if r.u32()? != 0 {
            let len = r.u32()? as usize;
            let o = r.p;
            r.skip(len)?;
            blend_dds = d[o..o + len].to_vec();
        }
        let len = r.u32()? as usize;
        let o = r.p;
        r.skip(len)?;
        let tail = d[o..o + len].to_vec();
        tiles.push(FloraTile { x, y, ecos, vertices: Vec::new(), triangles: Vec::new(), blend_dds, tail });
    }
    // maillage fin : sommets par côté (u32), grille de hauteurs (u32 + 4 octets chacune),
    // indices (u32 + u16), sommets (u32 + 16 octets : x, z de 0 à 64, hauteur lointaine et
    // proche en 1/32 m, puis 8 octets), lots (u32 + 16 octets : premier indice, nombre, premier
    // sommet, nombre), un par tuile dans l'ordre des tuiles
    r.u32()?;
    let nh = r.u32()? as usize;
    r.skip(nh * 4)?;
    let ni = r.u32()? as usize;
    ensure!(r.p + ni * 2 <= d.len(), "cnk0 : indices tronqués");
    let indices: Vec<u16> = d[r.p..r.p + ni * 2].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    r.p += ni * 2;
    let nv = r.u32()? as usize;
    ensure!(r.p + nv * 16 <= d.len(), "cnk0 : sommets tronqués");
    let verts: Vec<[f32; 3]> = d[r.p..r.p + nv * 16]
        .chunks_exact(16)
        .map(|c| {
            let s = |o: usize| i16::from_le_bytes([c[o], c[o + 1]]) as f32;
            [s(0), s(2), s(6) * crate::terrain::HEIGHT_SCALE]
        })
        .collect();
    r.p += nv * 16;
    let nb = r.u32()? as usize;
    for k in 0..nb.min(tiles.len()) {
        ensure!(r.p + 16 <= d.len(), "cnk0 : lots tronqués");
        let f = |o: usize| u32::from_le_bytes(d[r.p + o..r.p + o + 4].try_into().unwrap()) as usize;
        let (io, ic, vo, vc) = (f(0), f(4), f(8), f(12));
        r.p += 16;
        if io + ic > indices.len() || vo + vc > verts.len() {
            continue;
        }
        tiles[k].vertices = verts[vo..vo + vc].to_vec();
        tiles[k].triangles = indices[io..io + ic].chunks_exact(3).map(|t| [t[0] as u32, t[1] as u32, t[2] as u32]).collect();
    }
    Ok(tiles)
}
