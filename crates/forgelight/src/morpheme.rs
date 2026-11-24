//! Réseaux d'animation Morpheme (`.mrn`) : squelette (AnimRigDef), tables
//! squelette -> canaux, noms des clips et clips compressés au format NSA.
//!
//! Format rétro-analysé à partir des données du jeu :
//!
//! Bundle : suite d'entrées alignées sur 4 octets
//!   u32 24 | u32 6 | u32 type | u32 id | guid[16] | u32 taille | u32 alignement | données (alignées)
//! Types utilisés : 1 = squelette, 2 = table squelette->canaux, 13 = noms des clips, 15 = clip.
//!
//! Clip NSA : canaux "constants" (quantifiés 3x16 bits sur une plage globale) et
//! canaux "échantillonnés" (positions 11:11:10 bits, rotations 3x16 bits) exprimés
//! par rapport à une moyenne par canal (u8) et à des jeux de quantification partagés.
//! Les rotations sont des vecteurs "tangente du quart d'angle".

use anyhow::{bail, Context, Result};
use glam::{Quat, Vec3};
use std::collections::HashMap;

fn u16_at(d: &[u8], o: usize) -> Result<u16> {
    d.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]])).context("lecture hors limites")
}
fn u32_at(d: &[u8], o: usize) -> Result<u32> {
    d.get(o..o + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap())).context("lecture hors limites")
}
fn i32_at(d: &[u8], o: usize) -> Result<i32> {
    Ok(u32_at(d, o)? as i32)
}
fn f32_at(d: &[u8], o: usize) -> Result<f32> {
    Ok(f32::from_bits(u32_at(d, o)?))
}
fn vec3_at(d: &[u8], o: usize) -> Result<Vec3> {
    Ok(Vec3::new(f32_at(d, o)?, f32_at(d, o + 4)?, f32_at(d, o + 8)?))
}

/// Une entrée du bundle.
#[derive(Clone, Debug)]
pub struct Entry {
    pub ty: u32,
    pub id: u32,
    pub offset: usize,
    pub size: usize,
}

pub fn parse_bundle(data: &[u8]) -> Result<Vec<Entry>> {
    let mut out = Vec::new();
    let mut p = 0usize;
    while p + 0x28 <= data.len() {
        p = (p + 3) & !3;
        if p + 0x28 > data.len() || u32_at(data, p)? != 24 {
            break;
        }
        let ty = u32_at(data, p + 8)?;
        let id = u32_at(data, p + 12)?;
        let size = u32_at(data, p + 32)? as usize;
        let align = (u32_at(data, p + 36)? as usize).max(1);
        let start = (p + 0x28).div_ceil(align) * align;
        if start + size > data.len() {
            bail!("entrée Morpheme tronquée");
        }
        out.push(Entry { ty, id, offset: start, size });
        p = start + size;
    }
    if out.is_empty() {
        bail!("pas un réseau Morpheme");
    }
    Ok(out)
}

/// Table de chaînes Morpheme : count, dataLen, idsOff, offsetsOff, dataOff (relatifs).
fn string_table(d: &[u8], o: usize) -> Result<(Vec<u32>, Vec<String>)> {
    let cnt = u32_at(d, o)? as usize;
    let ids_o = o + u32_at(d, o + 8)? as usize;
    let offs_o = o + u32_at(d, o + 12)? as usize;
    let data_o = o + u32_at(d, o + 16)? as usize;
    let mut ids = Vec::with_capacity(cnt);
    let mut names = Vec::with_capacity(cnt);
    for i in 0..cnt {
        ids.push(u32_at(d, ids_o + 4 * i)?);
        let s = data_o + u32_at(d, offs_o + 4 * i)? as usize;
        let e = d[s..].iter().position(|&c| c == 0).map(|e| s + e).unwrap_or(d.len());
        names.push(String::from_utf8_lossy(&d[s..e]).to_string());
    }
    Ok((ids, names))
}

