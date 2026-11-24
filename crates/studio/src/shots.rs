//! Plans de caméra prédéfinis, ciblés sur un personnage : tours à 360°, demi-tour, arc,
//! spirale, travellings avant / arrière, grue, et cadrages fixes (face, trois-quarts, profil…).
//!
//! Un plan animé devient des clés de la caméra de prise, assez serrées pour que la spline
//! lissée suive le cercle à moins d'un millimètre près. Elles restent modifiables ensuite
//! comme n'importe quelle clé.

use crate::anim::{CamKey, CamTrack, Interp};
use crate::camera::ViewParams;
use crate::scene::Scene;
use glam::Vec3;
use serde::{Deserialize, Serialize};

/// Mouvement de caméra autour du personnage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Move {
    Orbit360,
    Orbit180,
    Arc90,
    Spiral,
    PushIn,
    PullOut,
    Crane,
}

impl Move {
    pub const ALL: [Move; 7] =
        [Move::Orbit360, Move::Orbit180, Move::Arc90, Move::Spiral, Move::PushIn, Move::PullOut, Move::Crane];

    pub fn label(self) -> &'static str {
        match self {
            Move::Orbit360 => tr!("⟳ Tour 360°", "⟳ Orbit 360°"),
            Move::Orbit180 => tr!("◠ Demi-tour 180°", "◠ Half orbit 180°"),
            Move::Arc90 => tr!("◜ Arc 90°", "◜ Arc 90°"),
            Move::Spiral => tr!("🌀 Spirale montante", "🌀 Rising spiral"),
            Move::PushIn => tr!("➡ Travelling avant", "➡ Push in"),
            Move::PullOut => tr!("⬅ Travelling arrière", "⬅ Pull out"),
            Move::Crane => tr!("⬆ Grue", "⬆ Crane"),
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Move::Orbit360 => tr!("Un tour complet autour du personnage, à distance constante", "A full turn around the character, at a constant distance"),
            Move::Orbit180 => tr!("Un demi-tour : de face jusqu'au dos (ou depuis le côté choisi)", "Half a turn: from the front to the back (or from the chosen side)"),
            Move::Arc90 => tr!("Un quart de tour, lent et discret", "A quarter turn, slow and subtle"),
            Move::Spiral => tr!("Un tour complet en montant : de la contre-plongée à la plongée", "A full turn while rising: from a low angle to a high angle"),
            Move::PushIn => tr!("La caméra s'approche du personnage jusqu'au cadrage choisi", "The camera moves in on the character up to the chosen framing"),
            Move::PullOut => tr!("La caméra part du cadrage choisi et recule", "The camera starts at the chosen framing and moves back"),
            Move::Crane => tr!("La caméra monte en restant tournée vers le personnage", "The camera rises while staying aimed at the character"),
        }
    }

    /// Rotation totale autour du personnage, en degrés.
    fn sweep(self) -> f32 {
        match self {
            Move::Orbit360 | Move::Spiral => 360.0,
            Move::Orbit180 => 180.0,
            Move::Arc90 => 90.0,
            Move::PushIn | Move::PullOut | Move::Crane => 0.0,
        }
    }
}

/// Taille du personnage dans l'image.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Framing {
    CloseUp,
    Feet,
    Medium,
    Full,
    Wide,
}

impl Framing {
    pub const ALL: [Framing; 5] = [Framing::CloseUp, Framing::Feet, Framing::Medium, Framing::Full, Framing::Wide];

    pub fn label(self) -> &'static str {
        match self {
            Framing::CloseUp => tr!("Gros plan (visage)", "Close-up (face)"),
            Framing::Feet => tr!("Gros plan (pieds)", "Close-up (feet)"),
            Framing::Medium => tr!("Plan taille", "Medium shot"),
            Framing::Full => tr!("Plan en pied", "Full shot"),
            Framing::Wide => tr!("Plan large", "Wide shot"),
        }
    }

    /// Hauteur visée, en fraction de la taille du personnage.
    fn aim(self) -> f32 {
        match self {
            Framing::CloseUp => 0.93,
            Framing::Feet => 0.06,
            Framing::Medium => 0.72,
            Framing::Full => 0.52,
            Framing::Wide => 0.45,
        }
    }

    /// Hauteur de l'image au niveau du personnage, en fraction de sa taille.
    fn extent(self) -> f32 {
        match self {
            Framing::CloseUp => 0.24,
            // les deux pieds et le bas des tibias
            Framing::Feet => 0.28,
            Framing::Medium => 0.62,
            Framing::Full => 1.25,
            Framing::Wide => 3.0,
        }
    }
}

