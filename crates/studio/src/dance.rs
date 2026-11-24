//! Danses procédurales : poses calculées par cinématique inverse (IK).
//!
//! On place les pieds et les mains dans l'espace du personnage, le squelette suit
//! (IK à deux os pour bras et jambes). Fonctionne sur tout squelette humain du jeu
//! (TR, NC, VS, NSO) car tout est calculé à partir de sa pose de repos.
//!
//! Repère du personnage : y vers le haut, il regarde vers +z, sa gauche est +x.

use crate::anim::{Interp, Key};
use crate::scene::Actor;
use forgelight::{Skeleton, Transform};
use glam::{Mat4, Quat, Vec3};
use std::f32::consts::{PI, TAU};

/// Indices des os utilisés.
pub(crate) struct Rig {
    pub(crate) cog: usize,
    pub(crate) spine_lower: usize,
    pub(crate) neck: Option<usize>,
    pub(crate) head: Option<usize>,
    pub(crate) shoulder: [usize; 2],
    pub(crate) elbow: [usize; 2],
    pub(crate) wrist: [usize; 2],
    pub(crate) hip: [usize; 2],
    pub(crate) knee: [usize; 2],
    pub(crate) ankle: [usize; 2],
    /// doigts : [main][doigt] = (A, B, C), doigt 0 = pouce
    pub(crate) fingers: [[Option<[usize; 3]>; 5]; 2],
}

const FINGERS: [&str; 5] = ["THUMB", "INDEX", "MIDDLE", "RING", "PINKY"];

impl Rig {
    pub(crate) fn find(sk: &Skeleton) -> Option<Self> {
        let f = |n: &str| sk.bones.iter().position(|b| b.name.eq_ignore_ascii_case(n));
        let pair = |n: &str| Some([f(&format!("L_{n}"))?, f(&format!("R_{n}"))?]);
        Some(Self {
            cog: f("COG")?,
            spine_lower: f("SPINELOWER")?,
            neck: f("NECK"),
            head: f("HEAD"),
            shoulder: pair("SHOULDER")?,
            elbow: pair("ELBOW")?,
            wrist: pair("WRIST")?,
            hip: pair("HIP")?,
            knee: pair("KNEE")?,
            ankle: pair("ANKLE")?,
            fingers: [0, 1].map(|s| {
                let side = if s == 0 { "L" } else { "R" };
                FINGERS.map(|fg| Some([f(&format!("{side}_{fg}A"))?, f(&format!("{side}_{fg}B"))?, f(&format!("{side}_{fg}C"))?]))
            }),
        })
    }
}

/// Pose en cours de construction (transformations locales + matrices "monde" du squelette).
pub(crate) struct Poser<'a> {
    pub(crate) sk: &'a Skeleton,
    pub(crate) pose: Vec<Transform>,
    pub(crate) world: Vec<Mat4>,
}

impl<'a> Poser<'a> {
    pub(crate) fn new(sk: &'a Skeleton) -> Self {
        let pose = sk.rest_pose();
        let world = sk.world_matrices(&pose, Mat4::IDENTITY);
        Self { sk, pose, world }
    }
    pub(crate) fn refresh(&mut self) {
        self.world = self.sk.world_matrices(&self.pose, Mat4::IDENTITY);
    }
    pub(crate) fn pos(&self, b: usize) -> Vec3 {
        self.world[b].w_axis.truncate()
    }
    pub(crate) fn parent_rot(&self, b: usize) -> Quat {
        match self.sk.bones[b].parent {
            Some(p) => {
                let (_, r, _) = self.world[p].to_scale_rotation_translation();
                r.normalize()
            }
            None => Quat::IDENTITY,
        }
    }
    /// Rotation exprimée dans le repère du personnage, appliquée à un os.
    pub(crate) fn rotate(&mut self, b: usize, q: Quat) {
        let pr = self.parent_rot(b);
        let t = &mut self.pose[b];
        t.rotation = (pr.inverse() * q * pr * t.rotation).normalize();
        self.refresh();
    }
    pub(crate) fn translate(&mut self, b: usize, d: Vec3) {
        let pr = self.parent_rot(b);
        self.pose[b].translation += pr.inverse() * d;
        self.refresh();
    }
    /// Oriente l'os `b` pour que l'articulation `child` aille vers `target`.
    pub(crate) fn aim(&mut self, b: usize, child: usize, target: Vec3) {
        let cur = (self.pos(child) - self.pos(b)).normalize_or_zero();
        let want = (target - self.pos(b)).normalize_or_zero();
        if cur.length_squared() > 0.5 && want.length_squared() > 0.5 {
            self.rotate(b, Quat::from_rotation_arc(cur, want));
        }
    }
    /// IK à deux os : racine `a`, milieu `m`, extrémité `e`, vers `target`, coude/genou vers `pole`.
    pub(crate) fn two_bone(&mut self, a: usize, m: usize, e: usize, target: Vec3, pole: Vec3) {
        let s = self.pos(a);
        let la = self.pos(m).distance(s);
        let lb = self.pos(e).distance(self.pos(m));
        let to = target - s;
        let d = to.length().clamp(0.05, (la + lb) * 0.999);
        let n = to.normalize_or(Vec3::NEG_Y);
        let cos_a = ((la * la + d * d - lb * lb) / (2.0 * la * d)).clamp(-1.0, 1.0);
        let sin_a = (1.0 - cos_a * cos_a).sqrt();
        let side = (pole - n * pole.dot(n)).normalize_or(n.any_orthonormal_vector());
        let mid = s + n * la * cos_a + side * la * sin_a;
        self.aim(a, m, mid);
        self.aim(m, e, s + n * d);
    }
    /// Normale de la paume (vers l'extérieur de la paume), déduite des doigts.
    pub(crate) fn palm_normal(&self, rig: &Rig, s: usize) -> Option<Vec3> {
        let f = &rig.fingers[s];
        let (index, middle, pinky) = (f[1]?, f[2]?, f[4]?);
        let along = self.pos(middle[0]) - self.pos(rig.wrist[s]);
        let across = self.pos(index[0]) - self.pos(pinky[0]);
        let n = along.cross(across);
        let n = if s == 0 { n } else { -n };
        Some(n.normalize_or_zero())
    }