/// Squelette d'animation Morpheme.
#[derive(Clone, Debug)]
pub struct Rig {
    pub names: Vec<String>,
    pub parents: Vec<Option<usize>>,
    pub bind_pos: Vec<Vec3>,
    pub bind_rot: Vec<Quat>,
}

impl Rig {
    pub fn parse(d: &[u8]) -> Result<Self> {
        let n = u32_at(d, 0x40)? as usize;
        if n == 0 || n > 4096 {
            bail!("squelette Morpheme invalide");
        }
        let parents = (0..n)
            .map(|i| i32_at(d, 0x48 + 4 * i).map(|p| (p >= 0).then_some(p as usize)))
            .collect::<Result<Vec<_>>>()?;
        let nt = u32_at(d, 0x2c)? as usize;
        let (_, names) = string_table(d, nt)?;
        // pose de liaison : positions (vec4) puis quaternions, en fin de bloc
        let end = d.len();
        let qbase = end - 16 * n;
        let pbase = end - 32 * n;
        let bind_pos = (0..n).map(|i| vec3_at(d, pbase + 16 * i)).collect::<Result<Vec<_>>>()?;
        let bind_rot = (0..n)
            .map(|i| {
                Ok(Quat::from_xyzw(f32_at(d, qbase + 16 * i)?, f32_at(d, qbase + 16 * i + 4)?, f32_at(d, qbase + 16 * i + 8)?, f32_at(d, qbase + 16 * i + 12)?)
                    .normalize())
            })
            .collect::<Result<Vec<_>>>()?;
        if names.len() != n {
            bail!("squelette : {} noms pour {} os", names.len(), n);
        }
        Ok(Self { names, parents, bind_pos, bind_rot })
    }
}

/// Table squelette -> canaux d'animation : canal c = c-ième os utilisé.
#[derive(Clone, Debug)]
pub struct RigToAnimMap {
    pub num_rig_bones: usize,
    /// os du squelette pour chaque canal
    pub channel_to_bone: Vec<usize>,
}

impl RigToAnimMap {
    pub fn parse(d: &[u8]) -> Result<Self> {
        let n = u32_at(d, 20)? as usize;
        let words = u32_at(d, 24)? as usize;
        if words * 32 < n || words > 1024 {
            bail!("table squelette->canaux invalide");
        }
        let mut channel_to_bone = Vec::new();
        for b in 0..n {
            let w = u32_at(d, 28 + 4 * (b / 32))?;
            if (w >> (31 - (b % 32))) & 1 == 1 {
                channel_to_bone.push(b);
            }
        }
        Ok(Self { num_rig_bones: n, channel_to_bone })
    }
}

// ------------------------------------------------------------------------- NSA

#[derive(Clone, Copy, Debug)]
struct QSet {
    offset: Vec3,
    scale: Vec3,
}

fn read_qset(d: &[u8], o: usize) -> Result<QSet> {
    Ok(QSet { offset: vec3_at(d, o)?, scale: vec3_at(d, o + 12)? })
}

fn channel_map(d: &[u8], o: usize) -> Result<Vec<usize>> {
    let n = u16_at(d, o)? as usize;
    (0..n).map(|i| u16_at(d, o + 2 + 2 * i).map(|v| v as usize)).collect()
}

/// Rotation depuis un vecteur "tangente du quart d'angle".
fn from_tqa(v: Vec3) -> Quat {
    let m = v.length_squared();
    let s = 1.0 / (1.0 + m);
    Quat::from_xyzw(2.0 * v.x * s, 2.0 * v.y * s, 2.0 * v.z * s, (1.0 - m) * s).normalize()
}

/// Clip décodé : transformations locales par image et par canal.
#[derive(Clone, Debug)]
pub struct AnimClip {
    pub duration: f32,
    pub sample_rate: f32,
    pub num_channels: usize,
    /// frames[f][canal] = (rotation, translation)
    pub frames: Vec<Vec<(Quat, Vec3)>>,
    /// déplacement de la racine depuis le début du clip, par image
    pub trajectory: Option<Vec<(Quat, Vec3)>>,
}

