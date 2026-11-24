//! Sillage des personnages qui marchent dans l'eau du décor (marais d'Hossin, ruisseaux…).
//!
//! Comme en jeu, un soldat qui avance dans l'eau écarte les algues, laisse derrière lui une
//! traînée d'eau brassée et blanchâtre qui retombe en quelques secondes, et fait partir des ronds
//! de ses jambes ; le passage ouvert dans les algues se referme lentement.
//!
//! Tout est calculé à partir de la timeline : la position des chevilles est relue aux instants
//! passés (pistes des os, sans toucher à la pose courante). La lecture, la photo et l'export
//! vidéo donnent donc exactement le même sillage, et on peut se placer n'importe où dans le temps.
//!
//! Par personnage, le shader reçoit :
//! - la **traînée** : le point de contact (milieu des chevilles) à des âges croissants, serrés
//!   sur les deux premières secondes (écume), espacés ensuite (algues, jusqu'à 12 s) ;
//! - les **ronds** : un rond part des jambes toutes les [`RING_PERIOD`] s, à des instants fixes
//!   de la timeline (sinon ils resteraient figés à la même taille d'une image à l'autre), avec
//!   une force qui suit la vitesse du personnage à cet instant.

use crate::scene::Scene;
use glam::{Mat4, Vec2, Vec3};

/// personnages suivis au plus (taille des tableaux du shader)
pub const MAX_CHARS: usize = 6;
/// points au plus, tous personnages confondus (taille du tableau du shader)
pub const MAX_POINTS: usize = 256;
/// intervalle entre deux ronds (s), à peu près le rythme des pas
pub const RING_PERIOD: f32 = 0.35;
/// ronds suivis par personnage (au-delà, ils sont éteints)
const RINGS: usize = 8;
/// profondeur au-delà de laquelle le personnage n'est plus « dans l'eau » (il nagerait)
const MAX_DEPTH: f32 = 1.6;

/// Âges des points de la traînée (s).
fn trail_ages() -> impl Iterator<Item = f32> {
    (0..17).map(|k| k as f32 * 0.125).chain((0..13).map(|k| 2.75 + k as f32 * 0.75))
}

/// Une nappe d'eau du décor (instance d'un maillage d'eau), pour retrouver sa hauteur.
pub struct WaterPlane {
    pub matrix: Mat4,
    pub inverse: Mat4,
    pub lo: Vec3,
    pub hi: Vec3,
}

impl WaterPlane {
    /// Hauteur de la surface au point (x, z) si la nappe le couvre.
    fn level(&self, xz: Vec2, near_y: f32) -> Option<f32> {
        let l = self.inverse.transform_point3(Vec3::new(xz.x, near_y, xz.y));
        if l.x < self.lo.x || l.x > self.hi.x || l.z < self.lo.z || l.z > self.hi.z {
            return None;
        }
        Some(self.matrix.transform_point3(Vec3::new(l.x, (self.lo.y + self.hi.y) * 0.5, l.z)).y)
    }
}

/// Surface d'eau dans laquelle se trouve un pied à la hauteur `foot` : la plus basse qui le
/// recouvre (jusqu'à [`MAX_DEPTH`]) ou qu'il effleure.
fn water_level(planes: &[WaterPlane], xz: Vec2, foot: f32) -> Option<f32> {
    planes
        .iter()
        .filter_map(|p| p.level(xz, foot))
        .filter(|y| (-0.05..=MAX_DEPTH).contains(&(y - foot)))
        .min_by(|a, b| a.total_cmp(b))
}

/// Sillages prêts à envoyer au shader (voir `Globals` dans `render.rs`).
#[derive(Clone, Default)]
pub struct Wakes {
    /// par personnage : centre (x, z), rayon d'effet (m), hauteur de l'eau
    pub heads: Vec<[f32; 4]>,
    /// par personnage : premier point, points de la traînée, ronds, vitesse actuelle (m/s)
    pub ranges: Vec<[f32; 4]>,
    /// traînée : x, z, âge (s), présence dans l'eau (0 à 1) ; ronds : x, z, âge (s), force
    pub points: Vec<[f32; 4]>,
}

/// Contact d'un personnage avec le sol à une image : milieu des chevilles (x, z) et hauteur de
/// la plante des pieds. Seules les chaînes d'os jusqu'aux chevilles sont évaluées.
struct Feet<'a> {
    actor: &'a crate::scene::Actor,
    chains: [Vec<usize>; 2],
}

impl<'a> Feet<'a> {
    fn new(actor: &'a crate::scene::Actor) -> Option<Self> {
        let bones = &actor.model.skeleton.bones;
        let chain = |name: &str| -> Option<Vec<usize>> {
            let mut b = actor.bone_index(name)?;
            let mut c = vec![b];
            while let Some(p) = bones[b].parent {
                if c.len() > bones.len() {
                    return None;
                }
                c.push(p);
                b = p;
            }
            c.reverse();
            Some(c)
        };
        Some(Self { actor, chains: [chain("L_ANKLE")?, chain("R_ANKLE")?] })
    }

