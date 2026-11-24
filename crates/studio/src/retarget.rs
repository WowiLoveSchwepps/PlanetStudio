//! Transfert d'une animation FBX (squelette Mixamo) sur le squelette des soldats de PlanetSide 2.
//!
//! Les deux squelettes n'ont ni les mêmes axes locaux ni la même pose de repos : Mixamo est en
//! T-pose, les soldats en A-pose. On ne copie donc pas les rotations locales. Pour chaque os
//! associé :
//! 1. on calcule une « pose d'alignement » du soldat, où chaque os pointe dans la même direction
//!    que son équivalent Mixamo au repos (et, pour les mains et les doigts, avec la paume tournée
//!    pareil) ;
//! 2. à chaque image, la rotation monde de l'os Mixamo par rapport à son repos est appliquée à
//!    l'os du soldat dans cette pose d'alignement ;
//! 3. on repasse en rotations locales en descendant la hiérarchie du soldat. Les longueurs d'os
//!    restent celles du soldat.
//!
//! Le bassin (Hips → COG) garde en plus son déplacement, mis à l'échelle de la longueur des
//! jambes. Le résultat est un `DecodedClip` ordinaire : mélange, masques et cuisson en clés
//! fonctionnent comme pour les clips du jeu.

use crate::clips::DecodedClip;
use anyhow::{bail, Result};
use forgelight::fbx::FbxAnimation;
use forgelight::Skeleton;
use glam::{Mat3, Mat4, Quat, Vec3};

/// Os Mixamo (sans préfixe) → os du soldat. Les os « _END » ne servent qu'à donner une direction.
const BONE_MAP: &[(&str, &str)] = &[
    ("Hips", "COG"),
    ("Spine", "SPINELOWER"),
    ("Spine1", "SPINEMIDDLE"),
    ("Spine2", "SPINEUPPER"),
    ("Neck", "NECK"),
    ("Head", "HEAD"),
    ("HeadTop_End", "HEAD_END"),
    ("LeftShoulder", "L_CLAVICLE"),
    ("LeftArm", "L_SHOULDER"),
    ("LeftForeArm", "L_ELBOW"),
    ("LeftHand", "L_WRIST"),
    ("RightShoulder", "R_CLAVICLE"),
    ("RightArm", "R_SHOULDER"),
    ("RightForeArm", "R_ELBOW"),
    ("RightHand", "R_WRIST"),
    ("LeftUpLeg", "L_HIP"),
    ("LeftLeg", "L_KNEE"),
    ("LeftFoot", "L_ANKLE"),
    ("LeftToeBase", "L_BALL"),
    ("LeftToe_End", "L_TOE"),
    ("RightUpLeg", "R_HIP"),
    ("RightLeg", "R_KNEE"),
    ("RightFoot", "R_ANKLE"),
    ("RightToeBase", "R_BALL"),
    ("RightToe_End", "R_TOE"),
];

const FINGERS: [(&str, &str); 5] = [("Thumb", "THUMB"), ("Index", "INDEX"), ("Middle", "MIDDLE"), ("Ring", "RING"), ("Pinky", "PINKY")];

/// Os terminaux : ils donnent la direction de leur parent mais ne sont pas animés.
fn is_end(target: &str) -> bool {
    target.ends_with("_END") || target.ends_with("_TOE")
}

/// Nom Mixamo sans espace de noms ni préfixe (« mixamorig:LeftArm », « mixamorig_LeftArm » → « LeftArm »).
fn short_name(n: &str) -> &str {
    let n = n.rsplit([':', '|']).next().unwrap_or(n);
    let lower = n.to_ascii_lowercase();
    if lower.starts_with("mixamorig") {
        let rest = &n["mixamorig".len()..];
        // « mixamorig1_Hips », « mixamorig_Hips »
        return rest.trim_start_matches(|c: char| c.is_ascii_digit()).trim_start_matches(['_', '.']);
    }
    n
}

