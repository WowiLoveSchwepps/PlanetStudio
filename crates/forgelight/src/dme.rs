//! Modèles `.dme` (DMOD v4) avec bloc de matériaux DMAT.
//!
//! DMOD | version u32 | dmat_len u32 | DMAT[dmat_len]
//! bounds min/max (6 f32) | mesh_count u32 | meshes
//!   mesh : draw_offset, draw_count, bone_count, unk(-1), stream_count,
//!          index_size, index_count, vertex_count (u32)
//!          streams : [bytes_per_vertex u32, data] ; puis indices
//! draw_call_count u32 | draw calls (9 u32)
//! bone_map_count u32 | (global_bone u16, palette_index u16)
//! bone_count u32 | inv_bind 4x3 f32 * n | bbox 6 f32 * n | name_hash u32 * n

use anyhow::{bail, Result};
use glam::{Mat4, Vec3, Vec4};

pub struct Reader<'a> {
    pub data: &'a [u8],
    pub pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.pos + n > self.data.len() {
            bail!("fin de fichier inattendue (pos {} + {} > {})", self.pos, n, self.data.len());
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.bytes(2)?.try_into().unwrap()))
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }
    pub fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }
    pub fn vec3(&mut self) -> Result<Vec3> {
        Ok(Vec3::new(self.f32()?, self.f32()?, self.f32()?))
    }
}

#[derive(Clone, Debug)]
pub struct MaterialParam {
    pub name_hash: u32,
    pub class: u32,
    pub ty: u32,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct DmatMaterial {
    pub name_hash: u32,
    pub definition_hash: u32,
    pub params: Vec<MaterialParam>,
}

impl DmatMaterial {
    /// Hash (jenkins MAJUSCULES) des textures référencées par le matériau.
    pub fn texture_hashes(&self) -> Vec<(u32, u32)> {
        self.params
            .iter()
            .filter(|p| p.ty == 5 && p.data.len() == 4)
            .map(|p| (p.name_hash, u32::from_le_bytes(p.data[..4].try_into().unwrap())))
            .collect()
    }
}

#[derive(Clone, Debug)]
pub struct VertexStream {
    pub stride: usize,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct RawMesh {
    pub draw_offset: u32,
    pub draw_count: u32,
    pub bone_count: u32,
    pub vertex_count: usize,
    pub streams: Vec<VertexStream>,
    pub indices: Vec<u32>,
}

#[derive(Clone, Copy, Debug)]
pub struct DrawCall {
    pub bone_start: u32,
    pub bone_count: u32,
    pub vertex_offset: u32,
    pub vertex_count: u32,
    pub index_offset: u32,
    pub index_count: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct BoneMapEntry {
    pub global: u16,
    pub palette: u16,
}

#[derive(Clone, Debug)]
pub struct DmeBone {
    pub name_hash: u32,
    pub inverse_bind: Mat4,
    pub bbox_min: Vec3,
    pub bbox_max: Vec3,
}

#[derive(Clone, Debug)]
pub struct Dme {
    pub version: u32,
    pub texture_names: Vec<String>,
    pub materials: Vec<DmatMaterial>,
    pub bounds_min: Vec3,
    pub bounds_max: Vec3,
    pub meshes: Vec<RawMesh>,
    pub draw_calls: Vec<DrawCall>,
    pub bone_map: Vec<BoneMapEntry>,
    pub bones: Vec<DmeBone>,
}

fn read_dmat(r: &mut Reader) -> Result<(Vec<String>, Vec<DmatMaterial>)> {
    if r.bytes(4)? != b"DMAT" {
        bail!("bloc DMAT attendu");
    }
    let _ver = r.u32()?;
    let tl = r.u32()? as usize;
    let names = r
        .bytes(tl)?
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).to_string())
        .collect();
    let mc = r.u32()?;
    let mut mats = Vec::with_capacity(mc as usize);
    for _ in 0..mc {
        let name_hash = r.u32()?;
        let len = r.u32()? as usize;
        let start = r.pos;
        let definition_hash = r.u32()?;
        let pc = r.u32()?;
        let mut params = Vec::with_capacity(pc as usize);
        for _ in 0..pc {
            let name_hash = r.u32()?;
            let class = r.u32()?;
            let ty = r.u32()?;
            let dl = r.u32()? as usize;
            params.push(MaterialParam { name_hash, class, ty, data: r.bytes(dl)?.to_vec() });
        }
        r.pos = start + len;
        mats.push(DmatMaterial { name_hash, definition_hash, params });
    }
    Ok((names, mats))
}

/// Matrice 4x3 D3D (vecteurs ligne) -> Mat4 glam (vecteurs colonne).
fn mat4x3(m: &[f32; 12]) -> Mat4 {
    Mat4::from_cols(
        Vec4::new(m[0], m[1], m[2], 0.0),
        Vec4::new(m[3], m[4], m[5], 0.0),
        Vec4::new(m[6], m[7], m[8], 0.0),
        Vec4::new(m[9], m[10], m[11], 1.0),
    )
}

impl Dme {
    /// Remplace les textures des matériaux `only` par celles d'une palette `.dma` (bloc DMAT
    /// seul). La définition de chaque matériau est gardée : elle fixe la disposition des
    /// sommets, qui doit rester celle du `.dme`.
    pub fn apply_palette(&mut self, dma: &[u8], only: &[usize]) -> Result<()> {
        let (names, mats) = read_dmat(&mut Reader::new(dma))?;
        if mats.len() != self.materials.len() {
            bail!("palette de {} matériaux pour {} dans le modèle", mats.len(), self.materials.len());
        }
        for (i, (m, p)) in self.materials.iter_mut().zip(mats).enumerate() {
            if only.contains(&i) {
                m.params = p.params;
            }
        }
        for n in names {
            if !self.texture_names.iter().any(|t| t.eq_ignore_ascii_case(&n)) {
                self.texture_names.push(n);
            }
        }
        Ok(())
    }

    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        if r.bytes(4)? != b"DMOD" {
            bail!("pas un fichier DMOD");
        }
        let version = r.u32()?;
        if version != 4 {
            bail!("version DMOD {version} non supportée (seule la v4 l'est)");
        }
        let dmat_len = r.u32()? as usize;
        let dmat_end = r.pos + dmat_len;
        let (texture_names, materials) = read_dmat(&mut r)?;
        r.pos = dmat_end;