/// Hauteur de la caméra.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Height {
    Low,
    Eye,
    High,
    Top,
}

impl Height {
    pub const ALL: [Height; 4] = [Height::Low, Height::Eye, Height::High, Height::Top];

    pub fn label(self) -> &'static str {
        match self {
            Height::Low => tr!("Contre-plongée", "Low angle"),
            Height::Eye => tr!("À hauteur", "Eye level"),
            Height::High => tr!("Plongée", "High angle"),
            Height::Top => tr!("Vue du dessus", "Top view"),
        }
    }

    /// Angle de la caméra au-dessus du point visé, en degrés.
    fn elevation(self) -> f32 {
        match self {
            Height::Low => -18.0,
            Height::Eye => 3.0,
            Height::High => 30.0,
            Height::Top => 65.0,
        }
    }
}

/// Côté d'où la caméra regarde le personnage (au départ d'un mouvement).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Front,
    ThreeQuarterRight,
    ProfileRight,
    Back,
    ProfileLeft,
    ThreeQuarterLeft,
}

impl Side {
    pub const ALL: [Side; 6] =
        [Side::Front, Side::ThreeQuarterRight, Side::ProfileRight, Side::Back, Side::ProfileLeft, Side::ThreeQuarterLeft];

    pub fn label(self) -> &'static str {
        match self {
            Side::Front => tr!("Face", "Front"),
            Side::ThreeQuarterRight => tr!("Trois-quarts droit", "Three-quarter right"),
            Side::ProfileRight => tr!("Profil droit", "Right profile"),
            Side::Back => tr!("Dos", "Back"),
            Side::ProfileLeft => tr!("Profil gauche", "Left profile"),
            Side::ThreeQuarterLeft => tr!("Trois-quarts gauche", "Three-quarter left"),
        }
    }

    /// Angle autour du personnage, en degrés (0 = face, + = vers sa gauche).
    /// Repère du personnage : il regarde vers +z, sa gauche est +x.
    fn azimuth(self) -> f32 {
        match self {
            Side::Front => 0.0,
            Side::ThreeQuarterRight => -40.0,
            Side::ProfileRight => -90.0,
            Side::Back => 180.0,
            Side::ProfileLeft => 90.0,
            Side::ThreeQuarterLeft => 40.0,
        }
    }
}

/// Réglages des plans, enregistrés dans la configuration.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShotSettings {
    /// mouvement choisi (`None` : plan fixe)
    pub choice: Option<Move>,
    pub framing: Framing,
    pub height: Height,
    pub side: Side,
    /// sens horaire vu du dessus
    pub clockwise: bool,
    /// durée d'un plan animé, en secondes
    pub secs: f32,
    /// départ et arrivée en douceur (sinon vitesse constante, pour une boucle)
    pub ease: bool,
    /// la caméra suit le personnage s'il se déplace
    pub follow: bool,
    /// remplace toute l'animation de la caméra (sinon : insère le plan à l'image courante,
    /// avec une coupe franche)
    pub replace: bool,
    /// distance imposée (sinon : celle qui donne le cadrage choisi avec le champ de vision)
    pub dist_manual: bool,
    /// distance entre la caméra et le point visé, en mètres
    pub dist_m: f32,
    /// champ de vision imposé (sinon : celui de la caméra de prise)
    pub fov_manual: bool,
    /// champ de vision vertical, en degrés
    pub fov_deg: f32,
}

