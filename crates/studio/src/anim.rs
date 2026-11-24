//! Pistes d'animation à images clés.

use forgelight::Transform;
use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Interp {
    /// interpolation lissée (ease in/out)
    #[default]
    Smooth,
    Linear,
    /// valeur constante jusqu'à la clé suivante
    Step,
}

impl Interp {
    pub const ALL: [Interp; 3] = [Interp::Smooth, Interp::Linear, Interp::Step];
    pub fn label(self) -> &'static str {
        match self {
            Interp::Smooth => tr!("Lissée", "Smooth"),
            Interp::Linear => tr!("Linéaire", "Linear"),
            Interp::Step => tr!("Palier", "Step"),
        }
    }
    fn weight(self, t: f32) -> f32 {
        match self {
            Interp::Smooth => t * t * (3.0 - 2.0 * t),
            Interp::Linear => t,
            Interp::Step => 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Key {
    pub frame: i32,
    pub t: [f32; 3],
    pub r: [f32; 4],
    pub s: [f32; 3],
    #[serde(default)]
    pub interp: Interp,
}

impl Key {
    pub fn new(frame: i32, tr: &Transform, interp: Interp) -> Self {
        Self {
            frame,
            t: tr.translation.to_array(),
            r: tr.rotation.to_array(),
            s: tr.scale.to_array(),
            interp,
        }
    }
    pub fn transform(&self) -> Transform {
        Transform {
            translation: Vec3::from(self.t),
            rotation: Quat::from_array(self.r).normalize(),
            scale: Vec3::from(self.s),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub keys: Vec<Key>,
}

impl Track {
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn set_key(&mut self, key: Key) {
        match self.keys.binary_search_by_key(&key.frame, |k| k.frame) {
            Ok(i) => {
                let interp = self.keys[i].interp;
                self.keys[i] = Key { interp, ..key };
            }
            Err(i) => self.keys.insert(i, key),
        }
    }

    pub fn remove_key(&mut self, frame: i32) -> bool {
        if let Ok(i) = self.keys.binary_search_by_key(&frame, |k| k.frame) {
            self.keys.remove(i);
            true
        } else {
            false
        }
    }

    pub fn key_at(&self, frame: i32) -> Option<&Key> {
        self.keys.binary_search_by_key(&frame, |k| k.frame).ok().map(|i| &self.keys[i])
    }

    /// Valeur interpolée à une image (fractionnaire).
    pub fn sample(&self, frame: f32) -> Option<Transform> {
        let first = self.keys.first()?;
        if frame <= first.frame as f32 {
            return Some(first.transform());
        }
        let last = self.keys.last().unwrap();
        if frame >= last.frame as f32 {
            return Some(last.transform());
        }
        let i = self.keys.partition_point(|k| (k.frame as f32) <= frame);
        let (a, b) = (&self.keys[i - 1], &self.keys[i]);
        let t = (frame - a.frame as f32) / (b.frame - a.frame) as f32;
        if a.interp == Interp::Smooth {
            // spline qui traverse les clés sans s'y arrêter (comme la caméra)
            let (k, at) = (&self.keys, |k: &Key| (k.frame, k.interp));
            return Some(Transform {
                translation: spline_at(k, i, t, at, |k| Vec3::from(k.t), same),
                rotation: {
                    // rotations en vecteurs de rotation autour de la clé de départ, chaînés de
                    // clé en clé : vitesse angulaire exacte même entre clés éloignées
                    let q = |k: &Key| Quat::from_array(k.r).normalize();
                    let rel = |from: Quat, to: Quat| {
                        let d = from.inverse() * to;
                        if d.w < 0.0 { -d } else { d }.to_scaled_axis()
                    };
                    let (ka, kb) = (&self.keys[i - 1], &self.keys[i]);
                    let (qa, qb) = (q(ka), q(kb));
                    let vb = rel(qa, qb);
                    let prev = (i >= 2).then(|| {
                        let kp = &self.keys[i - 2];
                        (kp.frame, kp.interp, rel(qa, q(kp)))
                    });
                    let next = self.keys.get(i + 1).map(|kn| (kn.frame, kn.interp, vb + rel(qb, q(kn))));
                    let v = spline_pts(prev, (ka.frame, ka.interp, Vec3::ZERO), (kb.frame, kb.interp, vb), next, t);
                    (qa * Quat::from_scaled_axis(v)).normalize()
                },
                scale: spline_at(k, i, t, at, |k| Vec3::from(k.s), same),
            });
        }
        let mut ta = a.transform();
        let tb = b.transform();
        // chemin le plus court pour les quaternions
        if ta.rotation.dot(tb.rotation) < 0.0 {
            ta.rotation = -ta.rotation;
        }
        Some(ta.lerp(&tb, a.interp.weight(t)))
    }
}

/// Clé de caméra (position, cible, champ de vision).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CamKey {
    pub frame: i32,
    pub eye: [f32; 3],
    pub target: [f32; 3],
    pub fov_deg: f32,
    #[serde(default)]
    pub interp: Interp,
}

/// Position fixe de la caméra de prise quand elle n'a pas (ou plus) de clés.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CamPose {
    pub eye: [f32; 3],
    pub target: [f32; 3],
    pub fov_deg: f32,
}

impl Default for CamPose {
    fn default() -> Self {
        // trois-quarts face, de l'autre côté que la caméra de travail par défaut
        Self { eye: [-1.6, 1.45, 3.4], target: [0.0, 1.0, 0.0], fov_deg: 40.0 }
    }
}

/// Format de prise de vue par défaut (absent des anciens projets) : Full HD 16:9.
pub const DEFAULT_SHOT_FORMAT: [u32; 2] = [1920, 1080];

fn default_shot_format() -> [u32; 2] {
    DEFAULT_SHOT_FORMAT
}

/// Caméra de prise : une pose fixe, ou une animation par clés si elle en a.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CamTrack {
    pub keys: Vec<CamKey>,
    #[serde(default)]
    pub base: CamPose,
    /// format de prise de vue (largeur × hauteur en pixels) : cadre de la vue « Prise »,
    /// taille de référence des exports
    #[serde(default = "default_shot_format")]
    pub format: [u32; 2],
}

impl Default for CamTrack {
    fn default() -> Self {
        Self { keys: Vec::new(), base: CamPose::default(), format: DEFAULT_SHOT_FORMAT }
    }
}

impl CamTrack {
    pub fn set_key(&mut self, key: CamKey) {
        match self.keys.binary_search_by_key(&key.frame, |k| k.frame) {
            Ok(i) => {
                let interp = self.keys[i].interp;
                self.keys[i] = CamKey { interp, ..key };
            }
            Err(i) => self.keys.insert(i, key),
        }
    }
    pub fn remove_key(&mut self, frame: i32) -> bool {
        if let Ok(i) = self.keys.binary_search_by_key(&frame, |k| k.frame) {
            let k = self.keys.remove(i);
            if self.keys.is_empty() {
                // la caméra reste là où était sa dernière clé
                self.base = CamPose { eye: k.eye, target: k.target, fov_deg: k.fov_deg };
            }
            true
        } else {
            false
        }
    }
    /// Position, cible et champ de la caméra de prise à une image (clés, sinon pose fixe).
    pub fn pose(&self, frame: f32) -> (Vec3, Vec3, f32) {
        self.sample(frame)
            .unwrap_or((Vec3::from(self.base.eye), Vec3::from(self.base.target), self.base.fov_deg))
    }

    /// Déplace la caméra : clé à `frame` si elle est animée, sinon nouvelle pose fixe.
    pub fn set_pose(&mut self, frame: i32, eye: Vec3, target: Vec3, fov_deg: f32, interp: Interp) {
        if self.keys.is_empty() {
            self.base = CamPose { eye: eye.to_array(), target: target.to_array(), fov_deg };
        } else {
            self.set_key(CamKey { frame, eye: eye.to_array(), target: target.to_array(), fov_deg, interp });
        }
    }

    /// Pose à une image. En « Lissée », la caméra suit une spline qui traverse
    /// les clés sans s'y arrêter : elle ne freine qu'à la première et à la
    /// dernière clé, avant un palier ou sur une clé qui tient la même position.
    pub fn sample(&self, frame: f32) -> Option<(Vec3, Vec3, f32)> {
        let first = self.keys.first()?;
        let v = |k: &CamKey| (Vec3::from(k.eye), Vec3::from(k.target), k.fov_deg);
        if frame <= first.frame as f32 {
            return Some(v(first));
        }
        let last = self.keys.last().unwrap();
        if frame >= last.frame as f32 {
            return Some(v(last));
        }
        let i = self.keys.partition_point(|k| (k.frame as f32) <= frame);
        let (a, b) = (&self.keys[i - 1], &self.keys[i]);
        let s = (frame - a.frame as f32) / (b.frame - a.frame) as f32;
        if a.interp != Interp::Smooth {
            let t = a.interp.weight(s);
            let ((ea, ta, fa), (eb, tb, fb)) = (v(a), v(b));
            return Some((ea.lerp(eb, t), ta.lerp(tb, t), fa + (fb - fa) * t));
        }
        let (k, at) = (&self.keys, |k: &CamKey| (k.frame, k.interp));
        Some((
            spline_at(k, i, s, at, |k| Vec3::from(k.eye), same),
            spline_at(k, i, s, at, |k| Vec3::from(k.target), same),
            spline_at(k, i, s, at, |k| k.fov_deg, same).max(1.0),
        ))
    }
}

/// Valeur interpolable par spline (position, rotation, angle, couleur…).
trait SplineValue: Copy + std::ops::Add<Output = Self> + std::ops::Sub<Output = Self> + std::ops::Mul<f32, Output = Self> {
    fn zero() -> Self;
    fn magnitude(self) -> f32;
}

impl SplineValue for f32 {
    fn zero() -> Self {
        0.0
    }
    fn magnitude(self) -> f32 {
        self.abs()
    }
}

impl SplineValue for Vec3 {
    fn zero() -> Self {
        Vec3::ZERO
    }
    fn magnitude(self) -> f32 {
        self.length()
    }
}

/// Pas d'alignement : la valeur telle quelle.
fn same<T>(v: T, _reference: T) -> T {
    v
}

/// Angle (degrés) déroulé à moins d'un demi-tour de la référence.
fn angle_align(a: f32, reference: f32) -> f32 {
    reference + (a - reference + 540.0).rem_euclid(360.0) - 180.0
}

/// Angle ramené dans -180..180.
fn wrap_deg(a: f32) -> f32 {
    (a + 540.0).rem_euclid(360.0) - 180.0
}

/// Point de contrôle : image, interpolation du segment qui part de la clé, valeur.
type Pt<T> = (i32, Interp, T);

/// Spline lissée sur le segment `keys[i-1] → keys[i]` à la fraction `s`. Les valeurs
/// voisines sont alignées sur leur voisine (quaternions, angles) avant le calcul.
fn spline_at<K, T: SplineValue>(
    keys: &[K],
    i: usize,
    s: f32,
    at: impl Fn(&K) -> (i32, Interp),
    get: impl Fn(&K) -> T,
    align: impl Fn(T, T) -> T,
) -> T {
    let pt = |k: &K, v: T| {
        let (f, it) = at(k);
        (f, it, v)
    };
    let (ka, kb) = (&keys[i - 1], &keys[i]);
    let a = pt(ka, get(ka));
    let b = pt(kb, align(get(kb), a.2));
    let prev = (i >= 2).then(|| pt(&keys[i - 2], align(get(&keys[i - 2]), a.2)));
    let next = keys.get(i + 1).map(|k| pt(k, align(get(k), b.2)));
    spline_pts(prev, a, b, next, s)
}

/// Spline lissée entre `a` et `b` à la fraction `s`, avec leurs voisines (déjà alignées).
fn spline_pts<T: SplineValue>(prev: Option<Pt<T>>, a: Pt<T>, b: Pt<T>, next: Option<Pt<T>>, s: f32) -> T {
    let m0 = tangent(prev, a, Some(b));
    let m1 = tangent(Some(a), b, next);
    hermite(a.2, b.2, m0, m1, (b.0 - a.0) as f32, s)
}

/// Hermite cubique entre `p0` et `p1`, vitesses `m0`/`m1` en unités par image.
fn hermite<T: SplineValue>(p0: T, p1: T, m0: T, m1: T, dt: f32, s: f32) -> T {
    let (s2, s3) = (s * s, s * s * s);
    p0 * (2.0 * s3 - 3.0 * s2 + 1.0)
        + m0 * ((s3 - 2.0 * s2 + s) * dt)
        + p1 * (3.0 * s2 - 2.0 * s3)
        + m1 * ((s3 - s2) * dt)
}

/// Vitesse au passage de la clé `b` (unités par image), entre ses voisines `a` et `c`.
fn tangent<T: SplineValue>(a: Option<Pt<T>>, b: Pt<T>, c: Option<Pt<T>>) -> T {
    // départ et arrivée en douceur
    let (Some(a), Some(c)) = (a, c) else {
        return T::zero();
    };
    // palier avant ou après : immobile sur cette clé
    if a.1 == Interp::Step || b.1 == Interp::Step {
        return T::zero();
    }
    // clé qui tient la valeur (deux clés identiques) : pas de dépassement
    if (b.2 - a.2).magnitude() < 1e-4 || (c.2 - b.2).magnitude() < 1e-4 {
        return T::zero();
    }
    let (dt0, dt1) = ((b.0 - a.0) as f32, (c.0 - b.0) as f32);
    let (d0, d1) = ((b.2 - a.2) * (1.0 / dt0), (c.2 - b.2) * (1.0 / dt1));
    // raccord sans à-coup avec un segment linéaire voisin
    if a.1 == Interp::Linear {
        return d0;
    }
    if b.1 == Interp::Linear {
        return d1;
    }
    // moyenne des pentes pondérée par la durée des segments (clés inégalement espacées)
    (d0 * dt1 + d1 * dt0) * (1.0 / (dt0 + dt1))
}

/// Clé d'une piste de réglages (soleil, ambiance).
pub trait LightKey: Copy {
    type State: Copy;
    fn frame(&self) -> i32;
    fn with_frame(self, frame: i32) -> Self;
    fn interp(&self) -> Interp;
    fn with_interp(self, interp: Interp) -> Self;
    fn state(&self) -> Self::State;
    /// État entre deux clés, `t` déjà pondéré par l'interpolation.
    fn blend(a: &Self, b: &Self, t: f32) -> Self::State;
    /// État lissé sur le segment `keys[i-1] → keys[i]` (spline sans arrêt aux clés).
    fn spline(keys: &[Self], i: usize, s: f32) -> Self::State;
}

/// Piste de clés de réglages : sans clé, ce sont les réglages fixes qui s'appliquent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KeyTrack<K> {
    pub keys: Vec<K>,
}