/// Cherche dans le FBX l'os Mixamo `canon` (« LeftForeArm », « RightHandIndex2 »…), sous son nom
/// Mixamo ou sous son nom dans la convention Unreal (pelvis, spine_01, thigh_l, upperarm_r,
/// index_01_l…), courante dans les modèles Sketchfab et les exports Blender.
fn src_find(anim: &FbxAnimation, canon: &str) -> Option<usize> {
    let by = |n: &str| anim.names.iter().position(|x| short_name(x).eq_ignore_ascii_case(n));
    if let Some(i) = by(canon) {
        return Some(i);
    }
    let (side, rest) = match (canon.strip_prefix("Left"), canon.strip_prefix("Right")) {
        (Some(r), _) => ("l", r),
        (_, Some(r)) => ("r", r),
        _ => ("", canon),
    };
    // doigts : « HandIndex1 » → index_01_l. Les os « _end » des exports Blender ne suivent pas
    // l'axe du doigt : la phalange distale n'a pas de bout. Le pouce commence à thumb_00 s'il existe.
    if let Some(fk) = rest.strip_prefix("Hand").filter(|s| !s.is_empty()) {
        let k = fk.chars().last()?.to_digit(10)? as usize;
        let finger = fk[..fk.len() - 1].to_ascii_lowercase();
        let first = usize::from(!(finger == "thumb" && by(&format!("thumb_00_{side}")).is_some()));
        let n = first + k - 1;
        return if n <= 3 { by(&format!("{finger}_{n:02}_{side}")) } else { None };
    }
    let base = match rest {
        "Hips" => return by("pelvis"),
        "Spine" => return by("spine_01"),
        "Spine1" => return by("spine_02"),
        "Spine2" => return by("spine_03"),
        "Neck" => return by("neck_01").or_else(|| by("neck")),
        "Head" => return by("head"),
        "Shoulder" => "clavicle",
        "Arm" => "upperarm",
        "ForeArm" => "lowerarm",
        "Hand" => "hand",
        "UpLeg" => "thigh",
        "Leg" => "calf",
        "Foot" => "foot",
        "ToeBase" => "ball",
        _ => return None,
    };
    if let Some(i) = by(&format!("{base}_{side}")) {
        return Some(i);
    }
    // coude ou genou nommé autrement (« upperarm_twist_01_r », « RightUpLegTwist ») : c'est le
    // parent de la main ou du pied, s'il descend du bras ou de la cuisse
    let (top, bottom) = match rest {
        "ForeArm" => ("Arm", "Hand"),
        "Leg" => ("UpLeg", "Foot"),
        _ => return None,
    };
    let side_word = if side == "l" { "Left" } else { "Right" };
    let top = src_find(anim, &format!("{side_word}{top}"))?;
    let mid = anim.parents[src_find(anim, &format!("{side_word}{bottom}"))?]?;
    let mut cur = anim.parents[mid];
    while let Some(c) = cur {
        if c == top {
            return Some(mid);
        }
        cur = anim.parents[c];
    }
    None
}

fn full_map() -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = BONE_MAP.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    for (side_src, side_dst) in [("Left", "L_"), ("Right", "R_")] {
        for (fs, fd) in FINGERS {
            for (k, suf) in ["A", "B", "C", "_END"].iter().enumerate() {
                v.push((format!("{side_src}Hand{fs}{}", k + 1), format!("{side_dst}{fd}{suf}")));
            }
        }
    }
    v
}

/// Repère orthonormé (direction principale, vecteur secondaire rendu perpendiculaire).
fn frame(dir: Vec3, side: Vec3) -> Option<Mat3> {
    let x = dir.normalize_or_zero();
    let y = (side - x * side.dot(x)).normalize_or_zero();
    if x.length_squared() < 0.5 || y.length_squared() < 0.5 {
        return None;
    }
    Some(Mat3::from_cols(x, y, x.cross(y)))
}

/// Rotation qui amène `from` sur `to` (directions, avec un vecteur secondaire facultatif).
fn align(from_dir: Vec3, from_side: Option<Vec3>, to_dir: Vec3, to_side: Option<Vec3>) -> Quat {
    if let (Some(fs), Some(ts)) = (from_side, to_side) {
        if let (Some(a), Some(b)) = (frame(from_dir, fs), frame(to_dir, ts)) {
            return Quat::from_mat3(&(b * a.transpose())).normalize();
        }
    }
    let (a, b) = (from_dir.normalize_or_zero(), to_dir.normalize_or_zero());
    if a.length_squared() < 0.5 || b.length_squared() < 0.5 {
        return Quat::IDENTITY;
    }
    Quat::from_rotation_arc(a, b)
}

fn rot_of(m: &Mat4) -> Quat {
    let (_, r, _) = m.to_scale_rotation_translation();
    r.normalize()
}

