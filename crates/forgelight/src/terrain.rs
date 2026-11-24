//! Terrain des cartes : chunks `<carte>_<a>_<b>.cnk<n>` (n = 1 à 5, niveaux de détail).
//!
//! En-tête (16 octets) : `CNK<n>`, version (u32), taille décompressée (u32), taille compressée
//! (u32), puis un flux LZHAM alpha dans une enveloppe zlib (voir [`crate::lzham`]).
//!
//! Contenu décompressé :
//! - 4 textures (une par quart du chunk), chacune 6 blocs préfixés par leur taille (u32) : la
//!   couleur (`ColorNX`, DDS : couleur RGB, normale X dans l'alpha), le spéculaire
//!   (`SpecNY`, DDS : normale Y dans l'alpha) et 4 petits blocs : les numéros des écosystèmes
//!   présents sur chaque tuile de 64 m du quart (voir [`crate::zone::Eco`]) ;
//! - sommets par côté d'un quart (u32, 65), puis une grille de hauteurs (u32 + 4 octets par
//!   point : i16 hauteur, 2 × u8) ;
//! - indices (u32 + u16 chacun), relatifs au premier sommet du lot ;
//! - sommets (u32 + 12 octets chacun) : x, y (i16, 0 à 64 dans le quart), hauteur lointaine,
//!   hauteur proche (i16, en 1/32 m) et une couleur (4 × u8) ;
//! - lots de rendu (u32 + 16 octets : premier indice, nombre d'indices, premier sommet, nombre
//!   de sommets), un par quart (les suivants sont vides).
//!
//! Un chunk de niveau n couvre 4 · 2^(n-1) tuiles de 64 m (256 m pour `.cnk1`, avec un sommet tous
//! les 2 m) ; ses noms avancent de ce pas, en tuiles. Le repère (quel quart, quel axe) est décrit
//! par [`quad_origin`], vérifié contre les objets posés au sol.

use anyhow::{ensure, Context, Result};

/// Un sommet du terrain, dans son quart.
#[derive(Clone, Copy, Debug)]
pub struct TerrainVertex {
    pub x: i16,
    pub y: i16,
    pub height_far: i16,
    pub height_near: i16,
    pub color: [u8; 4],
}

#[derive(Clone, Debug, Default)]
pub struct Quad {
    /// texture de couleur (DDS) et spéculaire (DDS)
    pub color_dds: Vec<u8>,
    pub spec_dds: Vec<u8>,
    /// écosystèmes présents, par tuile de 64 m du quart (numéros)
    pub ecos: Vec<Vec<u8>>,
    pub vertices: Vec<TerrainVertex>,
    pub indices: Vec<u16>,
}

#[derive(Clone, Debug)]
pub struct Chunk {
    pub lod: u32,
    /// sommets par côté d'un quart (65)
    pub verts_per_side: u32,
    /// les 4 quarts, dans l'ordre du fichier
    pub quads: Vec<Quad>,
}

/// Hauteur stockée → mètres.
pub const HEIGHT_SCALE: f32 = 1.0 / 32.0;

struct Reader<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        ensure!(self.p + n <= self.d.len(), "chunk tronqué à {:#x}", self.p);
        let s = &self.d[self.p..self.p + n];
        self.p += n;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn blob(&mut self) -> Result<&'a [u8]> {
        let n = self.u32()? as usize;
        self.take(n)
    }
}

/// Nom d'un chunk : `a` et `b` en tuiles de 64 m (pas de 4 · 2^(lod-1)).
pub fn chunk_name(zone: &str, a: i32, b: i32, lod: u32) -> String {
    format!("{zone}_{a}_{b}.cnk{lod}")
}

/// Côté d'un chunk de ce niveau, en tuiles de 64 m.
pub fn chunk_tiles(lod: u32) -> i32 {
    4 << (lod.max(1) - 1)
}

