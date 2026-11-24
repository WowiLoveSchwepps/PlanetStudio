//! Enregistrement en direct de la caméra de prise, avec stabilisation.
//!
//! Pendant l'enregistrement, les commandes (vol, visée, orbite) pilotent une pose « brute ».
//! La caméra affichée et enregistrée la suit à travers un amortisseur critique : les à-coups
//! deviennent de petites corrections douces. À la fin de la prise, une passe repère les
//! accélérations brutales qui restent et les adoucit localement.

use crate::anim::{CamKey, CamTrack, Interp};
use crate::camera::ViewParams;
use glam::Vec3;

/// Prise en cours.
pub struct CamRec {
    /// pose pilotée par les commandes
    pub raw: ViewParams,
    /// pose stabilisée : celle qu'on voit et qu'on enregistre
    pub out: ViewParams,
    vel_eye: Vec3,
    vel_target: Vec3,
    vel_fov: f32,
    /// dernière image (fractionnaire) et pose enregistrées
    last_frame: f32,
    last_out: ViewParams,
    last_key: i32,
    pub first_key: i32,
}

impl CamRec {
    /// Commence la prise à l'image entière `frame` et y pose la première clé.
    pub fn start(track: &mut CamTrack, frame: i32, pose: ViewParams) -> Self {
        key(track, frame, &pose);
        Self {
            raw: pose,
            out: pose,
            vel_eye: Vec3::ZERO,
            vel_target: Vec3::ZERO,
            vel_fov: 0.0,
            last_frame: frame as f32,
            last_out: pose,
            last_key: frame,
            first_key: frame,
        }
    }

    pub fn last_key(&self) -> i32 {
        self.last_key
    }

    /// Fait suivre la pose brute à la pose stabilisée. `stabilize` : 0 (aucune) à 1 (forte).
    pub fn smooth(&mut self, dt: f32, stabilize: f32) {
        let tau = stabilize.clamp(0.0, 1.0) * 0.4;
        if tau < 0.005 || dt <= 0.0 {
            self.out = self.raw;
            self.vel_eye = Vec3::ZERO;
            self.vel_target = Vec3::ZERO;
            self.vel_fov = 0.0;
            return;
        }
        self.out.eye = smooth_damp(self.out.eye, self.raw.eye, &mut self.vel_eye, tau, dt);
        self.out.target = smooth_damp(self.out.target, self.raw.target, &mut self.vel_target, tau, dt);
        let mut v = Vec3::new(self.vel_fov, 0.0, 0.0);
        self.out.fov_deg = smooth_damp(Vec3::new(self.out.fov_deg, 0.0, 0.0), Vec3::new(self.raw.fov_deg, 0.0, 0.0), &mut v, tau, dt).x;
        self.vel_fov = v.x;
    }

    /// La caméra stabilisée a rattrapé les commandes et ne bouge plus.
    pub fn settled(&self) -> bool {
        let gap = self.out.eye.distance(self.raw.eye)
            + self.out.target.distance(self.raw.target)
            + (self.out.fov_deg - self.raw.fov_deg).abs() * 0.01;
        let vel = self.vel_eye.length() + self.vel_target.length() + self.vel_fov.abs() * 0.01;
        gap < 5e-3 && vel < 2e-2
    }

    /// Enregistre une clé à chaque image entière passée depuis la dernière clé.
    pub fn record(&mut self, track: &mut CamTrack, frame: f32) {
        let end = frame.floor() as i32;
        if frame <= self.last_frame {
            return;
        }
        for f in (self.last_key + 1)..=end {
            let t = ((f as f32 - self.last_frame) / (frame - self.last_frame)).clamp(0.0, 1.0);
            let p = ViewParams {
                eye: self.last_out.eye.lerp(self.out.eye, t),
                target: self.last_out.target.lerp(self.out.target, t),
                fov_deg: self.last_out.fov_deg + (self.out.fov_deg - self.last_out.fov_deg) * t,
            };
            key(track, f, &p);
        }
        self.last_key = self.last_key.max(end);
        self.last_frame = frame;
        self.last_out = self.out;
    }
}

fn key(track: &mut CamTrack, frame: i32, p: &ViewParams) {
    track.set_key(CamKey {
        frame,
        eye: p.eye.to_array(),
        target: p.target.to_array(),
        fov_deg: p.fov_deg,
        interp: Interp::Smooth,
    });
    // une clé réécrite garde son ancienne interpolation : la prise est toujours lissée
    if let Ok(i) = track.keys.binary_search_by_key(&frame, |k| k.frame) {
        track.keys[i].interp = Interp::Smooth;
    }
}

/// Amortisseur critique (même formule que le SmoothDamp des moteurs de jeu).
fn smooth_damp(cur: Vec3, target: Vec3, vel: &mut Vec3, tau: f32, dt: f32) -> Vec3 {
    let omega = 2.0 / tau;
    let x = omega * dt;
    let exp = 1.0 / (1.0 + x + 0.48 * x * x + 0.235 * x * x * x);
    let change = cur - target;
    let temp = (*vel + change * omega) * dt;
    *vel = (*vel - temp * omega) * exp;
    target + (change + temp) * exp
}