        let bounds_min = r.vec3()?;
        let bounds_max = r.vec3()?;
        let mesh_count = r.u32()?;
        let mut meshes = Vec::with_capacity(mesh_count as usize);
        for _ in 0..mesh_count {
            let draw_offset = r.u32()?;
            let draw_count = r.u32()?;
            let bone_count = r.u32()?;
            let _unk = r.u32()?;
            let stream_count = r.u32()?;
            // le bit de poids fort est un drapeau (0x80000004 sur les skins PS_Operative) : la
            // taille d'un index est dans les bits de poids faible
            let index_size = (r.u32()? & 0x7FFF_FFFF) as usize;
            let index_count = r.u32()? as usize;
            let vertex_count = r.u32()? as usize;
            let mut streams = Vec::new();
            for _ in 0..stream_count {
                let stride = r.u32()? as usize;
                let data = r.bytes(stride * vertex_count)?.to_vec();
                streams.push(VertexStream { stride, data });
            }
            let raw = r.bytes(index_size * index_count)?;
            let indices = match index_size {
                2 => raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]]) as u32).collect(),
                4 => raw.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect(),
                n => bail!("taille d'index {n} inconnue"),
            };
            meshes.push(RawMesh { draw_offset, draw_count, bone_count, vertex_count, streams, indices });
        }

        let mut draw_calls = Vec::new();
        let mut bone_map = Vec::new();
        let mut bones = Vec::new();
        if r.pos < data.len() {
            let dc = r.u32()?;
            for _ in 0..dc {
                let _u0 = r.u32()?;
                let bone_start = r.u32()?;
                let bone_count = r.u32()?;
                let _delta = r.u32()?;
                let _u1 = r.u32()?;
                let vertex_offset = r.u32()?;
                let vertex_count = r.u32()?;
                let index_offset = r.u32()?;
                let index_count = r.u32()?;
                draw_calls.push(DrawCall { bone_start, bone_count, vertex_offset, vertex_count, index_offset, index_count });
            }
            let bmc = r.u32()?;
            for _ in 0..bmc {
                let global = r.u16()?;
                let palette = r.u16()?;
                bone_map.push(BoneMapEntry { global, palette });
            }
            let bc = r.u32()? as usize;
            let mut mats = Vec::with_capacity(bc);
            for _ in 0..bc {
                let mut m = [0f32; 12];
                for v in m.iter_mut() {
                    *v = r.f32()?;
                }
                mats.push(mat4x3(&m));
            }
            let mut boxes = Vec::with_capacity(bc);
            for _ in 0..bc {
                boxes.push((r.vec3()?, r.vec3()?));
            }
            for i in 0..bc {
                let name_hash = r.u32()?;
                bones.push(DmeBone { name_hash, inverse_bind: mats[i], bbox_min: boxes[i].0, bbox_max: boxes[i].1 });
            }
        }

        Ok(Self { version, texture_names, materials, bounds_min, bounds_max, meshes, draw_calls, bone_map, bones })
    }
}