impl Chunk {
    /// Lit un chunk compressé (`.cnk1` à `.cnk5`, fichier complet avec son en-tête).
    pub fn parse(file: &[u8]) -> Result<Self> {
        ensure!(file.len() >= 16 && &file[..3] == b"CNK", "signature CNK absente");
        let lod = (file[3] as char).to_digit(10).context("niveau de détail illisible")?;
        ensure!((1..=5).contains(&lod), "CNK{lod} : seuls les niveaux 1 à 5 sont gérés");
        let out_len = u32::from_le_bytes(file[8..12].try_into().unwrap()) as usize;
        let comp_len = u32::from_le_bytes(file[12..16].try_into().unwrap()) as usize;
        ensure!(16 + comp_len <= file.len(), "chunk tronqué");
        let data = crate::lzham::decompress_zlib(&file[16..16 + comp_len], out_len)?;
        Self::parse_decompressed(lod, &data)
    }

    pub fn parse_decompressed(lod: u32, d: &[u8]) -> Result<Self> {
        let mut r = Reader { d, p: 0 };
        let ntex = r.u32()? as usize;
        ensure!(ntex <= 64, "{ntex} textures");
        let mut quads: Vec<Quad> = Vec::with_capacity(ntex);
        for _ in 0..ntex {
            let color_dds = r.blob()?.to_vec();
            let spec_dds = r.blob()?.to_vec();
            let mut ecos = Vec::with_capacity(4);
            for _ in 0..4 {
                ecos.push(r.blob()?.to_vec());
            }
            quads.push(Quad { color_dds, spec_dds, ecos, ..Default::default() });
        }
        let verts_per_side = r.u32()?;
        let nh = r.u32()? as usize;
        r.take(nh * 4)?;
        let ni = r.u32()? as usize;
        let ib = r.take(ni * 2)?;
        let indices: Vec<u16> = ib.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let nv = r.u32()? as usize;
        let vb = r.take(nv * 12)?;
        let vertices: Vec<TerrainVertex> = vb
            .chunks_exact(12)
            .map(|c| {
                let s = |o: usize| i16::from_le_bytes([c[o], c[o + 1]]);
                TerrainVertex { x: s(0), y: s(2), height_far: s(4), height_near: s(6), color: [c[8], c[9], c[10], c[11]] }
            })
            .collect();
        let nb = r.u32()? as usize;
        let mut q = 0;
        for _ in 0..nb {
            let b = r.take(16)?;
            let f = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap()) as usize;
            let (io, ic, vo, vc) = (f(0), f(4), f(8), f(12));
            if vc == 0 {
                continue;
            }
            ensure!(io + ic <= indices.len() && vo + vc <= vertices.len(), "lot de rendu hors limites");
            ensure!(q < quads.len(), "plus de lots que de textures");
            quads[q].indices = indices[io..io + ic].to_vec();
            quads[q].vertices = vertices[vo..vo + vc].to_vec();
            q += 1;
        }
        Ok(Self { lod, verts_per_side, quads })
    }
}

// ---------------------------------------------------------------- repère et maillage

/// Pas entre deux sommets (m) pour un niveau de détail : 2 m pour `.cnk1`, puis ×2 par niveau.
pub fn vertex_spacing(lod: u32) -> f32 {
    (1u32 << lod) as f32
}

/// Coin minimal (x, z) du quart `q` d'un chunk. Repère vérifié en comparant la hauteur du
/// terrain à celle des arbustes et rochers posés dessus (flcli `terrain`, écart médian ~1 m) :
/// dans le nom, `a` suit le z du monde et `b` le x ; le quart 0 est au coin minimal, le 1 vers
/// +z, le 2 vers +x, le 3 vers +x +z ; dans un quart, le x des sommets suit le x du monde et
/// leur y le z, sans inversion.
pub fn quad_origin(a: i32, b: i32, lod: u32, q: usize) -> (f32, f32) {
    let side = 64.0 * vertex_spacing(lod);
    let (ci, cj) = ((q >> 1) as f32, (q & 1) as f32);
    (b as f32 * 64.0 + ci * side, a as f32 * 64.0 + cj * side)
}