/// Adoucit les à-coups restants entre les images `from` et `to` : une clé dont l'accélération
/// est brutale par rapport à la vitesse du moment est rapprochée de ses voisines.
/// Les clés de début et de fin de prise ne bougent pas. Renvoie le nombre de corrections.
pub fn smooth_jerks(track: &mut CamTrack, from: i32, to: i32, stabilize: f32) -> usize {
    if stabilize <= 0.0 {
        return 0;
    }
    let lo = track.keys.partition_point(|k| k.frame < from);
    let hi = track.keys.partition_point(|k| k.frame <= to);
    if hi < lo + 3 {
        return 0;
    }
    let mut corrected = std::collections::HashSet::new();
    let passes = 1 + (stabilize * 4.0).round() as usize;
    for _ in 0..passes {
        let mut any = false;
        for i in lo + 1..hi - 1 {
            let (a, b, c) = (track.keys[i - 1], track.keys[i], track.keys[i + 1]);
            // seulement entre images consécutives (la prise pose une clé par image)
            if b.frame - a.frame != 1 || c.frame - b.frame != 1 {
                continue;
            }
            let fix = |get: fn(&CamKey) -> Vec3| -> Option<Vec3> {
                let (pa, pb, pc) = (get(&a), get(&b), get(&c));
                let acc = pa + pc - pb * 2.0;
                let speed = (pc - pa).length() * 0.5;
                // brutal : accélération forte devant la vitesse, ou départ sec depuis l'arrêt
                let limit = 0.12 * speed + 0.002;
                (acc.length() > limit).then(|| pb + acc * 0.25)
            };
            let eye = fix(|k| Vec3::from(k.eye));
            let target = fix(|k| Vec3::from(k.target));
            let k = &mut track.keys[i];
            if let Some(e) = eye {
                k.eye = e.to_array();
            }
            if let Some(t) = target {
                k.target = t.to_array();
            }
            if eye.is_some() || target.is_some() {
                corrected.insert(i);
                any = true;
            }
        }
        if !any {
            break;
        }
    }
    corrected.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp(x: f32) -> ViewParams {
        ViewParams { eye: Vec3::new(x, 1.0, 5.0), target: Vec3::new(x, 1.0, 0.0), fov_deg: 40.0 }
    }

    #[test]
    fn records_every_frame_and_smooths_a_jump() {
        let mut track = CamTrack::default();
        let mut rec = CamRec::start(&mut track, 10, vp(0.0));
        // saut brutal des commandes : la caméra stabilisée le rattrape en douceur
        rec.raw = vp(2.0);
        let mut frame = 10.0;
        let mut prev = 0.0;
        let mut max_step: f32 = 0.0;
        for _ in 0..60 {
            frame += 0.5;
            rec.smooth(1.0 / 60.0, 0.5);
            rec.record(&mut track, frame);
            max_step = max_step.max(rec.out.eye.x - prev);
            prev = rec.out.eye.x;
        }
        assert!(max_step < 0.3, "{max_step}");
        assert!(rec.settled());
        // une clé par image, sans trou
        assert_eq!(track.keys.first().unwrap().frame, 10);
        assert_eq!(track.keys.last().unwrap().frame, 40);
        assert_eq!(track.keys.len(), 31);
        assert!((Vec3::from(track.keys.last().unwrap().eye).x - 2.0).abs() < 1e-2);
    }

    #[test]
    fn without_stabilization_follows_controls() {
        let mut track = CamTrack::default();
        let mut rec = CamRec::start(&mut track, 0, vp(0.0));
        rec.raw = vp(1.0);
        rec.smooth(1.0 / 60.0, 0.0);
        assert_eq!(rec.out.eye, rec.raw.eye);
    }

    #[test]
    fn jerk_pass_softens_a_spike() {
        let mut track = CamTrack::default();
        for f in 0..20 {
            let x = if f == 10 { 10.5 } else { f as f32 }; // secousse à l'image 10
            track.set_key(CamKey { frame: f, eye: [x, 0.0, 0.0], target: [x, 0.0, -5.0], fov_deg: 40.0, interp: Interp::Smooth });
        }
        let n = smooth_jerks(&mut track, 0, 19, 0.5);
        assert!(n > 0);
        let x10 = track.keys[10].eye[0];
        assert!((x10 - 10.0).abs() < 0.25, "{x10}");
        // les extrémités de la prise ne bougent pas, un mouvement régulier non plus
        assert_eq!(track.keys[0].eye[0], 0.0);
        assert_eq!(track.keys[19].eye[0], 19.0);
        assert_eq!(track.keys[3].eye[0], 3.0);
    }
}