/// Bloc trajectoire d'un clip NSA (relatif au clip) :
///   u32 type | u32 | u32 taille | u32 align | f32 fréquence | u32 nb images
///   quantif. position (min vec3, échelle vec3) | quantif. rotation (min vec3, échelle vec3)
///   u32 offset positions (0 = constante) | u32 offset rotations (0 = constante)
/// Positions : 11:11:10 bits par image. Rotations : 3 x u16 (tangente du quart d'angle).
/// Quand une piste est constante, les 6 flottants de quantification contiennent
/// directement la valeur (vec3, ou quaternion xyzw pour la rotation).
fn parse_trajectory(d: &[u8], t: usize) -> Result<Vec<(Quat, Vec3)>> {
    let n = u32_at(d, t + 0x14)? as usize;
    if n == 0 || n > 100_000 {
        bail!("trajectoire invalide");
    }
    let pq = read_qset(d, t + 0x18)?;
    let qq = read_qset(d, t + 0x30)?;
    let po = u32_at(d, t + 0x48)? as usize;
    let qo = u32_at(d, t + 0x4c)? as usize;
    let const_rot = Quat::from_xyzw(f32_at(d, t + 0x30)?, f32_at(d, t + 0x34)?, f32_at(d, t + 0x38)?, f32_at(d, t + 0x3c)?);
    let const_rot = if const_rot.length_squared() > 0.5 { const_rot.normalize() } else { Quat::IDENTITY };
    let mut out = Vec::with_capacity(n);
    for f in 0..n {
        let pos = if po == 0 {
            pq.offset
        } else {
            let w = u32_at(d, t + po + 4 * f)?;
            let s = Vec3::new((w >> 21) as f32, ((w >> 10) & 0x7ff) as f32, (w & 0x3ff) as f32);
            pq.offset + s * pq.scale
        };
        let rot = if qo == 0 {
            const_rot
        } else {
            let o = t + qo + 6 * f;
            let s = Vec3::new(u16_at(d, o)? as f32, u16_at(d, o + 2)? as f32, u16_at(d, o + 4)? as f32);
            from_tqa(qq.offset + s * qq.scale)
        };
        out.push((rot, pos));
    }
    Ok(out)
}