/// Correspondance trouvée entre les deux squelettes.
struct Pair {
    src: usize,
    dst: usize,
    end: bool,
}

/// Transfère l'animation sur le squelette `sk`. `name` devient le nom du clip.
pub fn retarget(anim: &FbxAnimation, sk: &Skeleton, name: &str) -> Result<DecodedClip> {
    let src_index = |n: &str| src_find(anim, n);
    let dst_index = |n: &str| sk.bones.iter().position(|b| b.name.eq_ignore_ascii_case(n));
    let pairs: Vec<Pair> = full_map()
        .iter()
        .filter_map(|(s, d)| Some(Pair { src: src_index(s)?, dst: dst_index(d)?, end: is_end(d) }))
        .collect();
    let find = |dst_name: &str| pairs.iter().find(|p| sk.bones[p.dst].name.eq_ignore_ascii_case(dst_name));
    let (Some(hips), Some(lhip), Some(rhip), Some(neck)) = (find("COG"), find("L_HIP"), find("R_HIP"), find("NECK")) else {
        bail!("squelette non reconnu : il faut un squelette Mixamo (Hips, Spine, LeftUpLeg, Neck…) ou Unreal (pelvis, thigh_l, neck_01…) et un soldat");
    };
    let animated = pairs.iter().filter(|p| !p.end).count();
    if animated < 10 {
        bail!("trop peu d'os reconnus ({animated}) : ce FBX n'utilise ni les noms Mixamo ni les noms Unreal");
    }

    // ---- repères des deux squelettes au repos
    let src_pos = |i: usize| anim.rest_world[i].w_axis.truncate();
    let dst_world: Vec<Mat4> = sk.bones.iter().map(|b| b.bind_world).collect();
    let dst_pos = |i: usize| dst_world[i].w_axis.truncate();

    // orientation du personnage : gauche (hanche droite → gauche) et haut (bassin → cou)
    let basis = |left: Vec3, up: Vec3| frame(left, up);
    let (Some(bs), Some(bd)) = (
        basis(src_pos(lhip.src) - src_pos(rhip.src), src_pos(neck.src) - src_pos(hips.src)),
        basis(dst_pos(lhip.dst) - dst_pos(rhip.dst), dst_pos(neck.dst) - dst_pos(hips.dst)),
    ) else {
        bail!("orientation du squelette FBX indéterminée");
    };
    // repère FBX → repère du soldat
    let m = Quat::from_mat3(&(bd * bs.transpose())).normalize();

    // échelle : longueur des jambes
    let leg_len = |src: bool| -> Option<f32> {
        let idx = |n: &str| find(n).map(|p| if src { p.src } else { p.dst });
        let pos = |i: usize| if src { src_pos(i) } else { dst_pos(i) };
        let (a, b, c) = (idx("L_HIP")?, idx("L_KNEE")?, idx("L_ANKLE")?);
        Some(pos(a).distance(pos(b)) + pos(b).distance(pos(c)))
    };
    let scale = match (leg_len(true), leg_len(false)) {
        (Some(s), Some(d)) if s > 1e-6 => d / s,
        _ => {
            let s = src_pos(neck.src).distance(src_pos(hips.src));
            let d = dst_pos(neck.dst).distance(dst_pos(hips.dst));
            if s > 1e-6 {
                d / s
            } else {
                0.01
            }
        }
    };

    // ---- pose d'alignement : rotation monde corrective de chaque os du soldat
    let child_of = |p: &Pair| -> Option<&Pair> {
        // enfant associé qui donne la direction de l'os (premier trouvé dans l'ordre de la table)
        let preferred: &[&str] = match sk.bones[p.dst].name.to_ascii_uppercase().as_str() {
            "COG" => &[],
            "SPINEUPPER" => &["NECK"],
            n if n.ends_with("_WRIST") => &["MIDDLEA"],
            _ => &[],
        };
        let side = &sk.bones[p.dst].name[..2.min(sk.bones[p.dst].name.len())];
        for pref in preferred {
            let want = if *pref == "NECK" { pref.to_string() } else { format!("{side}{pref}") };
            if let Some(c) = pairs.iter().find(|c| sk.bones[c.dst].name.eq_ignore_ascii_case(&want)) {
                return Some(c);
            }
        }
        if sk.bones[p.dst].name.eq_ignore_ascii_case("COG") {
            return None;
        }
        pairs.iter().find(|c| sk.bones[c.dst].parent == Some(p.dst))
    };
    // largeur de la main (index → auriculaire) : fixe la rotation des mains et des doigts sur leur axe
    let hand_side = |side: &str, src: bool| -> Option<Vec3> {
        let idx = |n: &str| find(&format!("{side}{n}")).map(|p| if src { p.src } else { p.dst });
        let (i, k) = (idx("INDEXA")?, idx("PINKYA")?);
        Some(if src { m * (src_pos(i) - src_pos(k)) } else { dst_pos(i) - dst_pos(k) })
    };
    let mut correction = vec![Quat::IDENTITY; sk.bones.len()];
    for p in pairs.iter().filter(|p| !p.end) {
        let Some(c) = child_of(p) else { continue };
        let sdir = m * (src_pos(c.src) - src_pos(p.src));
        let ddir = dst_pos(c.dst) - dst_pos(p.dst);
        let name = sk.bones[p.dst].name.to_ascii_uppercase();
        let side = &name[..2.min(name.len())];
        let in_hand = (side == "L_" || side == "R_")
            && (name.ends_with("_WRIST") || FINGERS.iter().any(|(_, f)| name[2..].starts_with(f)) || name.ends_with("_ELBOW"));
        let (ss, ds) = if in_hand { (hand_side(side, true), hand_side(side, false)) } else { (None, None) };
        correction[p.dst] = align(ddir, ds, sdir, ss);
    }

    // ---- images
    let src_rest_rot: Vec<Quat> = anim.rest_world.iter().map(rot_of).collect();
    let dst_rest_rot: Vec<Quat> = dst_world.iter().map(rot_of).collect();
    let mut src_of = vec![None; sk.bones.len()];
    for p in pairs.iter().filter(|p| !p.end) {
        src_of[p.dst] = Some(p.src);
    }
    let order = sk.eval_order();
    let hips_rest = src_pos(hips.src);
    let cog_rest = dst_pos(hips.dst);
    // os écrits dans le clip : ceux qui sont associés (plus le COG pour la translation)
    let written: Vec<usize> = (0..sk.bones.len()).filter(|&b| src_of[b].is_some()).collect();

    let mut frames = Vec::with_capacity(anim.frames.len());
    for f in &anim.frames {
        let mut world = vec![Mat4::IDENTITY; sk.bones.len()];
        let mut local = vec![(Quat::IDENTITY, Vec3::ZERO); sk.bones.len()];
        for &b in &order {
            let bone = &sk.bones[b];
            let parent_world = bone.parent.map(|p| world[p]).unwrap_or(Mat4::IDENTITY);
            let parent_rot = rot_of(&parent_world);
            let mut t = bone.rest_local.translation;
            let rot_world = match src_of[b] {
                Some(s) => {
                    // rotation monde Mixamo par rapport à son repos, exprimée dans le repère du soldat
                    let delta = m * (rot_of(&f[s]) * src_rest_rot[s].inverse()) * m.inverse();
                    (delta * correction[b] * dst_rest_rot[b]).normalize()
                }
                None => (parent_rot * bone.rest_local.rotation).normalize(),
            };
            if b == hips.dst {
                let want = cog_rest + m * (f[hips.src].w_axis.truncate() - hips_rest) * scale;
                t = parent_world.inverse().transform_point3(want);
            }
            let rot_local = (parent_rot.inverse() * rot_world).normalize();
            local[b] = (rot_local, t);
            world[b] = parent_world * Mat4::from_scale_rotation_translation(bone.rest_local.scale, rot_local, t);
        }
        frames.push(written.iter().map(|&b| local[b]).collect::<Vec<_>>());
    }

    // continuité des quaternions d'une image à l'autre (interpolation sans retournement)
    for k in 1..frames.len() {
        for a in 0..written.len() {
            let prev = frames[k - 1][a].0;
            if prev.dot(frames[k][a].0) < 0.0 {
                frames[k][a].0 = -frames[k][a].0;
            }
        }
    }

    Ok(DecodedClip {
        name: name.to_string(),
        sample_rate: anim.fps,
        bone_hashes: written.iter().map(|&b| sk.bones[b].hash).collect(),
        bind: written.iter().map(|&b| (sk.bones[b].rest_local.rotation, sk.bones[b].rest_local.translation)).collect(),
        frames,
        trajectory: None,
    })
}