impl<K> Default for KeyTrack<K> {
    fn default() -> Self {
        Self { keys: Vec::new() }
    }
}

impl<K: LightKey> KeyTrack<K> {
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    fn index(&self, frame: i32) -> Result<usize, usize> {
        self.keys.binary_search_by_key(&frame, |k| k.frame())
    }

    /// Pose une clé ; sur une clé existante, garde son interpolation.
    pub fn set_key(&mut self, key: K) {
        match self.index(key.frame()) {
            Ok(i) => {
                let interp = self.keys[i].interp();
                self.keys[i] = key.with_interp(interp);
            }
            Err(i) => self.keys.insert(i, key),
        }
    }

    pub fn remove_key(&mut self, frame: i32) -> Option<K> {
        let i = self.index(frame).ok()?;
        Some(self.keys.remove(i))
    }

    pub fn key_at(&self, frame: i32) -> Option<&K> {
        self.index(frame).ok().map(|i| &self.keys[i])
    }

    /// Déplace une clé, sans écraser une clé existante.
    pub fn move_key(&mut self, from: i32, to: i32) -> bool {
        if from == to || self.key_at(to).is_some() {
            return false;
        }
        match self.remove_key(from) {
            Some(k) => {
                self.set_key(k.with_frame(to));
                true
            }
            None => false,
        }
    }

