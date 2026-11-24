//! Application des clips d'animation du jeu (Morpheme) aux acteurs.

use crate::anim::{Interp, Key};
use crate::scene::Actor;
use forgelight::hash::jenkins_upper;
use forgelight::morpheme::{ClipInfo, Network};
use forgelight::{Skeleton, Transform};
use glam::{Quat, Vec3};
use std::collections::HashMap;

/// Clip décodé, exprimé par os du squelette d'animation.
pub struct DecodedClip {
    pub name: String,
    pub sample_rate: f32,
    /// hash Jenkins (MAJUSCULES) de chaque os du squelette d'animation
    pub bone_hashes: Vec<u32>,
    pub bind: Vec<(Quat, Vec3)>,
    pub frames: Vec<Vec<(Quat, Vec3)>>,
    /// déplacement de la racine depuis le début du clip, par image
    pub trajectory: Option<Vec<(Quat, Vec3)>>,
}

impl DecodedClip {
    pub fn decode(net: &Network, clip: &ClipInfo) -> anyhow::Result<Self> {
        let rig = net.main_rig().ok_or_else(|| anyhow::anyhow!(tr!("aucun squelette", "no skeleton")))?;
        let bc = net.bone_frames(clip)?;
        // une trajectoire immobile n'apporte rien
        let trajectory = bc.trajectory.filter(|t| {
            t.iter().any(|(q, p)| p.length() > 1e-3 || q.angle_between(Quat::IDENTITY) > 1e-3)
        });
        Ok(Self {
            name: clip.name.clone(),
            sample_rate: bc.sample_rate,
            bone_hashes: bc.bone_names.iter().map(|n| jenkins_upper(n)).collect(),
            bind: rig.bind_rot.iter().copied().zip(rig.bind_pos.iter().copied()).collect(),
            frames: bc.frames,
            trajectory,
        })
    }

    pub fn duration(&self) -> f32 {
        (self.frames.len().max(1) - 1) as f32 / self.sample_rate.max(1.0)
    }

    pub fn has_root_motion(&self) -> bool {
        self.trajectory.is_some()
    }

    /// Déplacement de la racine depuis le début du clip, à un temps donné.
    pub fn root_delta_at(&self, time: f32) -> Transform {
        let Some(tr) = &self.trajectory else { return Transform::IDENTITY };
        if tr.is_empty() {
            return Transform::IDENTITY;
        }
        let n = tr.len();
        let f = (time * self.sample_rate).clamp(0.0, (n - 1) as f32);
        let i = (f.floor() as usize).min(n.saturating_sub(2));
        let t = if n > 1 { f - i as f32 } else { 0.0 };
        let (qa, pa) = tr[i];
        let (qb, pb) = tr[(i + 1).min(n - 1)];
        let qb = if qa.dot(qb) < 0.0 { -qb } else { qb };
        Transform { rotation: qa.slerp(qb, t).normalize(), translation: pa.lerp(pb, t), scale: Vec3::ONE }
    }

    /// Racine de l'acteur à un temps donné, à partir de sa racine au début du clip.
    pub fn root_at(&self, start_root: &Transform, time: f32) -> Transform {
        Transform::from_mat4(start_root.to_mat4() * self.root_delta_at(time).to_mat4())
    }

    /// Pour chaque os du modèle : indice dans le squelette d'animation.
    pub fn bone_mapping(&self, sk: &Skeleton) -> Vec<Option<usize>> {
        let idx: HashMap<u32, usize> = self.bone_hashes.iter().enumerate().map(|(i, h)| (*h, i)).collect();
        sk.bones.iter().map(|b| idx.get(&b.hash).copied()).collect()
    }

    /// Valeur brute du clip (rotation, translation) par os du modèle, à un temps donné.
    fn raw_at(&self, mapping: &[Option<usize>], time: f32) -> Vec<Option<(Quat, Vec3, usize)>> {
        let n = self.frames.len();
        let f = (time * self.sample_rate).clamp(0.0, (n.max(1) - 1) as f32);
        let i = (f.floor() as usize).min(n.saturating_sub(2));
        let t = if n > 1 { f - i as f32 } else { 0.0 };
        let (fa, fb) = (&self.frames[i], &self.frames[(i + 1).min(n - 1)]);
        mapping
            .iter()
            .map(|m| {
                let a = (*m)?;
                let (qa, pa) = fa[a];
                let (qb, pb) = fb[a];
                let qb = if qa.dot(qb) < 0.0 { -qb } else { qb };
                Some((qa.slerp(qb, t).normalize(), pa.lerp(pb, t), a))
            })
            .collect()
    }