impl Default for ShotSettings {
    fn default() -> Self {
        Self {
            choice: Some(Move::Orbit360),
            framing: Framing::Full,
            height: Height::Eye,
            side: Side::Front,
            clockwise: false,
            secs: 8.0,
            ease: true,
            follow: true,
            replace: true,
            dist_manual: false,
            dist_m: 4.0,
            fov_manual: false,
            fov_deg: 40.0,
        }
    }
}

impl ShotSettings {
    /// Champ de vision du plan : le sien s'il est imposé, sinon `current` (celui de la caméra).
    pub fn fov(&self, current: f32) -> f32 {
        if self.fov_manual {
            self.fov_deg.clamp(MIN_FOV, MAX_FOV)
        } else {
            current
        }
    }

    /// Distance au point visé (avant le recul d'un travelling) : imposée, sinon celle du cadrage.
    pub fn distance(&self, subj: &Subject, fov: f32) -> f32 {
        if self.dist_manual {
            self.dist_m.clamp(MIN_DIST, MAX_DIST)
        } else {
            auto_distance(self.framing, subj, fov)
        }
    }
}

pub const MIN_DIST: f32 = 0.2;
pub const MAX_DIST: f32 = 200.0;
pub const MIN_FOV: f32 = 5.0;
pub const MAX_FOV: f32 = 120.0;

/// Distance qui met `framing.extent()` de la taille du personnage dans la hauteur de l'image.
pub fn auto_distance(framing: Framing, subj: &Subject, fov: f32) -> f32 {
    let half = (fov.clamp(MIN_FOV, 150.0).to_radians() * 0.5).tan();
    (subj.height * framing.extent() * 0.5 / half).max(MIN_DIST)
}

/// Ce que la caméra cadre : un personnage à une image.
#[derive(Clone, Copy, Debug)]
pub struct Subject {
    /// position au sol (centre du bassin, hauteur des pieds)
    pub ground: Vec3,
    /// taille, en mètres
    pub height: f32,
    /// position de la tête, si le squelette en a une
    pub head: Option<Vec3>,
    /// milieu des pieds (chevilles et plantes), si le squelette en a
    pub feet: Option<Vec3>,
    /// direction du regard (radians autour de y ; 0 = vers +z)
    pub facing: f32,
}

impl Subject {
    fn aim(&self, framing: Framing) -> Vec3 {
        match (framing, self.head) {
            // l'os de la tête est à la base du crâne : on vise les yeux
            (Framing::CloseUp, Some(h)) => h + Vec3::Y * 0.08,
            // les os des pieds sont près du sol : on vise un peu au-dessus, au niveau des chevilles
            (Framing::Feet, _) => match self.feet {
                Some(f) => Vec3::new(f.x, f.y.max(self.ground.y) + self.height * 0.02, f.z),
                None => self.ground + Vec3::Y * self.height * framing.aim(),
            },
            _ => self.ground + Vec3::Y * self.height * framing.aim(),
        }
    }
}

/// Acteur racine d'un acteur attaché (tête, casque, arme → le corps qui les porte).
pub fn root_actor(scene: &Scene, mut id: u64) -> u64 {
    for _ in 0..16 {
        match scene.actor(id).and_then(|a| a.attachment.as_ref()) {
            Some(att) if scene.actor(att.parent()).is_some() => id = att.parent(),
            _ => break,
        }
    }
    id
}

/// Le personnage `id` (et tout ce qui lui est attaché) tel qu'évalué à l'image courante.
pub fn subject(scene: &Scene, id: u64) -> Option<Subject> {
    let root = root_actor(scene, id);
    let actor = scene.actor(root)?;
    let mut lo = Vec3::splat(f32::MAX);
    let mut hi = Vec3::splat(f32::MIN);
    for a in &scene.actors {
        if a.visible && root_actor(scene, a.id) == root {
            let (l, h) = a.bounds_world();
            lo = lo.min(l);
            hi = hi.max(h);
        }
    }
    if lo.x > hi.x {
        (lo, hi) = actor.bounds_world();
    }
    let bone = |name: &str| actor.bone_index(name).and_then(|b| actor.world.get(b)).map(|m| m.w_axis.truncate());
    let center = bone("PELVIS").or_else(|| bone("COG")).unwrap_or((lo + hi) * 0.5);
    let fwd = actor.root_world.transform_vector3(Vec3::Z);
    let facing = if fwd.x.abs() + fwd.z.abs() > 1e-4 { fwd.x.atan2(fwd.z) } else { 0.0 };
    let foot_bones: Vec<Vec3> = ["L_ANKLE", "R_ANKLE", "L_BALL", "R_BALL"].into_iter().filter_map(bone).collect();
    let feet = (!foot_bones.is_empty()).then(|| foot_bones.iter().sum::<Vec3>() / foot_bones.len() as f32);
    Some(Subject {
        ground: Vec3::new(center.x, lo.y, center.z),
        height: (hi.y - lo.y).max(0.3),
        head: bone("HEAD"),
        feet,
        facing,
    })
}

