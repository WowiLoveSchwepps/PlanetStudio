//! Squelette : noms d'os retrouvés via leur hash Jenkins, hiérarchie reconstruite.
//!
//! Les .dme ne stockent ni les noms (seulement un hash) ni les parents des os.
//! Les noms sont retrouvés grâce à un dictionnaire ; la hiérarchie grâce à des
//! règles sur les noms du squelette humain de PlanetSide 2, avec, pour les os
//! inconnus, un repli spatial (os précédent le plus proche en pose de liaison).

use crate::dme::Dme;
use crate::hash::jenkins_upper;
use glam::{Mat4, Quat, Vec3};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Transform {
    pub const IDENTITY: Self = Self { translation: Vec3::ZERO, rotation: Quat::IDENTITY, scale: Vec3::ONE };

    pub fn from_mat4(m: Mat4) -> Self {
        let (scale, rotation, translation) = m.to_scale_rotation_translation();
        Self { translation, rotation: rotation.normalize(), scale }
    }
    pub fn to_mat4(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }
    pub fn lerp(&self, o: &Self, t: f32) -> Self {
        Self {
            translation: self.translation.lerp(o.translation, t),
            rotation: self.rotation.slerp(o.rotation, t).normalize(),
            scale: self.scale.lerp(o.scale, t),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Bone {
    pub name: String,
    pub hash: u32,
    pub parent: Option<usize>,
    pub inverse_bind: Mat4,
    pub bind_world: Mat4,
    /// transformation locale de repos (relative au parent)
    pub rest_local: Transform,
}

#[derive(Clone, Debug, Default)]
pub struct Skeleton {
    pub bones: Vec<Bone>,
}

const SIDES: [&str; 2] = ["L_", "R_"];
const FINGERS: [&str; 5] = ["THUMB", "INDEX", "MIDDLE", "RING", "PINKY"];

/// Dictionnaire de noms d'os candidats.
pub fn bone_name_dictionary() -> HashMap<u32, String> {
    let mut names: Vec<String> = [
        "WORLDROOT", "ROOT", "COG", "PELVIS", "SPINELOWER", "SPINEMIDDLE", "SPINEUPPER", "NECK",
        "HEAD", "HEADEND", "HEAD_END", "JAW", "CAMERA", "CAMERAROOT", "WEAPON", "WEAPONROOT",
        "BACKPACK", "PACK", "HELMET", "ATTACH", "ATTACHMENT", "AIM", "LOOK", "EYES", "MOUTH",
        "TONGUE", "CHIN", "NOSE", "FOREHEAD", "HAIR", "PONYTAIL", "SPINE", "CHEST", "HIPS",
        "ROOT_MOTION", "MOVER", "TRAJECTORY", "COLLISION", "HEADPROP", "BELT",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let side_parts = [
        "HIP", "KNEE", "ANKLE", "BALL", "TOE", "CLAVICLE", "SHOULDER", "SHOULDERROLL", "ELBOW",
        "ELBOWROLL", "FOREARM", "FOREARMROLL", "WRIST", "WRISTROLL", "WEAPON", "HAND", "HIPROLL",
        "KNEEROLL", "THIGH", "THIGHROLL", "CALF", "FOOT", "EYE", "EYELID", "EYELID_UPPER",
        "EYELID_LOWER", "BROW", "BROW_INNER", "BROW_OUTER", "CHEEK", "LIP", "LIP_UPPER",
        "LIP_LOWER", "LIPCORNER", "MOUTHCORNER", "EAR", "PAD", "SHOULDERPAD", "KNEEPAD",
        "HOLSTER", "POUCH", "BICEP", "TRICEP", "SHIN", "UPPERARM", "ARM", "LEG", "PALM",
        "SHIELD", "CANNON", "GUN", "THRUSTER", "JET", "WING", "HOSE", "STRAP",
    ];
    for s in SIDES {
        for p in side_parts {
            names.push(format!("{s}{p}"));
            for i in 0..6 {
                names.push(format!("{s}{p}{i}"));
                names.push(format!("{s}{p}_{i}"));
            }
            for suf in ["A", "B", "C", "D"] {
                names.push(format!("{s}{p}{suf}"));
            }
        }
        for f in FINGERS {
            for suf in ["A", "B", "C", "D", "END", "_END", "1", "2", "3", "4"] {
                names.push(format!("{s}{f}{suf}"));
            }
        }
    }
    names.into_iter().map(|n| (jenkins_upper(&n), n)).collect()
}

fn side_of(name: &str) -> Option<(&str, &str)> {
    for s in SIDES {
        if let Some(rest) = name.strip_prefix(s) {
            return Some((s, rest));
        }
    }
    None
}

/// Parents candidats (par ordre de préférence) selon le nom.
fn parent_candidates(name: &str) -> Vec<String> {
    let c = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    match name {
        "WORLDROOT" => return vec![],
        "COG" => return c(&["WORLDROOT"]),
        "PELVIS" => return c(&["COG"]),
        "SPINELOWER" => return c(&["COG", "PELVIS"]),
        "SPINEMIDDLE" => return c(&["SPINELOWER"]),
        "SPINEUPPER" => return c(&["SPINEMIDDLE", "SPINELOWER"]),
        "NECK" => return c(&["SPINEUPPER"]),
        "HEAD" => return c(&["NECK"]),
        "HEADEND" | "HEAD_END" | "JAW" => return c(&["HEAD"]),
        _ => {}
    }
    if let Some((s, rest)) = side_of(name) {
        let sd = |p: &str| format!("{s}{p}");
        let v: Vec<String> = match rest {
            "HIP" => vec!["PELVIS".into()],
            "KNEE" => vec![sd("HIP")],
            "ANKLE" => vec![sd("KNEE")],
            "BALL" => vec![sd("ANKLE")],
            "TOE" => vec![sd("BALL")],
            "CLAVICLE" => vec!["SPINEUPPER".into()],
            "SHOULDER" => vec![sd("CLAVICLE")],
            "SHOULDERROLL" => vec![sd("SHOULDER")],
            "ELBOW" => vec![sd("SHOULDER")],
            "ELBOWROLL" | "FOREARM" => vec![sd("ELBOW")],
            "WRIST" => vec![sd("ELBOW"), sd("FOREARM")],
            "WEAPON" => vec![sd("WRIST")],
            "EYE" | "EYELID" | "BROW" | "CHEEK" | "LIP" | "EAR" => vec!["HEAD".into()],
            _ => {
                for f in FINGERS {
                    if let Some(suf) = rest.strip_prefix(f) {
                        return match suf {
                            "A" => vec![sd("WRIST")],
                            "B" => vec![sd(&format!("{f}A"))],
                            "C" => vec![sd(&format!("{f}B"))],
                            "D" | "END" | "_END" => vec![sd(&format!("{f}C")), sd(&format!("{f}B"))],
                            _ => vec![sd("WRIST")],
                        };
                    }
                }
                vec![]
            }
        };
        return v;
    }
    vec![]
}

impl Skeleton {
    pub fn from_dme(dme: &Dme) -> Self {
        let dict = bone_name_dictionary();
        let n = dme.bones.len();
        let names: Vec<String> = dme
            .bones
            .iter()
            .map(|b| dict.get(&b.name_hash).cloned().unwrap_or_else(|| format!("Os_{:08X}", b.name_hash)))
            .collect();
        let by_name: HashMap<&str, usize> = names.iter().enumerate().map(|(i, n)| (n.as_str(), i)).collect();
        let bind_world: Vec<Mat4> = dme.bones.iter().map(|b| b.inverse_bind.inverse()).collect();

        let mut parents = vec![None; n];
        for i in 1..n {
            let cands = parent_candidates(&names[i]);
            let mut p = cands.iter().find_map(|c| by_name.get(c.as_str()).copied()).filter(|&p| p != i);
            if p.is_none() {
                // repli spatial : os précédent le plus proche (hors WORLDROOT si possible)
                let pos = bind_world[i].w_axis.truncate();
                p = (0..i)
                    .filter(|&j| j != 0 || i == 1)
                    .min_by(|&a, &b| {
                        let da = bind_world[a].w_axis.truncate().distance_squared(pos);
                        let db = bind_world[b].w_axis.truncate().distance_squared(pos);
                        da.total_cmp(&db)
                    })
                    .or(Some(0));
            }
            parents[i] = p;
        }
        // garantir l'absence de cycles : un parent doit être déjà "ancré"
        for i in 0..n {
            let mut seen = 0;
            let mut cur = parents[i];
            while let Some(c) = cur {
                seen += 1;
                if c == i || seen > n {
                    parents[i] = if i == 0 { None } else { Some(0) };
                    break;
                }
                cur = parents[c];
            }
        }

        let bones = (0..n)
            .map(|i| {
                let local = match parents[i] {
                    Some(p) => bind_world[p].inverse() * bind_world[i],
                    None => bind_world[i],
                };
                Bone {
                    name: names[i].clone(),
                    hash: dme.bones[i].name_hash,
                    parent: parents[i],
                    inverse_bind: dme.bones[i].inverse_bind,
                    bind_world: bind_world[i],
                    rest_local: Transform::from_mat4(local),
                }
            })
            .collect();
        Self { bones }
    }

    pub fn rest_pose(&self) -> Vec<Transform> {
        self.bones.iter().map(|b| b.rest_local).collect()
    }

    /// Ordre de parcours garantissant parent avant enfant.
    pub fn eval_order(&self) -> Vec<usize> {
        let n = self.bones.len();
        let mut done = vec![false; n];
        let mut order = Vec::with_capacity(n);
        while order.len() < n {
            let before = order.len();
            for i in 0..n {
                if !done[i] && self.bones[i].parent.is_none_or(|p| done[p]) {
                    done[i] = true;
                    order.push(i);
                }
            }
            if order.len() == before {
                break;
            }
        }
        order
    }

    /// Matrices monde de chaque os pour une pose locale donnée.
    pub fn world_matrices(&self, pose: &[Transform], root: Mat4) -> Vec<Mat4> {
        let mut world = vec![Mat4::IDENTITY; self.bones.len()];
        for i in self.eval_order() {
            let local = pose.get(i).copied().unwrap_or(self.bones[i].rest_local).to_mat4();
            world[i] = match self.bones[i].parent {
                Some(p) => world[p] * local,
                None => root * local,
            };
        }
        world
    }

    /// Matrices de skinning (monde * inverse_bind).
    pub fn skin_matrices(&self, world: &[Mat4]) -> Vec<Mat4> {
        world.iter().zip(&self.bones).map(|(w, b)| *w * b.inverse_bind).collect()
    }

    pub fn children(&self, i: usize) -> impl Iterator<Item = usize> + '_ {
        self.bones.iter().enumerate().filter(move |(_, b)| b.parent == Some(i)).map(|(j, _)| j)
    }
}