/// Maillage d'un quart, en coordonnées du monde.
pub struct QuadMesh {
    pub positions: Vec<glam::Vec3>,
    pub normals: Vec<glam::Vec3>,
    /// UV de la texture de couleur du quart
    pub uvs: Vec<glam::Vec2>,
    pub indices: Vec<u32>,
}

impl Chunk {
    /// Maillage monde du quart `q` du chunk nommé (a, b).
    pub fn quad_mesh(&self, a: i32, b: i32, q: usize) -> QuadMesh {
        let quad = &self.quads[q];
        let sp = vertex_spacing(self.lod);
        let (x0, z0) = quad_origin(a, b, self.lod, q);
        let n = (self.verts_per_side.max(2) - 1) as f32;
        let positions: Vec<glam::Vec3> = quad
            .vertices
            .iter()
            .map(|v| glam::Vec3::new(x0 + v.x as f32 * sp, v.height_near as f32 * HEIGHT_SCALE, z0 + v.y as f32 * sp))
            .collect();
        // texture : colonnes selon z, lignes selon x (comme les tuiles de la vue d'ensemble)
        let uvs = quad.vertices.iter().map(|v| glam::Vec2::new(v.y as f32 / n, v.x as f32 / n)).collect();
        let indices: Vec<u32> = quad.indices.iter().map(|&i| i as u32).filter(|&i| (i as usize) < positions.len()).collect();
        let indices: Vec<u32> = indices.chunks_exact(3).flatten().copied().collect();
        let mut normals = vec![glam::Vec3::ZERO; positions.len()];
        for t in indices.chunks_exact(3) {
            let (p0, p1, p2) = (positions[t[0] as usize], positions[t[1] as usize], positions[t[2] as usize]);
            let mut f = (p1 - p0).cross(p2 - p0);
            if f.y < 0.0 {
                f = -f;
            }
            for &i in t {
                normals[i as usize] += f;
            }
        }
        let normals = normals.into_iter().map(|n| n.try_normalize().unwrap_or(glam::Vec3::Y)).collect();
        QuadMesh { positions, normals, uvs, indices }
    }

    /// Hauteur du terrain au point (x, z) du monde, s'il est dans ce chunk (a, b).
    pub fn height_at(&self, a: i32, b: i32, x: f32, z: f32) -> Option<f32> {
        for q in 0..self.quads.len() {
            let (x0, z0) = quad_origin(a, b, self.lod, q);
            let side = 64.0 * vertex_spacing(self.lod);
            if x < x0 || z < z0 || x > x0 + side || z > z0 + side {
                continue;
            }
            let m = self.quad_mesh(a, b, q);
            for t in m.indices.chunks_exact(3) {
                let [p0, p1, p2] = [0, 1, 2].map(|k| m.positions[t[k] as usize]);
                let d = (p1.z - p2.z) * (p0.x - p2.x) + (p2.x - p1.x) * (p0.z - p2.z);
                if d.abs() < 1e-9 {
                    continue;
                }
                let w0 = ((p1.z - p2.z) * (x - p2.x) + (p2.x - p1.x) * (z - p2.z)) / d;
                let w1 = ((p2.z - p0.z) * (x - p2.x) + (p0.x - p2.x) * (z - p2.z)) / d;
                let w2 = 1.0 - w0 - w1;
                if w0 >= -1e-4 && w1 >= -1e-4 && w2 >= -1e-4 {
                    return Some(w0 * p0.y + w1 * p1.y + w2 * p2.y);
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quad_layout() {
        assert_eq!(chunk_tiles(1), 4);
        assert_eq!(chunk_tiles(3), 16);
        assert_eq!(chunk_name("Indar", -4, 8, 1), "Indar_-4_8.cnk1");
        // chunk (a = z, b = x) = (4, -8) : quart 0 au coin minimal, 1 vers +z, 2 vers +x
        assert_eq!(quad_origin(4, -8, 1, 0), (-512.0, 256.0));
        assert_eq!(quad_origin(4, -8, 1, 1), (-512.0, 384.0));
        assert_eq!(quad_origin(4, -8, 1, 2), (-384.0, 256.0));
    }
}