/// Pose de la caméra à l'avancement `u` (0 → 1, déjà adouci) d'un mouvement, ou cadrage
/// fixe (`mv` = `None`). `facing` : orientation du personnage au début du plan (la caméra
/// tourne dans le monde, pas avec lui). `fov` en degrés.
pub fn pose(mv: Option<Move>, s: &ShotSettings, subj: &Subject, facing: f32, u: f32, fov: f32) -> ViewParams {
    let dir = if s.clockwise { -1.0 } else { 1.0 };
    let sweep = mv.map_or(0.0, Move::sweep);
    let azimuth = facing + (s.side.azimuth() + dir * sweep * u).to_radians();
    let base = s.height.elevation();
    let lerp = |a: f32, b: f32| a + (b - a) * u;
    let elevation = match mv {
        Some(Move::Spiral) => lerp(-15.0, 55.0),
        Some(Move::Crane) => lerp(base.min(-12.0), base.max(0.0) + 45.0),
        _ => base,
    }
    .clamp(-80.0, 85.0)
    .to_radians();
    let zoom = match mv {
        Some(Move::PushIn) => lerp(2.6, 1.0),
        Some(Move::PullOut) => lerp(1.0, 2.6),
        _ => 1.0,
    };
    let aim = subj.aim(s.framing);
    let dist = (s.distance(subj, fov) * zoom).max(MIN_DIST);
    let back = Vec3::new(elevation.cos() * azimuth.sin(), elevation.sin(), elevation.cos() * azimuth.cos());
    let mut eye = aim + back * dist;
    // jamais sous les pieds du personnage
    eye.y = eye.y.max(subj.ground.y + 0.12);
    ViewParams { eye, target: aim, fov_deg: fov }
}

/// Avancement adouci : accélère sur le premier quart, vitesse constante, freine sur le
/// dernier quart (sans à-coup : l'accélération part de zéro).
fn ease(t: f32) -> f32 {
    const A: f32 = 0.25;
    let t = t.clamp(0.0, 1.0);
    let vmax = 1.0 / (1.0 - A);
    let ramp = |t: f32| {
        let x = t / A;
        vmax * A * (x * x * x - x * x * x * x * 0.5)
    };
    if t < A {
        ramp(t)
    } else if t > 1.0 - A {
        1.0 - ramp(1.0 - t)
    } else {
        vmax * (A * 0.5 + t - A)
    }
}