    /// Fait tourner l'avant-bras sur lui-même pour orienter la paume vers `want`.
    pub(crate) fn twist_palm(&mut self, rig: &Rig, s: usize, want: Vec3) {
        let Some(n) = self.palm_normal(rig, s) else { return };
        let axis = (self.pos(rig.wrist[s]) - self.pos(rig.elbow[s])).normalize_or_zero();
        let a = (n - axis * n.dot(axis)).normalize_or_zero();
        let b = (want - axis * want.dot(axis)).normalize_or_zero();
        if a.length_squared() < 0.5 || b.length_squared() < 0.5 {
            return;
        }
        let angle = a.cross(b).dot(axis).atan2(a.dot(b));
        self.rotate(rig.elbow[s], Quat::from_axis_angle(axis, angle));
    }

    /// Referme les doigts (poing) : chaque phalange tourne vers la paume.
    pub(crate) fn make_fist(&mut self, rig: &Rig, s: usize, amount: f32) {
        self.curl_fingers(rig, s, [amount; 5]);
    }

    /// Plie chaque doigt : `amount[d]` de 0 (tendu) à 1 (replié), doigt 0 = pouce.
    /// Une valeur négative tend le doigt vers l'arrière (pouce levé).
    pub(crate) fn curl_fingers(&mut self, rig: &Rig, s: usize, amount: [f32; 5]) {
        for (i, finger) in rig.fingers[s].iter().enumerate() {
            let Some(bones) = finger else { continue };
            if amount[i] == 0.0 {
                continue;
            }
            let angles: [f32; 3] = if i == 0 { [15.0, 25.0, 25.0] } else { [70.0, 80.0, 55.0] };
            for k in 0..3 {
                let Some(n) = self.palm_normal(rig, s) else { return };
                let b = bones[k];
                let next = if k < 2 { self.pos(bones[k + 1]) } else { self.pos(b) * 2.0 - self.pos(bones[1]) };
                let dir = (next - self.pos(b)).normalize_or_zero();
                // n sort de la paume : les doigts se plient vers n (vérifié au rendu, signe V et poing)
                let axis = dir.cross(n).normalize_or_zero();
                if axis.length_squared() > 0.5 {
                    self.rotate(b, Quat::from_axis_angle(axis, (angles[k] * amount[i]).to_radians()));
                }
            }
        }
    }

    /// Remet l'os dans son orientation de repos (dans le repère du personnage).
    pub(crate) fn keep_rest_orientation(&mut self, b: usize) {
        let (_, rest_world, _) = self.sk.bones[b].bind_world.to_scale_rotation_translation();
        let pr = self.parent_rot(b);
        self.pose[b].rotation = (pr.inverse() * rest_world.normalize()).normalize();
        self.refresh();
    }
}