impl AnimClip {
    pub fn parse_nsa(d: &[u8]) -> Result<Self> {
        let anim_type = u32_at(d, 0x0c)?;
        if anim_type != 2 {
            bail!("format d'animation {anim_type} non pris en charge (seul NSA l'est)");
        }
        let duration = f32_at(d, 0x18)?;
        let sample_rate = f32_at(d, 0x1c)?;
        let nch = u32_at(d, 0x20)? as usize;
        let n_frame_sections = u32_at(d, 0x24)? as usize;
        let n_chan_sections = u32_at(d, 0x28)? as usize;
        if nch == 0 || nch > 4096 {
            bail!("nombre de canaux invalide");
        }
        let unch_pos_map = channel_map(d, u32_at(d, 0x30)? as usize)?;
        let unch_quat_map = channel_map(d, u32_at(d, 0x34)? as usize)?;
        let pos_mean_min = vec3_at(d, 0x40)?;
        let pos_mean_scale = vec3_at(d, 0x4c)?;
        let n_pos_sets = u32_at(d, 0x58)? as usize;
        let n_quat_sets = u32_at(d, 0x5c)? as usize;
        let pos_sets_o = u32_at(d, 0x60)? as usize;
        let quat_sets_o = u32_at(d, 0x64)? as usize;
        let pos_sets = (0..n_pos_sets).map(|i| read_qset(d, pos_sets_o + 24 * i)).collect::<Result<Vec<_>>>()?;
        let quat_sets = (0..n_quat_sets).map(|i| read_qset(d, quat_sets_o + 24 * i)).collect::<Result<Vec<_>>>()?;

        // canaux constants
        let mut base = vec![(Quat::IDENTITY, Vec3::ZERO); nch];
        let u = u32_at(d, 0x68)? as usize;
        if u != 0 {
            let np = u32_at(d, u)? as usize;
            let nq = u32_at(d, u + 4)? as usize;
            let pq = read_qset(d, u + 8)?;
            let qq = read_qset(d, u + 0x20)?;
            let pdo = u + u32_at(d, u + 0x38)? as usize;
            let qdo = u + u32_at(d, u + 0x3c)? as usize;
            for i in 0..np.min(unch_pos_map.len()) {
                let s = Vec3::new(u16_at(d, pdo + 6 * i)? as f32, u16_at(d, pdo + 6 * i + 2)? as f32, u16_at(d, pdo + 6 * i + 4)? as f32);
                if let Some(c) = base.get_mut(unch_pos_map[i]) {
                    c.1 = pq.offset + s * pq.scale;
                }
            }
            for i in 0..nq.min(unch_quat_map.len()) {
                let s = Vec3::new(u16_at(d, qdo + 6 * i)? as f32, u16_at(d, qdo + 6 * i + 2)? as f32, u16_at(d, qdo + 6 * i + 4)? as f32);
                if let Some(c) = base.get_mut(unch_quat_map[i]) {
                    c.0 = from_tqa(qq.offset + s * qq.scale);
                }
            }
        }

        let num_frames = ((duration * sample_rate).round() as usize + 1).max(1);
        if n_frame_sections == 0 || n_chan_sections == 0 {
            return Ok(Self { duration, sample_rate, num_channels: nch, frames: vec![base], trajectory: None });
        }
        let mut frames = vec![base; num_frames];

        let pos_maps_o = u32_at(d, 0x38)? as usize;
        let quat_maps_o = u32_at(d, 0x3c)? as usize;
        let starts_o = u32_at(d, 0x70)? as usize;
        let sections_o = u32_at(d, 0x78)? as usize;
        for fs in 0..n_frame_sections {
            let start_frame = u32_at(d, starts_o + 4 * fs)? as usize;
            for cs in 0..n_chan_sections {
                let pos_map = channel_map(d, u32_at(d, pos_maps_o + 4 * cs)? as usize)?;
                let quat_map = channel_map(d, u32_at(d, quat_maps_o + 4 * cs)? as usize)?;
                let s = u32_at(d, sections_o + 4 * (fs * n_chan_sections + cs))? as usize;
                let nf = u32_at(d, s)? as usize;
                let np = u32_at(d, s + 4)? as usize;
                let nq = u32_at(d, s + 8)? as usize;
                let pdo = s + u32_at(d, s + 0x0c)? as usize;
                let pqo = s + u32_at(d, s + 0x10)? as usize;
                let qdo = s + u32_at(d, s + 0x14)? as usize;
                let qqo = s + u32_at(d, s + 0x18)? as usize;
                // quantification par canal : moyenne (3 x u8) + jeu (3 x u8)
                let pos_q: Vec<(Vec3, [usize; 3])> = (0..np)
                    .map(|c| {
                        let o = pqo + 6 * c;
                        let m = d.get(o..o + 6).context("quantification tronquée")?;
                        let mean = pos_mean_min + Vec3::new(m[0] as f32, m[1] as f32, m[2] as f32) * pos_mean_scale;
                        Ok((mean, [m[3] as usize, m[4] as usize, m[5] as usize]))
                    })
                    .collect::<Result<_>>()?;
                let quat_q: Vec<(Vec3, [usize; 3])> = (0..nq)
                    .map(|c| {
                        let o = qqo + 6 * c;
                        let m = d.get(o..o + 6).context("quantification tronquée")?;
                        let mean = Vec3::new(m[0] as f32, m[1] as f32, m[2] as f32) * (2.0 / 255.0) - Vec3::ONE;
                        Ok((mean, [m[3] as usize, m[4] as usize, m[5] as usize]))
                    })
                    .collect::<Result<_>>()?;
                for f in 0..nf {
                    let gf = start_frame + f;
                    if gf >= frames.len() {
                        break;
                    }
                    for c in 0..np.min(pos_map.len()) {
                        let w = u32_at(d, pdo + 4 * (f * np + c))?;
                        let smp = Vec3::new((w >> 21) as f32, ((w >> 10) & 0x7ff) as f32, (w & 0x3ff) as f32);
                        let (mean, set) = pos_q[c];
                        let mut v = mean;
                        for k in 0..3 {
                            let qs = pos_sets.get(set[k]).context("jeu de quantification invalide")?;
                            v[k] += qs.offset[k] + smp[k] * qs.scale[k];
                        }
                        if let Some(ch) = frames[gf].get_mut(pos_map[c]) {
                            ch.1 = v;
                        }
                    }
                    for c in 0..nq.min(quat_map.len()) {
                        let o = qdo + 6 * (f * nq + c);
                        let smp = Vec3::new(u16_at(d, o)? as f32, u16_at(d, o + 2)? as f32, u16_at(d, o + 4)? as f32);
                        let (mean, set) = quat_q[c];
                        let mut v = mean;
                        for k in 0..3 {
                            let qs = quat_sets.get(set[k]).context("jeu de quantification invalide")?;
                            v[k] += qs.offset[k] + smp[k] * qs.scale[k];
                        }
                        if let Some(ch) = frames[gf].get_mut(quat_map[c]) {
                            ch.0 = from_tqa(v);
                        }
                    }
                }
            }
        }
        let traj_o = u32_at(d, 0x7c)? as usize;
        let trajectory = if traj_o != 0 { parse_trajectory(d, traj_o).ok() } else { None };
        Ok(Self { duration, sample_rate, num_channels: nch, frames, trajectory })
    }