    /// État interpolé à une image (fractionnaire).
    pub fn sample(&self, frame: f32) -> Option<K::State> {
        let first = self.keys.first()?;
        if frame <= first.frame() as f32 {
            return Some(first.state());
        }
        let last = self.keys.last().unwrap();
        if frame >= last.frame() as f32 {
            return Some(last.state());
        }
        let i = self.keys.partition_point(|k| (k.frame() as f32) <= frame);
        let (a, b) = (&self.keys[i - 1], &self.keys[i]);
        let s = (frame - a.frame() as f32) / (b.frame() - a.frame()) as f32;
        if a.interp() == Interp::Smooth {
            return Some(K::spline(&self.keys, i, s));
        }
        Some(K::blend(a, b, a.interp().weight(s)))
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Angle en degrés interpolé par le plus court chemin, ramené dans -180..180.
fn lerp_angle(a: f32, b: f32, t: f32) -> f32 {
    let d = (b - a + 540.0).rem_euclid(360.0) - 180.0;
    (a + d * t + 540.0).rem_euclid(360.0) - 180.0
}

/// État du soleil : azimut et hauteur (degrés), couleur (RVB 0..1), intensité.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SunState {
    pub azimuth: f32,
    pub elevation: f32,
    pub color: [f32; 3],
    pub intensity: f32,
}

/// Clé de soleil.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SunKey {
    pub frame: i32,
    pub azimuth: f32,
    pub elevation: f32,
    pub color: [f32; 3],
    pub intensity: f32,
    #[serde(default)]
    pub interp: Interp,
}