/// Lissage 0..1 -> 0..1.
pub(crate) fn ease(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// Pose de la "danse du cheval" à l'instant `t` (secondes).
/// Cycle de 16 temps : 8 temps de "rênes" puis 8 temps de "lasso".
pub fn horse_dance_pose(sk: &Skeleton, t: f32, bpm: f32) -> Option<Vec<Transform>> {
    let rig = Rig::find(sk)?;
    let mut p = Poser::new(sk);
    let rest = |b: usize| sk.bones[b].bind_world.w_axis.truncate();

    let beat = t * bpm / 60.0;
    let ph = beat.fract();
    // creux du rebond sur chaque temps
    let bounce = 0.5 + 0.5 * (TAU * beat).cos();
    let cycle = beat % 16.0;
    // passage progressif rênes -> lasso -> rênes
    let lasso = ease((cycle - 7.5) * 2.0) * (1.0 - ease((cycle - 15.5) * 2.0));

    // --- bassin : accroupi, rebond, léger balancement latéral
    let sway = (PI * beat).sin() * 0.03;
    let drop = -0.10 - 0.05 * bounce;
    p.translate(rig.cog, Vec3::new(sway, drop, 0.0));
    // buste penché en avant
    p.rotate(rig.spine_lower, Quat::from_rotation_x(10f32.to_radians() + 3f32.to_radians() * bounce));

    // --- jambes : un pied d'appui, l'autre tape le sol ; on change tous les 2 temps
    let free = ((beat / 2.0).floor() as i32).rem_euclid(2) as usize; // 0 = gauche libre, 1 = droite
    for s in 0..2 {
        let sign = if s == 0 { 1.0 } else { -1.0 };
        let base = rest(rig.ankle[s]);
        let mut target = Vec3::new(sign * (base.x.abs() + 0.10), base.y, base.z + 0.02);
        if s == free {
            let lift = 0.5 - 0.5 * (TAU * ph).cos(); // 0 sur le temps, 1 entre deux temps
            target.y += 0.11 * lift;
            target.z += 0.08 * lift;
        }
        let pole = Vec3::new(sign * 0.35, 0.0, 1.0);
        p.two_bone(rig.hip[s], rig.knee[s], rig.ankle[s], target, pole);
        p.keep_rest_orientation(rig.ankle[s]);
    }

    // --- bras : poignets croisés devant le ventre (les rênes), qui sautent avec le rythme
    let belly = rest(rig.spine_lower);
    let hands_y = belly.y + 0.12 + drop + 0.05 * (1.0 - bounce);
    let hands_z = belly.z + 0.34;
    for s in 0..2 {
        let sign = if s == 0 { 1.0 } else { -1.0 };
        // croisés : la main gauche passe légèrement à droite et inversement
        let reins = Vec3::new(-sign * 0.035, hands_y + if s == 0 { 0.02 } else { 0.0 }, hands_z);
        let reins_pole = Vec3::new(sign * 1.0, -0.6, -0.4);
        let (target, pole) = if s == 1 && lasso > 0.0 {
            // main droite levée qui fait tourner le lasso au-dessus de la tête
            let head = rig.head.map(rest).unwrap_or(belly + Vec3::Y * 0.6);
            let a = TAU * beat;
            let top = Vec3::new(-0.06 + 0.13 * a.cos(), head.y + 0.32 + drop, head.z + 0.04 + 0.13 * a.sin());
            let lasso_pole = Vec3::new(-1.0, 0.2, -0.2);
            (reins.lerp(top, lasso), reins_pole.lerp(lasso_pole, lasso))
        } else if s == 0 && lasso > 0.0 {
            // la main gauche reste sur les rênes, au centre
            (reins.lerp(Vec3::new(0.0, hands_y, hands_z), lasso), reins_pole)
        } else {
            (reins, reins_pole)
        };
        p.two_bone(rig.shoulder[s], rig.elbow[s], rig.wrist[s], target, pole);
        // paumes vers le bas sur les rênes ; vers l'avant pour la main du lasso
        let palm_reins = Vec3::new(-sign * 0.3, -1.0, 0.1).normalize();
        let palm = if s == 1 { palm_reins.lerp(Vec3::new(0.0, 0.2, 1.0), lasso).normalize() } else { palm_reins };
        p.twist_palm(&rig, s, palm);
        p.make_fist(&rig, s, 1.0);
    }

    // --- tête : hochement sur le rythme
    if let Some(h) = rig.head.or(rig.neck) {
        p.rotate(h, Quat::from_rotation_x(-6f32.to_radians() * (TAU * beat).cos()));
    }
    Some(p.pose)
}

/// Écrit la danse en clés dans la timeline de l'acteur. Renvoie la dernière image.
pub fn bake_horse_dance(actor: &mut Actor, start: i32, fps: f32, bpm: f32, seconds: f32) -> Option<i32> {
    let sk = actor.model.skeleton.clone();
    Rig::find(&sk)?;
    let frames = (seconds * fps).round().max(1.0) as i32;
    for k in 0..=frames {
        let pose = horse_dance_pose(&sk, k as f32 / fps, bpm)?;
        for (b, tr) in pose.into_iter().enumerate() {
            if tr != sk.bones[b].rest_local || !actor.anim.bones[b].is_empty() {
                actor.anim.bones[b].set_key(Key::new(start + k, &tr, Interp::Linear));
            }
        }
    }
    Some(start + frames)
}