    /// Échantillon interpolé à un temps (secondes) : (rotation, translation) par canal.
    pub fn sample(&self, time: f32) -> Vec<(Quat, Vec3)> {
        if self.frames.len() == 1 {
            return self.frames[0].clone();
        }
        let f = (time * self.sample_rate).clamp(0.0, (self.frames.len() - 1) as f32);
        let i = (f.floor() as usize).min(self.frames.len() - 2);
        let t = f - i as f32;
        self.frames[i]
            .iter()
            .zip(&self.frames[i + 1])
            .map(|(a, b)| (a.0.slerp(b.0, t).normalize(), a.1.lerp(b.1, t)))
            .collect()
    }
}

/// Description d'un clip disponible dans un réseau.
#[derive(Clone, Debug)]
pub struct ClipInfo {
    pub id: u32,
    pub name: String,
    /// dossier source (ex: "Male/Emote")
    pub category: String,
    pub num_channels: usize,
    pub duration: f32,
    entry: usize,
}

/// Réseau Morpheme chargé : squelettes, tables et clips (décodés à la demande).
pub struct Network {
    pub data: Vec<u8>,
    pub entries: Vec<Entry>,
    pub rigs: Vec<Rig>,
    pub maps: Vec<RigToAnimMap>,
    pub clips: Vec<ClipInfo>,
}

