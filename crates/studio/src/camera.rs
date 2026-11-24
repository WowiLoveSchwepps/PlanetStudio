//! Caméra de travail (vol libre façon drone, orbite en option) et utilitaires de projection.

use glam::{Mat4, Vec2, Vec3, Vec4Swizzles};
use serde::{Deserialize, Serialize};

/// Réglages du vol libre (« drone » / observateur), enregistrés dans la configuration.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DroneSettings {
    /// vitesse de croisière, en mètres par seconde (Maj = ×4)
    pub speed: f32,
    /// multiplicateur de sensibilité de la souris en visée
    pub sensitivity: f32,
    /// souris inversée en vertical (pousser = regarder en bas)
    pub invert_y: bool,
    /// douceur des accélérations, en secondes (0 = arrêt net)
    pub smoothing: f32,
    /// stabilisation de l'enregistrement en direct (0 = aucune, 1 = forte)
    pub stabilize: f32,
}

impl Default for DroneSettings {
    fn default() -> Self {
        Self { speed: 3.0, sensitivity: 1.0, invert_y: false, smoothing: 0.15, stabilize: 0.5 }
    }
}

impl DroneSettings {
    pub const MIN_SPEED: f32 = 0.1;
    pub const MAX_SPEED: f32 = 200.0;

    /// Radians par unité de déplacement brut de la souris.
    pub fn look_rate(&self) -> f32 {
        0.0025 * self.sensitivity.clamp(0.05, 10.0)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct OrbitCamera {
    pub target: Vec3,
    pub distance: f32,
    /// angle horizontal (radians)
    pub yaw: f32,
    /// angle vertical (radians)
    pub pitch: f32,
    pub fov_deg: f32,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        Self { target: Vec3::new(0.0, 1.0, 0.0), distance: 4.0, yaw: 0.6, pitch: 0.15, fov_deg: 45.0 }
    }
}

impl OrbitCamera {
    /// Direction de la cible vers l'œil.
    fn back_dir(&self) -> Vec3 {
        Vec3::new(self.pitch.cos() * self.yaw.sin(), self.pitch.sin(), self.pitch.cos() * self.yaw.cos())
    }

    pub fn eye(&self) -> Vec3 {
        self.target + self.back_dir() * self.distance
    }

    /// Regard libre (drone) : la caméra tourne sur elle-même, sa position ne bouge pas.
    /// `delta` est en radians (x = lacet, y = tangage ; positif = vers la droite / le bas).
    pub fn look(&mut self, delta: Vec2) {
        let eye = self.eye();
        self.yaw -= delta.x;
        self.pitch = (self.pitch + delta.y).clamp(-1.55, 1.55);
        self.target = eye - self.back_dir() * self.distance;
    }

    pub fn orbit(&mut self, delta: Vec2) {
        self.yaw -= delta.x * 0.008;
        self.pitch = (self.pitch + delta.y * 0.008).clamp(-1.55, 1.55);
    }

    pub fn pan(&mut self, delta: Vec2, viewport_h: f32) {
        let view = self.view();
        let right = view.row(0).xyz();
        let up = view.row(1).xyz();
        let scale = self.distance * (self.fov_deg.to_radians() * 0.5).tan() * 2.0 / viewport_h.max(1.0);
        self.target += (-right * delta.x + up * delta.y) * scale;
    }

    pub fn view(&self) -> Mat4 {
        Mat4::look_at_rh(self.eye(), self.target, Vec3::Y)
    }

    /// Place la caméra à partir d'un couple position/cible.
    pub fn set_from(&mut self, eye: Vec3, target: Vec3, fov: f32) {
        let d = eye - target;
        self.target = target;
        self.distance = d.length().max(0.05);
        let n = d / self.distance;
        self.pitch = n.y.clamp(-1.0, 1.0).asin();
        self.yaw = n.x.atan2(n.z);
        self.fov_deg = fov;
    }
}

/// Vue + projection d'une image.
#[derive(Clone, Copy, Debug)]
pub struct ViewParams {
    pub eye: Vec3,
    pub target: Vec3,
    pub fov_deg: f32,
}

impl ViewParams {
    pub fn view(&self) -> Mat4 {
        Mat4::look_at_rh(self.eye, self.target, Vec3::Y)
    }
    /// Projection en profondeur inversée (« reversed-Z ») : 1 au plan proche, 0 au loin. Avec un
    /// tampon de profondeur en flottant, la précision reste presque constante avec la distance
    /// (quelques centièmes de millimètre à 100 m). En profondeur classique, plan proche à 2 cm,
    /// elle n'était plus que de 7 mm à 50 m et 3 cm à 100 m : la plaque lumineuse d'une lampe,
    /// posée à quelques millimètres de son boîtier, passait devant ou derrière selon l'image
    /// (les lampes des arches clignotaient pendant un travelling).
    pub fn proj(&self, aspect: f32) -> Mat4 {
        Mat4::perspective_rh(self.fov_deg.to_radians(), aspect.max(0.01), FAR, NEAR)
    }
    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        self.proj(aspect) * self.view()
    }
}

/// Projette un point monde en coordonnées écran (pixels, relatif au viewport).
pub fn project(vp: &Mat4, p: Vec3, size: Vec2) -> Option<Vec2> {
    let c = *vp * p.extend(1.0);
    if c.w <= 1e-4 {
        return None;
    }
    let ndc = c.xyz() / c.w;
    Some(Vec2::new((ndc.x * 0.5 + 0.5) * size.x, (1.0 - (ndc.y * 0.5 + 0.5)) * size.y))
}

/// Plans proche et lointain de la caméra (m).
pub const NEAR: f32 = 0.02;
pub const FAR: f32 = 5000.0;

/// Rayon (origine, direction) passant par un pixel du viewport (profondeur inversée : 1 au plan
/// proche, 0 au loin).
pub fn ray(vp: &Mat4, px: Vec2, size: Vec2) -> (Vec3, Vec3) {
    let inv = vp.inverse();
    let ndc = Vec2::new(px.x / size.x * 2.0 - 1.0, 1.0 - px.y / size.y * 2.0);
    let a = inv * ndc.extend(1.0).extend(1.0);
    let b = inv * ndc.extend(0.0).extend(1.0);
    let a = a.xyz() / a.w;
    let b = b.xyz() / b.w;
    (a, (b - a).normalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn look_turns_in_place() {
        let mut c = OrbitCamera::default();
        let eye = c.eye();
        let fwd0 = (c.target - eye).normalize();
        let right0 = fwd0.cross(Vec3::Y).normalize();
        // souris vers la droite : le regard part à droite, l'œil ne bouge pas
        c.look(Vec2::new(0.2, 0.0));
        assert!(c.eye().distance(eye) < 1e-4);
        assert!((c.target - eye).normalize().dot(right0) > 0.1);
        // souris vers le bas : le regard descend
        let y0 = (c.target - eye).normalize().y;
        c.look(Vec2::new(0.0, 0.2));
        assert!(c.eye().distance(eye) < 1e-4);
        assert!((c.target - eye).normalize().y < y0);
    }
}
