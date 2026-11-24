//! Scène : acteurs (modèles posés/animés), caméra de plan, sérialisation du projet.

use crate::anim::{AmbTrack, CamTrack, Interp, Key, SunTrack, Track};
use forgelight::{Model, Transform};
use glam::{Mat4, Quat, Vec3};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// Lien d'un acteur à un autre.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Attachment {
    /// les os de même nom suivent ceux du parent (tête, casque, armure...)
    BoneMerge { parent: u64 },
    /// l'acteur entier suit un os du parent (arme dans la main...)
    Bone { parent: u64, bone: String },
}

impl Attachment {
    pub fn parent(&self) -> u64 {
        match self {
            Attachment::BoneMerge { parent } | Attachment::Bone { parent, .. } => *parent,
        }
    }
}

/// Données animables d'un acteur (ce qui est annulable).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ActorAnim {
    pub root: Track,
    pub root_static: Transform,
    pub bones: Vec<Track>,
}

#[derive(Clone)]
pub struct Actor {
    pub id: u64,
    pub name: String,
    pub asset: String,
    pub model: Arc<Model>,
    pub visible: bool,
    pub attachment: Option<Attachment>,
    pub anim: ActorAnim,
    pub camo: crate::camo::Camo,
    pub decal: crate::decal::Decal,
    /// pose évaluée (locale) à l'image courante
    pub pose: Vec<Transform>,
    pub root_pose: Transform,
    /// matrices monde évaluées
    pub world: Vec<Mat4>,
    pub root_world: Mat4,
}

impl Actor {
    pub fn new(id: u64, asset: &str, model: Arc<Model>) -> Self {
        let n = model.skeleton.bones.len();
        let name = asset.rsplit_once('.').map(|(a, _)| a).unwrap_or(asset).to_string();
        Self {
            id,
            name,
            asset: asset.to_string(),
            pose: model.skeleton.rest_pose(),
            model,
            visible: true,
            attachment: None,
            camo: Default::default(),
            decal: Default::default(),
            anim: ActorAnim { root: Track::default(), root_static: Transform::IDENTITY, bones: vec![Track::default(); n] },
            root_pose: Transform::IDENTITY,
            world: vec![Mat4::IDENTITY; n],
            root_world: Mat4::IDENTITY,
        }
    }

    pub fn bone_index(&self, name: &str) -> Option<usize> {
        self.model.skeleton.bones.iter().position(|b| b.name.eq_ignore_ascii_case(name))
    }

    /// Évalue la pose locale à une image.
    pub fn evaluate_local(&mut self, frame: f32) {
        self.root_pose = self.anim.root.sample(frame).unwrap_or(self.anim.root_static);
        for (i, b) in self.model.skeleton.bones.iter().enumerate() {
            self.pose[i] = self.anim.bones.get(i).and_then(|t| t.sample(frame)).unwrap_or(b.rest_local);
        }
    }

    /// Écrit une transformation locale d'os comme clé à l'image donnée.
    pub fn key_bone(&mut self, bone: usize, frame: i32, tr: Transform, interp: Interp) {
        if bone < self.anim.bones.len() {
            self.pose[bone] = tr;
            self.anim.bones[bone].set_key(Key::new(frame, &tr, interp));
        }
    }

    /// Écrit la transformation racine (clé si la piste est animée ou si `force_key`).
    pub fn key_root(&mut self, frame: i32, tr: Transform, interp: Interp, force_key: bool) {
        self.root_pose = tr;
        if force_key || !self.anim.root.is_empty() {
            self.anim.root.set_key(Key::new(frame, &tr, interp));
        } else {
            self.anim.root_static = tr;
        }
    }

    pub fn bounds_world(&self) -> (Vec3, Vec3) {
        let (mn, mx) = (self.model.bounds_min, self.model.bounds_max);
        let mut lo = Vec3::splat(f32::MAX);
        let mut hi = Vec3::splat(f32::MIN);
        for i in 0..8 {
            let c = Vec3::new(
                if i & 1 == 0 { mn.x } else { mx.x },
                if i & 2 == 0 { mn.y } else { mx.y },
                if i & 4 == 0 { mn.z } else { mx.z },
            );
            let p = self.root_world.transform_point3(c);
            lo = lo.min(p);
            hi = hi.max(p);
        }
        (lo, hi)
    }

    /// Matrices de skinning à envoyer au GPU.
    pub fn skin_matrices(&self) -> Vec<Mat4> {
        if self.model.skeleton.bones.is_empty() {
            vec![self.root_world]
        } else {
            self.model.skeleton.skin_matrices(&self.world)
        }
    }
}