impl Network {
    pub fn parse(data: Vec<u8>) -> Result<Self> {
        let entries = parse_bundle(&data)?;
        let slice = |e: &Entry| &data[e.offset..e.offset + e.size];
        let mut rigs = Vec::new();
        let mut maps = Vec::new();
        let mut names: HashMap<u32, (String, String)> = HashMap::new();
        for e in &entries {
            match e.ty {
                1 => {
                    if let Ok(r) = Rig::parse(slice(e)) {
                        rigs.push(r)
                    }
                }
                2 => {
                    if let Ok(m) = RigToAnimMap::parse(slice(e)) {
                        maps.push(m)
                    }
                }
                13 => {
                    let d = slice(e);
                    // tables : [0] fichiers, [2] chemins sources, [3] noms de prises
                    if let (Ok(t0), Ok(t2)) = (u32_at(d, 0), u32_at(d, 8)) {
                        let files = string_table(d, t0 as usize).ok();
                        let sources = string_table(d, t2 as usize).ok();
                        if let Some((ids, files)) = files {
                            for (i, id) in ids.iter().enumerate() {
                                let src = sources.as_ref().and_then(|s| s.1.get(i)).cloned().unwrap_or_default();
                                let src = src.replace('\\', "/");
                                let base = src
                                    .rsplit('/')
                                    .next()
                                    .and_then(|s| s.strip_suffix(".xmd"))
                                    .map(String::from)
                                    .unwrap_or_else(|| files[i].trim_end_matches(".nsa").to_string());
                                let category = src
                                    .split("/Animations/")
                                    .last()
                                    .and_then(|p| p.rsplit_once('/'))
                                    .map(|(dir, _)| dir.to_string())
                                    .unwrap_or_default();
                                names.insert(*id, (base, category));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        let mut clips = Vec::new();
        for (i, e) in entries.iter().enumerate() {
            if e.ty != 15 {
                continue;
            }
            let d = slice(e);
            let (Ok(2), Ok(nch), Ok(dur)) = (u32_at(d, 0x0c), u32_at(d, 0x20), f32_at(d, 0x18)) else { continue };
            let (name, category) = names.get(&e.id).cloned().unwrap_or_else(|| (format!("clip_{}", e.id), String::new()));
            clips.push(ClipInfo { id: e.id, name, category, num_channels: nch as usize, duration: dur, entry: i });
        }
        Ok(Self { data, entries, rigs, maps, clips })
    }

    /// Squelette principal (le plus grand).
    pub fn main_rig(&self) -> Option<&Rig> {
        self.rigs.iter().max_by_key(|r| r.names.len())
    }

    /// Clips utilisables avec le squelette principal.
    pub fn main_clips(&self) -> Vec<&ClipInfo> {
        let Some(rig) = self.main_rig() else { return Vec::new() };
        self.clips.iter().filter(|c| self.map_for(rig.names.len(), c.num_channels).is_some()).collect()
    }

    pub fn map_for(&self, rig_bones: usize, channels: usize) -> Option<&RigToAnimMap> {
        self.maps.iter().find(|m| m.num_rig_bones == rig_bones && m.channel_to_bone.len() == channels)
    }

    pub fn decode(&self, clip: &ClipInfo) -> Result<AnimClip> {
        let e = &self.entries[clip.entry];
        AnimClip::parse_nsa(&self.data[e.offset..e.offset + e.size]).with_context(|| format!("clip {}", clip.name))
    }

    /// Décode un clip et l'exprime par os du squelette principal :
    /// pour chaque image, (nom de l'os, rotation locale, translation locale).
    pub fn bone_frames(&self, clip: &ClipInfo) -> Result<BoneClip> {
        let rig = self.main_rig().context("aucun squelette dans le réseau")?;
        let map = self.map_for(rig.names.len(), clip.num_channels).context("pas de table pour ce clip")?;
        let anim = self.decode(clip)?;
        let mut frames = Vec::with_capacity(anim.frames.len());
        for f in &anim.frames {
            let mut bones: Vec<(Quat, Vec3)> = rig.bind_rot.iter().copied().zip(rig.bind_pos.iter().copied()).collect();
            for (c, &b) in map.channel_to_bone.iter().enumerate() {
                if let (Some(v), Some(slot)) = (f.get(c), bones.get_mut(b)) {
                    *slot = *v;
                }
            }
            frames.push(bones);
        }
        Ok(BoneClip { sample_rate: anim.sample_rate, bone_names: rig.names.clone(), frames, trajectory: anim.trajectory })
    }
}

/// Clip exprimé par os du squelette principal.
pub struct BoneClip {
    pub sample_rate: f32,
    pub bone_names: Vec<String>,
    /// frames[f][os] = (rotation locale, translation locale)
    pub frames: Vec<Vec<(Quat, Vec3)>>,
    /// déplacement de la racine depuis le début du clip
    pub trajectory: Option<Vec<(Quat, Vec3)>>,
}