impl SunKey {
    pub fn new(frame: i32, s: SunState, interp: Interp) -> Self {
        Self { frame, azimuth: s.azimuth, elevation: s.elevation, color: s.color, intensity: s.intensity, interp }
    }
}

impl LightKey for SunKey {
    type State = SunState;
    fn frame(&self) -> i32 {
        self.frame
    }
    fn with_frame(self, frame: i32) -> Self {
        Self { frame, ..self }
    }
    fn interp(&self) -> Interp {
        self.interp
    }
    fn with_interp(self, interp: Interp) -> Self {
        Self { interp, ..self }
    }
    fn state(&self) -> SunState {
        SunState { azimuth: self.azimuth, elevation: self.elevation, color: self.color, intensity: self.intensity }
    }
    /// L'azimut tourne par le plus court chemin.
    fn blend(a: &Self, b: &Self, t: f32) -> SunState {
        SunState {
            azimuth: lerp_angle(a.azimuth, b.azimuth, t),
            elevation: lerp(a.elevation, b.elevation, t),
            color: [0, 1, 2].map(|i| lerp(a.color[i], b.color[i], t)),
            intensity: lerp(a.intensity, b.intensity, t),
        }
    }
    fn spline(keys: &[Self], i: usize, s: f32) -> SunState {
        let at = |k: &Self| (k.frame, k.interp);
        SunState {
            azimuth: wrap_deg(spline_at(keys, i, s, at, |k| k.azimuth, angle_align)),
            elevation: spline_at(keys, i, s, at, |k| k.elevation, same).clamp(-90.0, 90.0),
            color: spline_at(keys, i, s, at, |k| Vec3::from(k.color), same).max(Vec3::ZERO).to_array(),
            intensity: spline_at(keys, i, s, at, |k| k.intensity, same).max(0.0),
        }
    }
}