    /// Combine le clip avec une pose existante (une transformation locale par os).
    /// `w` : poids effectif (poids x fondus). Renvoie `None` pour les os non concernés.
    pub fn blend_onto(
        &self,
        sk: &Skeleton,
        mapping: &[Option<usize>],
        time: f32,
        existing: &[Transform],
        opts: &BlendOptions,
        w: f32,
    ) -> Vec<Option<Transform>> {
        let raw = self.raw_at(mapping, time);
        sk.bones
            .iter()
            .enumerate()
            .map(|(b, bone)| {
                if opts.mask.as_ref().is_some_and(|m| !m.get(b).copied().unwrap_or(true)) {
                    return None;
                }
                let (q, p, a) = raw[b]?;
                let base = existing.get(b).copied().unwrap_or(bone.rest_local);
                Some(match opts.mode {
                    BlendMode::Replace => {
                        // translation relative à la liaison : s'adapte aux autres morphologies
                        let target = Transform {
                            rotation: q,
                            translation: bone.rest_local.translation + (p - self.bind[a].1),
                            scale: base.scale,
                        };
                        let mut base = base;
                        if base.rotation.dot(target.rotation) < 0.0 {
                            base.rotation = -base.rotation;
                        }
                        base.lerp(&target, w)
                    }
                    BlendMode::Additive => {
                        // écart exprimé dans le repère local de l'os
                        let d = Quat::IDENTITY.slerp(q, w).normalize();
                        Transform {
                            // vérifié visuellement : base x écart (l'inverse tord les bras)
                            rotation: (base.rotation * d).normalize(),
                            translation: base.translation + p * w,
                            scale: base.scale,
                        }
                    }
                })
            })
            .collect()
    }

    /// Écrit le clip en clés dans les pistes d'un acteur, à partir de `start`.
    /// Le clip est combiné avec l'animation déjà présente (remplacement pondéré
    /// avec fondus, ou superposition additive). Avec `root_start`, le déplacement
    /// de la racine est aussi écrit. Renvoie la dernière image et la racine finale.
    pub fn bake(&self, actor: &mut Actor, start: i32, bake: &BakeOptions) -> (i32, Option<Transform>) {
        let sk = actor.model.skeleton.clone();
        let mapping = self.bone_mapping(&sk);
        let frames = ((self.duration() / bake.speed.max(0.01)) * bake.fps).round().max(0.0) as i32;
        // animation existante, échantillonnée avant toute écriture
        let existing: Vec<Vec<Transform>> = (0..=frames)
            .map(|k| {
                sk.bones
                    .iter()
                    .enumerate()
                    .map(|(b, bone)| actor.anim.bones[b].sample((start + k) as f32).unwrap_or(bone.rest_local))
                    .collect()
            })
            .collect();
        let root_start = bake.root_start.filter(|_| self.has_root_motion() && bake.blend.mode == BlendMode::Replace);
        let mut root_end = root_start;
        for k in 0..=frames {
            let time = k as f32 / bake.fps * bake.speed;
            let w = bake.blend.weight * fade(k, frames, bake.fade_in, bake.fade_out);
            let pose = self.blend_onto(&sk, &mapping, time, &existing[k as usize], &bake.blend, w);
            for (b, tr) in pose.into_iter().enumerate() {
                if let Some(tr) = tr {
                    actor.anim.bones[b].set_key(Key::new(start + k, &tr, Interp::Linear));
                }
            }
            if let Some(r0) = &root_start {
                let r = self.root_at(r0, time);
                actor.anim.root.set_key(Key::new(start + k, &r, Interp::Linear));
                root_end = Some(r);
            }
        }
        (start + frames, root_end)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlendMode {
    /// le clip remplace l'animation (pondéré par le poids)
    Replace,
    /// le clip s'ajoute par-dessus l'animation existante
    Additive,
}

/// Partie du corps concernée.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyPart {
    All,
    Upper,
    Lower,
}

impl BodyPart {
    pub const ALL: [BodyPart; 3] = [BodyPart::All, BodyPart::Upper, BodyPart::Lower];
    pub fn label(self) -> &'static str {
        match self {
            BodyPart::All => tr!("Tout le corps", "Full body"),
            BodyPart::Upper => tr!("Haut du corps", "Upper body"),
            BodyPart::Lower => tr!("Bas du corps", "Lower body"),
        }
    }
    /// Masque par os : le haut du corps = SPINELOWER et ses descendants.
    pub fn mask(self, sk: &Skeleton) -> Option<Vec<bool>> {
        if self == BodyPart::All {
            return None;
        }
        let spine = sk.bones.iter().position(|b| b.name.eq_ignore_ascii_case("SPINELOWER"))?;
        let upper: Vec<bool> = (0..sk.bones.len())
            .map(|mut b| loop {
                if b == spine {
                    break true;
                }
                match sk.bones[b].parent {
                    Some(p) => b = p,
                    None => break false,
                }
            })
            .collect();
        Some(upper.into_iter().map(|u| if self == BodyPart::Upper { u } else { !u }).collect())
    }
}

#[derive(Clone, Debug)]
pub struct BlendOptions {
    pub mode: BlendMode,
    pub weight: f32,
    pub mask: Option<Vec<bool>>,
}

#[derive(Clone, Debug)]
pub struct BakeOptions {
    pub fps: f32,
    pub speed: f32,
    pub root_start: Option<Transform>,
    pub blend: BlendOptions,
    /// fondu d'entrée / de sortie, en images
    pub fade_in: i32,
    pub fade_out: i32,
}

/// Facteur de fondu (lissé) à l'image k d'un clip de `len` images.
fn fade(k: i32, len: i32, fade_in: i32, fade_out: i32) -> f32 {
    let smooth = |x: f32| {
        let x = x.clamp(0.0, 1.0);
        x * x * (3.0 - 2.0 * x)
    };
    let mut f = 1.0;
    if fade_in > 0 {
        f *= smooth(k as f32 / fade_in as f32);
    }
    if fade_out > 0 {
        f *= smooth((len - k) as f32 / fade_out as f32);
    }
    f
}