pub struct Scene {
    pub actors: Vec<Actor>,
    pub camera: CamTrack,
    /// soleil animé (sans clé : soleil fixe des réglages d'éclairage)
    pub sun: SunTrack,
    /// ambiance animée : rotation et intensité (sans clé : réglages fixes)
    pub ambiance: AmbTrack,
    pub next_id: u64,
}

impl Default for Scene {
    fn default() -> Self {
        Self { actors: Vec::new(), camera: CamTrack::default(), sun: SunTrack::default(), ambiance: AmbTrack::default(), next_id: 1 }
    }
}

impl Scene {
    pub fn actor(&self, id: u64) -> Option<&Actor> {
        self.actors.iter().find(|a| a.id == id)
    }
    pub fn actor_mut(&mut self, id: u64) -> Option<&mut Actor> {
        self.actors.iter_mut().find(|a| a.id == id)
    }
    pub fn index_of(&self, id: u64) -> Option<usize> {
        self.actors.iter().position(|a| a.id == id)
    }

    pub fn add(&mut self, asset: &str, model: Arc<Model>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let mut a = Actor::new(id, asset, model);
        // nom unique
        let base = a.name.clone();
        let mut n = 2;
        while self.actors.iter().any(|o| o.name == a.name) {
            a.name = format!("{base} ({n})");
            n += 1;
        }
        self.actors.push(a);
        id
    }

    pub fn remove(&mut self, id: u64) {
        self.actors.retain(|a| a.id != id);
        for a in &mut self.actors {
            if a.attachment.as_ref().is_some_and(|t| t.parent() == id) {
                a.attachment = None;
            }
        }
    }

    /// Tous les acteurs attachés (directement ou non) à `id`, sans `id` lui-même.
    pub fn descendants(&self, id: u64) -> Vec<u64> {
        let mut out: Vec<u64> = Vec::new();
        let mut stack = vec![id];
        while let Some(p) = stack.pop() {
            for a in &self.actors {
                if a.id != id && !out.contains(&a.id) && a.attachment.as_ref().is_some_and(|t| t.parent() == p) {
                    out.push(a.id);
                    stack.push(a.id);
                }
            }
        }
        out
    }

    /// Acteurs à dessiner : visibles, et dont l'emplacement n'est pas couvert par une autre pièce
    /// visible du même personnage (comme dans le jeu, une cagoule intégrale `CoveredSlots
    /// CustomizationHead` masque la tête ; la tête revient si on masque le casque).
    pub fn drawn_actors(&self) -> impl Iterator<Item = &Actor> {
        let mut covered: Vec<(u64, &str)> = Vec::new();
        for a in self.actors.iter().filter(|a| a.visible && !a.model.covered_slots.is_empty()) {
            let root = self.merge_root(a.id);
            covered.extend(a.model.covered_slots.iter().map(|s| (root, s.as_str())));
        }
        let hidden: Vec<u64> = if covered.is_empty() {
            Vec::new()
        } else {
            self.actors
                .iter()
                .filter(|a| {
                    a.model.equip_slot.as_deref().is_some_and(|slot| {
                        let root = self.merge_root(a.id);
                        covered.iter().any(|(r, s)| *r == root && s.eq_ignore_ascii_case(slot))
                    })
                })
                .map(|a| a.id)
                .collect()
        };
        self.actors.iter().filter(move |a| a.visible && !hidden.contains(&a.id))
    }

    /// Personnage auquel une pièce est fusionnée (en remontant les fusions) ; l'acteur lui-même sinon.
    pub fn merge_root(&self, id: u64) -> u64 {
        let mut cur = id;
        for _ in 0..self.actors.len() {
            match self.actor(cur).and_then(|a| a.attachment.as_ref()) {
                Some(Attachment::BoneMerge { parent }) if self.actor(*parent).is_some() && *parent != id => cur = *parent,
                _ => break,
            }
        }
        cur
    }

    /// Ordre d'évaluation : parents avant enfants attachés.
    fn order(&self) -> Vec<usize> {
        let mut order = Vec::new();
        let mut done = vec![false; self.actors.len()];
        for _ in 0..self.actors.len() + 1 {
            for (i, a) in self.actors.iter().enumerate() {
                if done[i] {
                    continue;
                }
                let ready = match &a.attachment {
                    None => true,
                    Some(t) => match self.index_of(t.parent()) {
                        Some(p) => done[p],
                        None => true,
                    },
                };
                if ready {
                    done[i] = true;
                    order.push(i);
                }
            }
        }
        // cycles éventuels : on les évalue quand même
        for (i, d) in done.iter().enumerate() {
            if !d {
                order.push(i);
            }
        }
        order
    }

    /// Évalue toutes les poses et matrices monde à une image.
    pub fn evaluate(&mut self, frame: f32) {
        for a in &mut self.actors {
            a.evaluate_local(frame);
        }
        self.update_world();
    }