/// Soleil animé.
pub type SunTrack = KeyTrack<SunKey>;

/// Couleur de fond d'origine du studio (linéaire).
pub const DEFAULT_BACKGROUND: [f32; 3] = [0.11, 0.12, 0.14];

fn default_background() -> [f32; 3] {
    DEFAULT_BACKGROUND
}

/// État de l'ambiance animable : rotation du panorama (degrés), intensité, couleur de fond.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AmbState {
    pub rotation: f32,
    pub intensity: f32,
    pub background: [f32; 3],
}

/// Clé d'ambiance.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AmbKey {
    pub frame: i32,
    pub rotation: f32,
    pub intensity: f32,
    /// absent des clés antérieures au fond animé : fond d'origine
    #[serde(default = "default_background")]
    pub background: [f32; 3],
    #[serde(default)]
    pub interp: Interp,
}

impl AmbKey {
    pub fn new(frame: i32, s: AmbState, interp: Interp) -> Self {
        Self { frame, rotation: s.rotation, intensity: s.intensity, background: s.background, interp }
    }
}

impl LightKey for AmbKey {
    type State = AmbState;
    fn frame(&self) -> i32 {
        self.frame
    }
    fn with_frame(self, frame: i32) -> Self {
        Self { frame, ..self }
    }
    fn interp(&self) -> Interp {
        self.interp
    }
    fn with_interp(self, interp: Interp) -> Self {
        Self { interp, ..self }
    }
    fn state(&self) -> AmbState {
        AmbState { rotation: self.rotation, intensity: self.intensity, background: self.background }
    }
    /// La rotation tourne par le plus court chemin.
    fn blend(a: &Self, b: &Self, t: f32) -> AmbState {
        AmbState {
            rotation: lerp_angle(a.rotation, b.rotation, t),
            intensity: lerp(a.intensity, b.intensity, t),
            background: [0, 1, 2].map(|i| lerp(a.background[i], b.background[i], t)),
        }
    }
    fn spline(keys: &[Self], i: usize, s: f32) -> AmbState {
        let at = |k: &Self| (k.frame, k.interp);
        AmbState {
            rotation: wrap_deg(spline_at(keys, i, s, at, |k| k.rotation, angle_align)),
            intensity: spline_at(keys, i, s, at, |k| k.intensity, same).max(0.0),
            background: spline_at(keys, i, s, at, |k| Vec3::from(k.background), same).max(Vec3::ZERO).to_array(),
        }
    }
}