    fn at(&self, frame: f32) -> (Vec2, f32) {
        let a = self.actor;
        let root = a.anim.root.sample(frame).unwrap_or(a.anim.root_static).to_mat4();
        let bones = &a.model.skeleton.bones;
        let ankle = |chain: &[usize]| -> Vec3 {
            let mut m = root;
            for &b in chain {
                let local = a.anim.bones.get(b).and_then(|t| t.sample(frame)).unwrap_or(bones[b].rest_local);
                m *= local.to_mat4();
            }
            m.w_axis.truncate()
        };
        let (l, r) = (ankle(&self.chains[0]), ankle(&self.chains[1]));
        let mid = (l + r) * 0.5;
        // la cheville est à une dizaine de centimètres au-dessus de la plante du pied
        (Vec2::new(mid.x, mid.z), l.y.min(r.y) - 0.1)
    }
}

/// Sillages de tous les personnages (acteurs racines visibles qui ont des chevilles) à une image
/// de la timeline, dans les nappes d'eau données.
pub fn build(scene: &Scene, frame: f32, fps: f32, planes: &[WaterPlane]) -> Wakes {
    let mut out = Wakes::default();
    if planes.is_empty() {
        return out;
    }
    let fps = fps.max(1.0);
    let now = frame / fps;
    for actor in scene.actors.iter().filter(|a| a.visible && a.attachment.is_none()) {
        if out.heads.len() >= MAX_CHARS {
            break;
        }
        let Some(feet) = Feet::new(actor) else { continue };
        // la timeline est en images : instant (s) → image
        let at = |t: f32| feet.at(t * fps);
        let start = out.points.len();
        let mut level: Option<f32> = None;
        let mut trail: Vec<[f32; 4]> = Vec::new();
        for age in trail_ages() {
            let (p, foot) = at(now - age);
            let w = water_level(planes, p, foot);
            // pied qui effleure l'eau : sillage faible
            let s = w.map_or(0.0, |y| smoothstep(0.0, 0.12, y - foot));
            if level.is_none() && s > 0.0 {
                level = w;
            }
            trail.push([p.x, p.y, age, s]);
        }
        let Some(level) = level else { continue };
        // ronds : instants fixes de la timeline (multiples de la période)
        let last = (now / RING_PERIOD).floor() * RING_PERIOD;
        let mut rings: Vec<[f32; 4]> = Vec::new();
        for j in 0..RINGS {
            let t = last - j as f32 * RING_PERIOD;
            let (p, foot) = at(t);
            let (q, _) = at(t - 0.1);
            let speed = (p - q).length() / 0.1;
            let s = water_level(planes, p, foot).map_or(0.0, |y| smoothstep(0.0, 0.12, y - foot));
            // debout dans l'eau : petits ronds des jambes qui se balancent ; en marche : vrais remous
            rings.push([p.x, p.y, now - t, s * (0.25 + 0.75 * smoothstep(0.3, 3.0, speed))]);
        }
        if start + trail.len() + rings.len() > MAX_POINTS {
            break;
        }
        let speed = Vec2::new(trail[0][0] - trail[1][0], trail[0][1] - trail[1][1]).length() / 0.125;
        // zone touchée : les points, plus l'écume élargie et les ronds (≈ 2,5 m)
        let (mut lo, mut hi) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
        for p in trail.iter().chain(&rings) {
            lo = lo.min(Vec2::new(p[0], p[1]));
            hi = hi.max(Vec2::new(p[0], p[1]));
        }
        let center = (lo + hi) * 0.5;
        let radius = (hi - lo).length() * 0.5 + 3.0;
        out.heads.push([center.x, center.y, radius, level]);
        out.ranges.push([start as f32, trail.len() as f32, rings.len() as f32, speed]);
        out.points.extend(trail);
        out.points.extend(rings);
    }
    out
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plane(y: f32) -> WaterPlane {
        let matrix = Mat4::from_translation(Vec3::new(0.0, y, 0.0));
        WaterPlane { matrix, inverse: matrix.inverse(), lo: Vec3::new(-50.0, 0.0, -50.0), hi: Vec3::new(50.0, 0.0, 50.0) }
    }

    #[test]
    fn niveau_de_l_eau() {
        let planes = [plane(0.4), plane(5.0)];
        // pied au fond de l'eau : la nappe à 0,4 m
        assert_eq!(water_level(&planes, Vec2::ZERO, 0.0), Some(0.4));
        // trop profond, ou hors de la nappe
        assert_eq!(water_level(&planes, Vec2::ZERO, -3.0), None);
        assert_eq!(water_level(&planes, Vec2::new(80.0, 0.0), 0.0), None);
        // pied au-dessus de l'eau
        assert_eq!(water_level(&planes, Vec2::ZERO, 1.0), None);
    }

    #[test]
    fn ages_croissants() {
        let a: Vec<f32> = trail_ages().collect();
        assert!(a.windows(2).all(|w| w[1] > w[0]));
        assert_eq!(a[0], 0.0);
        assert!(*a.last().unwrap() > 11.0);
        // tous les personnages tiennent dans le tableau du shader
        assert!(MAX_CHARS * (a.len() + RINGS) <= MAX_POINTS);
    }
}