    /// Recalcule les matrices monde à partir des poses locales courantes.
    pub fn update_world(&mut self) {
        for i in self.order() {
            let attachment = self.actors[i].attachment.clone();
            let mut root = self.actors[i].root_pose.to_mat4();
            let mut merged: HashMap<u32, Mat4> = HashMap::new();
            match &attachment {
                Some(Attachment::Bone { parent, bone }) => {
                    if let Some(p) = self.actor(*parent) {
                        let w = p.bone_index(bone).map(|b| p.world[b]).unwrap_or(p.root_world);
                        root = w * root;
                    }
                }
                Some(Attachment::BoneMerge { parent }) => {
                    if let Some(p) = self.actor(*parent) {
                        root = p.root_world * root;
                        // déformation de chaque os du parent (monde × liaison inverse) : un modèle
                        // dont le squelette a d'autres orientations de liaison (casquettes
                        // CommissarHat, Look005Upgrade…) suit le même mouvement que le parent
                        for (b, w) in p.model.skeleton.bones.iter().zip(&p.world) {
                            merged.insert(b.hash, *w * b.inverse_bind);
                        }
                    }
                }
                None => {}
            }
            let a = &mut self.actors[i];
            a.root_world = root;
            if merged.is_empty() {
                a.world = a.model.skeleton.world_matrices(&a.pose, root);
            } else {
                // fusion : os de même nom déformés comme ceux du parent, les autres suivent leur hiérarchie
                let sk = &a.model.skeleton;
                let mut world = vec![Mat4::IDENTITY; sk.bones.len()];
                for b in sk.eval_order() {
                    let bone = &sk.bones[b];
                    world[b] = if let Some(d) = merged.get(&bone.hash) {
                        *d * bone.bind_world
                    } else {
                        let local = a.pose[b].to_mat4();
                        match bone.parent {
                            Some(p) => world[p] * local,
                            None => root * local,
                        }
                    };
                }
                a.world = world;
            }
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            actors: self.actors.clone(),
            camera: self.camera.clone(),
            sun: self.sun.clone(),
            ambiance: self.ambiance.clone(),
        }
    }

    /// Revient à l'état mémorisé : animation et liens des acteurs présents, et liste des
    /// acteurs elle-même (un acteur supprimé depuis revient, un acteur ajouté depuis s'en va).
    pub fn restore(&mut self, s: &Snapshot) {
        let mut cur = std::mem::take(&mut self.actors);
        self.actors = s
            .actors
            .iter()
            .map(|snap| match cur.iter().position(|a| a.id == snap.id) {
                Some(i) => {
                    let mut a = cur.swap_remove(i);
                    a.anim = snap.anim.clone();
                    a.attachment = snap.attachment.clone();
                    a
                }
                None => snap.clone(),
            })
            .collect();
        self.camera = s.camera.clone();
        self.sun = s.sun.clone();
        self.ambiance = s.ambiance.clone();
    }

    /// Remet exactement l'état mémorisé, acteurs entiers compris (visibilité, camouflage...) :
    /// pour rendre un export de la file d'attente tel qu'il était à son ajout.
    pub fn replace(&mut self, s: &Snapshot) {
        self.actors = s.actors.clone();
        self.camera = s.camera.clone();
        self.sun = s.sun.clone();
        self.ambiance = s.ambiance.clone();
        self.next_id = self.next_id.max(self.actors.iter().map(|a| a.id + 1).max().unwrap_or(1));
    }
}

#[derive(Clone)]
pub struct Snapshot {
    /// copies complètes, pour pouvoir faire revenir un acteur supprimé
    actors: Vec<Actor>,
    camera: CamTrack,
    sun: SunTrack,
    ambiance: AmbTrack,
}

impl PartialEq for Snapshot {
    /// Seul ce qui est annulable compte : liste des acteurs, animation, liens, caméra, éclairage.
    fn eq(&self, o: &Self) -> bool {
        self.actors.len() == o.actors.len()
            && self
                .actors
                .iter()
                .zip(&o.actors)
                .all(|(a, b)| a.id == b.id && a.anim == b.anim && a.attachment == b.attachment)
            && self.camera == o.camera
            && self.sun == o.sun
            && self.ambiance == o.ambiance
    }
}

/// Historique annuler / rétablir.
#[derive(Default)]
pub struct History {
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
}