/// Clés d'un mouvement de `start` à `start + secs × fps`. `subject_at(image)` donne le
/// personnage à cette image (le même partout si la caméra ne le suit pas).
pub fn plan(mv: Move, s: &ShotSettings, start: i32, fps: f32, fov: f32, mut subject_at: impl FnMut(i32) -> Subject) -> Vec<CamKey> {
    let n = ((s.secs.max(0.1) * fps.max(1.0)).round() as i32).max(2);
    let end = start + n;
    // ≤ 10° de rotation par segment (le cercle reste rond), et des clés assez serrées
    // pour suivre un personnage qui bouge
    let by_angle = (mv.sweep() / 10.0).ceil() as i32;
    let by_time = (n as f32 / if s.follow { 6.0 } else { 15.0 }).ceil() as i32;
    let segs = by_angle.max(by_time).max(4).min(n);
    let mut frames: Vec<i32> = (0..=segs).map(|i| start + (i as f32 * n as f32 / segs as f32).round() as i32).collect();
    if !s.ease && n >= 4 {
        // vitesse constante jusqu'aux bords : un segment linéaire d'une image à chaque bout
        frames.push(start + 1);
        frames.push(end - 1);
    }
    frames.sort_unstable();
    frames.dedup();
    let facing = subject_at(start).facing;
    frames
        .into_iter()
        .map(|f| {
            let t = (f - start) as f32 / n as f32;
            let u = if s.ease { ease(t) } else { t };
            let v = pose(Some(mv), s, &subject_at(f), facing, u, fov);
            let linear = !s.ease && (f == start || f == end - 1);
            CamKey {
                frame: f,
                eye: v.eye.to_array(),
                target: v.target.to_array(),
                fov_deg: v.fov_deg,
                interp: if linear { Interp::Linear } else { Interp::Smooth },
            }
        })
        .collect()
}