/// Ambiance animée (le choix du panorama, lui, reste fixe).
pub type AmbTrack = KeyTrack<AmbKey>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sun_track() {
        let s = |az: f32, el: f32| SunState { azimuth: az, elevation: el, color: [1.0, 1.0, 1.0], intensity: 1.0 };
        let mut tr = SunTrack::default();
        assert!(tr.sample(3.0).is_none());
        tr.set_key(SunKey::new(0, s(170.0, 60.0), Interp::Linear));
        tr.set_key(SunKey::new(10, s(-170.0, 10.0), Interp::Linear));
        let m = tr.sample(5.0).unwrap();
        // 170° → -170° passe par 180°, pas par 0°
        assert!((m.azimuth.abs() - 180.0).abs() < 1e-3, "{}", m.azimuth);
        assert!((m.elevation - 35.0).abs() < 1e-4);
        assert_eq!(tr.sample(-5.0).unwrap().elevation, 60.0);
        assert_eq!(tr.sample(50.0).unwrap().elevation, 10.0);
        // déplacement sans écrasement, suppression
        assert!(!tr.move_key(0, 10));
        assert!(tr.move_key(0, 4));
        assert_eq!(tr.keys[0].frame, 4);
        assert_eq!(tr.remove_key(4).unwrap().elevation, 60.0);
        assert_eq!(tr.keys.len(), 1);
        // sérialisation
        let back: SunTrack = serde_json::from_str(&serde_json::to_string(&tr).unwrap()).unwrap();
        assert_eq!(back, tr);
    }

    #[test]
    fn amb_track() {
        let st = |rotation: f32, intensity: f32, b: f32| AmbState { rotation, intensity, background: [b; 3] };
        let mut tr = AmbTrack::default();
        tr.set_key(AmbKey::new(0, st(-150.0, 1.0, 0.2), Interp::Linear));
        tr.set_key(AmbKey::new(20, st(150.0, 0.2, 0.0), Interp::Smooth));
        let m = tr.sample(10.0).unwrap();
        assert!((m.rotation.abs() - 180.0).abs() < 1e-3, "{}", m.rotation);
        assert!((m.intensity - 0.6).abs() < 1e-4);
        assert!((m.background[0] - 0.1).abs() < 1e-4);
        // une clé enregistrée avant le fond animé prend le fond d'origine
        let old: AmbKey = serde_json::from_str(r#"{"frame":3,"rotation":0.0,"intensity":1.0}"#).unwrap();
        assert_eq!(old.background, DEFAULT_BACKGROUND);
        // l'interpolation d'une clé existante est gardée quand on la réécrit
        tr.set_key(AmbKey::new(0, st(0.0, 0.5, 0.2), Interp::Step));
        assert_eq!(tr.keys[0].interp, Interp::Linear);
        assert_eq!(tr.keys[0].intensity, 0.5);
    }
    #[test]
    fn camera_flows_through_keys() {
        let key = |frame: i32, x: f32, interp: Interp| CamKey {
            frame,
            eye: [x, 1.0, 0.0],
            target: [0.0; 3],
            fov_deg: 40.0,
            interp,
        };
        let mut cam = CamTrack::default();
        cam.set_key(key(0, 0.0, Interp::Smooth));
        cam.set_key(key(10, 10.0, Interp::Smooth));
        cam.set_key(key(20, 20.0, Interp::Smooth));
        let x = |cam: &CamTrack, f: f32| cam.sample(f).unwrap().0.x;
        // passe exactement par les clés
        assert!((x(&cam, 10.0) - 10.0).abs() < 1e-4);
        // ne freine pas sur la clé du milieu : vitesse ≈ 1 unité/image
        let v_mid = (x(&cam, 10.1) - x(&cam, 9.9)) / 0.2;
        assert!((v_mid - 1.0).abs() < 0.05, "{v_mid}");
        // mais démarre et s'arrête en douceur aux extrémités
        assert!((x(&cam, 0.1) - x(&cam, 0.0)) / 0.1 < 0.1);
        assert!((x(&cam, 20.0) - x(&cam, 19.9)) / 0.1 < 0.1);
        // une clé qui tient la position ne fait pas dépasser la caméra
        cam.set_key(key(30, 20.0, Interp::Smooth));
        for f in 20..=30 {
            assert!(x(&cam, f as f32) <= 20.0 + 1e-4);
        }
        // le linéaire reste linéaire
        cam.keys.iter_mut().for_each(|k| k.interp = Interp::Linear);
        assert!((x(&cam, 5.0) - 5.0).abs() < 1e-4);
    }

    #[test]
    fn bones_and_sun_flow_through_keys() {
        // os : translation et rotation ne s'arrêtent pas sur la clé du milieu
        let mut tr = Track::default();
        for (f, x) in [(0, 0.0), (10, 10.0), (20, 20.0)] {
            let t = Transform {
                translation: Vec3::new(x, 0.0, 0.0),
                rotation: Quat::from_rotation_y((x * 9.0_f32).to_radians()),
                scale: Vec3::ONE,
            };
            tr.set_key(Key::new(f, &t, Interp::Smooth));
        }
        let x = |f: f32| tr.sample(f).unwrap().translation.x;
        let ang = |f: f32| {
            let q = tr.sample(f).unwrap().rotation;
            2.0 * q.y.atan2(q.w).to_degrees()
        };
        assert!(((x(10.1) - x(9.9)) / 0.2 - 1.0).abs() < 0.05);
        assert!(((ang(10.1) - ang(9.9)) / 0.2 - 9.0).abs() < 0.5, "{} {} {}", ang(9.9), ang(10.0), ang(10.1));
        assert!((tr.sample(13.0).unwrap().rotation.length() - 1.0).abs() < 1e-4);
        // soleil : l'azimut traverse ±180° par le plus court chemin, sans arrêt au milieu
        let s = |az: f32| SunState { azimuth: az, elevation: 30.0, color: [1.0; 3], intensity: 1.0 };
        let mut sun = SunTrack::default();
        sun.set_key(SunKey::new(0, s(160.0), Interp::Smooth));
        sun.set_key(SunKey::new(10, s(180.0), Interp::Smooth));
        sun.set_key(SunKey::new(20, s(-160.0), Interp::Smooth));
        let az = |f: f32| sun.sample(f).unwrap().azimuth;
        assert!((az(10.0).abs() - 180.0).abs() < 1e-3);
        let step = (az(10.1) + 360.0 - az(9.9)).rem_euclid(360.0) / 0.2;
        assert!((step - 2.0).abs() < 0.1, "{step}");
    }

    #[test]
    fn sample_between_keys() {
        let mut tr = Track::default();
        let a = Transform::IDENTITY;
        let mut b = Transform::IDENTITY;
        b.translation = Vec3::new(10.0, 0.0, 0.0);
        tr.set_key(Key::new(0, &a, Interp::Linear));
        tr.set_key(Key::new(10, &b, Interp::Linear));
        let m = tr.sample(5.0).unwrap();
        assert!((m.translation.x - 5.0).abs() < 1e-4);
        assert_eq!(tr.sample(-3.0).unwrap().translation.x, 0.0);
        assert_eq!(tr.sample(30.0).unwrap().translation.x, 10.0);
    }
}