impl History {
    pub fn push(&mut self, s: Snapshot) {
        if self.undo.last() != Some(&s) {
            self.undo.push(s);
            if self.undo.len() > 200 {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
    }
    pub fn undo(&mut self, scene: &mut Scene) -> bool {
        if let Some(s) = self.undo.pop() {
            self.redo.push(scene.snapshot());
            scene.restore(&s);
            true
        } else {
            false
        }
    }
    pub fn redo(&mut self, scene: &mut Scene) -> bool {
        if let Some(s) = self.redo.pop() {
            self.undo.push(scene.snapshot());
            scene.restore(&s);
            true
        } else {
            false
        }
    }
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}

// ---------------------------------------------------------------- fichier projet

/// Marqueur de la timeline : une image repérée, avec un nom facultatif.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub frame: i32,
    #[serde(default)]
    pub name: String,
}


#[derive(Serialize, Deserialize)]
pub struct ProjectFile {
    pub format: String,
    pub fps: f32,
    pub start: i32,
    pub end: i32,
    pub actors: Vec<ActorFile>,
    pub camera: CamTrack,
    /// ambiance et éclairage (absents des anciens projets : éclairage studio)
    #[serde(default)]
    pub lighting: crate::environment::Lighting,
    /// clés du soleil (absentes des anciens projets : soleil fixe)
    #[serde(default)]
    pub sun: SunTrack,
    /// clés d'ambiance (absentes des anciens projets : ambiance fixe)
    #[serde(default)]
    pub ambiance_keys: AmbTrack,
    /// décor d'une carte (absent des anciens projets : pas de décor)
    #[serde(default)]
    pub decor: Option<crate::decor::DecorSettings>,
    /// rotation de la grille au sol (degrés ; absente des anciens projets : 0)
    #[serde(default)]
    pub grid_yaw: f32,
    /// origine de la grille au sol (absente des anciens projets : 0, 0, 0)
    #[serde(default)]
    pub grid_origin: [f32; 3],
    /// particules d'ambiance (absentes des anciens projets : « Auto », selon le continent)
    #[serde(default)]
    pub particles: crate::particles::Particles,
    /// post-traitement : étalonnage, profondeur de champ, netteté (absent des anciens projets : neutre)
    #[serde(default)]
    pub post: crate::post::PostFx,
    /// repères d'entrée et de sortie de l'export (absents des anciens projets : aucun)
    #[serde(default)]
    pub export_marks: Option<(i32, i32)>,
    /// marqueurs de la timeline (absents des anciens projets : aucun)
    #[serde(default)]
    pub markers: Vec<Marker>,
    /// caméra de travail à l'enregistrement (absente des anciens projets : la vue ne bouge pas)
    #[serde(default)]
    pub work_camera: Option<crate::camera::OrbitCamera>,
}

#[derive(Serialize, Deserialize)]
pub struct ActorFile {
    pub id: u64,
    pub name: String,
    pub asset: String,
    pub visible: bool,
    pub attachment: Option<Attachment>,
    pub root: Track,
    pub root_static: Key,
    /// pistes par nom d'os
    pub bones: BTreeMap<String, Track>,
    /// camouflage (absent des anciens projets : couleurs d'origine)
    #[serde(default)]
    pub camo: crate::camo::Camo,
    /// décalcomanie (absente des anciens projets : aucune)
    #[serde(default)]
    pub decal: crate::decal::Decal,
}

impl ActorFile {
    pub fn from_actor(a: &Actor) -> Self {
        let bones = a
            .model
            .skeleton
            .bones
            .iter()
            .zip(&a.anim.bones)
            .filter(|(_, t)| !t.is_empty())
            .map(|(b, t)| (b.name.clone(), t.clone()))
            .collect();
        Self {
            id: a.id,
            name: a.name.clone(),
            asset: a.asset.clone(),
            visible: a.visible,
            attachment: a.attachment.clone(),
            root: a.anim.root.clone(),
            root_static: Key::new(0, &a.anim.root_static, Interp::Smooth),
            bones,
            camo: a.camo.clone(),
            decal: a.decal.clone(),
        }
    }

    pub fn apply_to(&self, a: &mut Actor) {
        a.id = self.id;
        a.name = self.name.clone();
        a.visible = self.visible;
        a.attachment = self.attachment.clone();
        a.camo = self.camo.clone();
        a.decal = self.decal.clone();
        a.anim.root = self.root.clone();
        a.anim.root_static = self.root_static.transform();
        for (name, t) in &self.bones {
            if let Some(i) = a.bone_index(name) {
                a.anim.bones[i] = t.clone();
            }
        }
    }
}

pub fn quat_from_euler_deg(e: Vec3) -> Quat {
    Quat::from_euler(glam::EulerRot::YXZ, e.y.to_radians(), e.x.to_radians(), e.z.to_radians())
}

pub fn euler_deg_from_quat(q: Quat) -> Vec3 {
    let (y, x, z) = q.to_euler(glam::EulerRot::YXZ);
    Vec3::new(x.to_degrees(), y.to_degrees(), z.to_degrees())
}