/// Écrit les clés d'un plan sur la caméra de prise. `replace` : efface toute l'ancienne
/// animation ; sinon le plan remplace seulement ses images, avec une coupe franche avant
/// (l'ancienne caméra tient sa position jusqu'au plan) et après (le plan tient la sienne
/// jusqu'à la clé suivante).
pub fn insert(track: &mut CamTrack, mut keys: Vec<CamKey>, replace: bool) {
    let (Some(first), Some(last)) = (keys.first().map(|k| k.frame), keys.last().map(|k| k.frame)) else {
        return;
    };
    if replace {
        track.keys = keys;
        return;
    }
    let before = track.keys.iter().any(|k| k.frame < first);
    let after = track.keys.iter().any(|k| k.frame > last);
    let hold = before.then(|| track.pose((first - 1) as f32));
    track.keys.retain(|k| k.frame < first - 1 || k.frame > last);
    if let Some((eye, target, fov_deg)) = hold {
        track.keys.push(CamKey {
            frame: first - 1,
            eye: eye.to_array(),
            target: target.to_array(),
            fov_deg,
            interp: Interp::Step,
        });
    }
    if after {
        keys.last_mut().unwrap().interp = Interp::Step;
    }
    track.keys.extend(keys);
    track.keys.sort_by_key(|k| k.frame);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn soldier() -> Subject {
        Subject {
            ground: Vec3::new(1.0, 0.0, -2.0),
            height: 1.9,
            head: Some(Vec3::new(1.0, 1.7, -2.0)),
            feet: Some(Vec3::new(1.05, 0.07, -1.95)),
            facing: 0.0,
        }
    }

    #[test]
    fn feet_shot_aims_low_and_close() {
        let s = ShotSettings { framing: Framing::Feet, ..Default::default() };
        let v = pose(None, &s, &soldier(), 0.0, 0.0, 40.0);
        // vise les pieds (un peu au-dessus des os), de près, sans passer sous le sol
        assert!((v.target - Vec3::new(1.05, 0.07 + 1.9 * 0.02, -1.95)).length() < 1e-3);
        assert!(v.eye.distance(v.target) < 1.0);
        assert!(v.eye.y >= 0.12 - 1e-4);
        // sans os de pied : au ras du sol, sous le personnage
        let no_feet = Subject { feet: None, ..soldier() };
        let v = pose(None, &s, &no_feet, 0.0, 0.0, 40.0);
        assert!((v.target - Vec3::new(1.0, 1.9 * 0.06, -2.0)).length() < 1e-3);
    }

    fn track(keys: Vec<CamKey>) -> CamTrack {
        CamTrack { keys, ..Default::default() }
    }

    fn azimuth(eye: Vec3, center: Vec3) -> f32 {
        let d = eye - center;
        d.x.atan2(d.z).to_degrees()
    }

    #[test]
    fn front_shot_faces_the_character() {
        let s = ShotSettings::default();
        let v = pose(None, &s, &soldier(), 0.0, 0.0, 40.0);
        // il regarde vers +z : la caméra est devant lui, tournée vers lui
        assert!(v.eye.z > -2.0 + 2.0, "{:?}", v.eye);
        assert!((v.eye.x - 1.0).abs() < 1e-3);
        assert!((v.target - Vec3::new(1.0, 1.9 * 0.52, -2.0)).length() < 1e-3);
        // profil gauche : du côté +x du personnage
        let s = ShotSettings { side: Side::ProfileLeft, ..s };
        assert!(pose(None, &s, &soldier(), 0.0, 0.0, 40.0).eye.x > 2.0);
        // un personnage tourné : le cadrage tourne avec lui
        let v = pose(None, &ShotSettings::default(), &soldier(), 90f32.to_radians(), 0.0, 40.0);
        assert!(v.eye.x > 2.0 && (v.eye.z + 2.0).abs() < 1e-3);
    }

    #[test]
    fn close_up_aims_at_the_head() {
        let s = ShotSettings { framing: Framing::CloseUp, ..Default::default() };
        let v = pose(None, &s, &soldier(), 0.0, 0.0, 40.0);
        assert!((v.target.y - 1.78).abs() < 1e-3);
        assert!(v.eye.distance(v.target) < 1.0);
    }

    #[test]
    fn manual_distance_and_fov() {
        let subj = soldier();
        let s = ShotSettings { dist_manual: true, dist_m: 7.5, ..Default::default() };
        // distance imposée, quel que soit le champ de vision
        for fov in [20.0, 40.0, 90.0] {
            let v = pose(None, &s, &subj, 0.0, 0.0, fov);
            assert!((v.eye.distance(v.target) - 7.5).abs() < 1e-3);
        }
        // le travelling avant finit à la distance choisie
        let v = pose(Some(Move::PushIn), &s, &subj, 0.0, 1.0, 40.0);
        assert!((v.eye.distance(v.target) - 7.5).abs() < 1e-3);
        // champ de vision : imposé, ou celui de la caméra
        let s = ShotSettings { fov_manual: true, fov_deg: 25.0, ..Default::default() };
        assert_eq!(s.fov(60.0), 25.0);
        assert_eq!(ShotSettings::default().fov(60.0), 60.0);
        // en automatique, un champ plus étroit éloigne la caméra
        let d = |fov| ShotSettings::default().distance(&subj, fov);
        assert!(d(20.0) > d(40.0) * 1.9);
    }

    #[test]
    fn low_angle_stays_above_ground() {
        let s = ShotSettings { height: Height::Low, framing: Framing::Wide, ..Default::default() };
        let v = pose(None, &s, &soldier(), 0.0, 0.0, 40.0);
        assert!(v.eye.y >= 0.12 - 1e-4);
    }

    #[test]
    fn orbit_is_a_round_full_turn() {
        let s = ShotSettings { ease: false, follow: false, ..Default::default() };
        let subj = soldier();
        let keys = plan(Move::Orbit360, &s, 10, 30.0, 40.0, |_| subj);
        assert_eq!(keys.first().unwrap().frame, 10);
        assert_eq!(keys.last().unwrap().frame, 10 + 240);
        let t = track(keys);
        let aim = Vec3::new(1.0, 1.9 * 0.52, -2.0);
        let (e0, _, _) = t.pose(10.0);
        let r = e0.distance(aim);
        // la boucle se referme, et la spline reste sur le cercle entre les clés
        assert!(t.pose(250.0).0.distance(e0) < 1e-3);
        let mut prev = azimuth(e0, aim);
        let mut step0 = None;
        for f in 11..=250 {
            let (e, target, _) = t.pose(f as f32);
            assert!((e.distance(aim) - r).abs() < r * 2e-3, "image {f} : {} au lieu de {r}", e.distance(aim));
            assert!(target.distance(aim) < 1e-4);
            // vitesse constante d'une image à l'autre (boucle sans à-coup)
            let a = azimuth(e, aim);
            let step = (a - prev + 540.0).rem_euclid(360.0) - 180.0;
            let s0 = *step0.get_or_insert(step);
            assert!((step - s0).abs() < 0.05, "image {f} : {step}° au lieu de {s0}°");
            prev = a;
        }
        // sens anti-horaire vu du dessus : l'angle autour de y augmente
        assert!(step0.unwrap() > 1.4);
    }

    #[test]
    fn clockwise_turns_the_other_way_and_eases() {
        let s = ShotSettings { clockwise: true, follow: false, ..Default::default() };
        let subj = soldier();
        let t = track(plan(Move::Orbit180, &s, 0, 30.0, 40.0, |_| subj));
        let aim = subj.aim(s.framing);
        let a = |f: f32| azimuth(t.pose(f).0, aim);
        assert!(a(20.0) < a(0.0));
        // départ en douceur : premier pas bien plus petit que celui du milieu
        let first = (a(1.0) - a(0.0)).abs();
        let mid = (a(121.0) - a(120.0)).abs();
        assert!(first < mid * 0.2, "{first} / {mid}");
        assert!((a(240.0) - 180.0).abs() < 1e-2 || (a(240.0) + 180.0).abs() < 1e-2);
    }

    #[test]
    fn follows_a_walking_character() {
        let s = ShotSettings::default();
        let walk = |f: i32| Subject { ground: Vec3::new(f as f32 * 0.05, 0.0, 0.0), ..soldier() };
        let keys = plan(Move::Arc90, &s, 0, 30.0, 40.0, walk);
        let t = track(keys);
        for f in [0, 37, 120, 203, 240] {
            let (_, target, _) = t.pose(f as f32);
            assert!((target.x - f as f32 * 0.05).abs() < 0.01, "image {f} : {}", target.x);
        }
    }

    #[test]
    fn push_in_gets_closer_and_crane_rises() {
        let s = ShotSettings::default();
        let subj = soldier();
        let p = |mv, u| pose(Some(mv), &s, &subj, 0.0, u, 40.0);
        let d = |v: ViewParams| v.eye.distance(v.target);
        assert!(d(p(Move::PushIn, 0.0)) > d(p(Move::PushIn, 1.0)) * 2.0);
        assert!(d(p(Move::PullOut, 1.0)) > d(p(Move::PullOut, 0.0)) * 2.0);
        assert!(p(Move::Crane, 1.0).eye.y > p(Move::Crane, 0.0).eye.y + 1.0);
    }

    #[test]
    fn insert_cuts_between_shots() {
        let s = ShotSettings { follow: false, secs: 2.0, ..Default::default() };
        let subj = soldier();
        let mut t = track(vec![
            CamKey { frame: 0, eye: [0.0, 1.0, 9.0], target: [0.0; 3], fov_deg: 40.0, interp: Interp::Smooth },
            CamKey { frame: 100, eye: [9.0, 1.0, 0.0], target: [0.0; 3], fov_deg: 40.0, interp: Interp::Smooth },
            CamKey { frame: 200, eye: [0.0, 1.0, -9.0], target: [0.0; 3], fov_deg: 40.0, interp: Interp::Smooth },
        ]);
        let before = t.pose(49.0);
        let shot = plan(Move::Arc90, &s, 50, 30.0, 40.0, |_| subj);
        let first = shot[0];
        insert(&mut t, shot, false);
        // l'ancienne caméra tient jusqu'à l'image 49, le plan commence net à 50
        assert!(t.pose(49.0).0.distance(before.0) < 1e-4);
        assert!(t.pose(49.5).0.distance(before.0) < 1e-4);
        assert!(t.pose(50.0).0.distance(Vec3::from(first.eye)) < 1e-4);
        // la clé de l'image 100 est remplacée ; celle de 200 reste
        assert!(t.keys.iter().all(|k| k.frame != 100));
        assert!(t.keys.iter().any(|k| k.frame == 200));
        assert!(t.keys.windows(2).all(|w| w[0].frame < w[1].frame));
        // remplacer : seul le plan reste
        let shot = plan(Move::Arc90, &s, 0, 30.0, 40.0, |_| subj);
        insert(&mut t, shot, true);
        assert_eq!(t.keys.first().unwrap().frame, 0);
        assert_eq!(t.keys.last().unwrap().frame, 60);
    }
}
