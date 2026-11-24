//! Aperçu au survol des listes d'assets (acteurs, modèles, armures, casques) : une bulle près du
//! curseur montre le modèle seul, qui tourne sur lui-même.
//!
//! Le modèle survolé est chargé en arrière-plan après un court arrêt du curseur (pas pendant
//! qu'on parcourt la liste), puis dessiné chaque image dans une petite cible à part. Les derniers
//! aperçus restent en mémoire pour qu'un nouveau survol soit immédiat ; au-delà, leurs modèles et
//! les textures qu'eux seuls utilisaient sont libérés.

use super::StudioApp;
use crate::render::DrawItem;
use eframe::egui::{self, Color32, Pos2, Rect, RichText, Sense};
use forgelight::Model;
use glam::{Mat4, Quat, Vec3};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Côté de l'image dans la bulle (points).
const SIZE: f32 = 220.0;
/// Arrêt du curseur avant de charger le modèle survolé.
const HOVER_DELAY: Duration = Duration::from_millis(180);
/// Aperçus gardés en mémoire.
const KEEP: usize = 4;
/// Identifiant d'acteur du rendu de l'aperçu (ceux de la scène partent de 1).
const PREVIEW_ID: u64 = u64::MAX;
/// Un tour complet (s).
const TURN_SECONDS: f32 = 8.0;

struct Entry {
    asset: String,
    model: Arc<Model>,
    /// modèle envoyé au GPU pour l'aperçu (à libérer), pas déjà là pour la scène
    own_model: bool,
    /// textures envoyées pour l'aperçu
    textures: Vec<String>,
    /// cadrage : bas de l'objet, centre sur le sol, rayon autour de l'axe vertical, hauteur
    fit: Option<Fit>,
}

#[derive(Clone, Copy)]
struct Fit {
    base: Vec3,
    radius: f32,
    height: f32,
}

impl Fit {
    /// D'après les sommets (la boîte des fichiers du jeu est souvent bien plus large que l'objet).
    fn of(model: &Model) -> Option<Fit> {
        let pts = || model.meshes.iter().filter(|m| !m.indices.is_empty()).flat_map(|m| &m.vertices).map(|v| Vec3::from(v.position));
        let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for p in pts().filter(|p| p.is_finite()) {
            lo = lo.min(p);
            hi = hi.max(p);
        }
        if hi.cmplt(lo).any() {
            return None;
        }
        let base = Vec3::new((lo.x + hi.x) * 0.5, lo.y, (lo.z + hi.z) * 0.5);
        let radius = pts()
            .filter(|p| p.is_finite())
            .map(|p| (p.x - base.x).hypot(p.z - base.z))
            .fold(0.0_f32, f32::max);
        Some(Fit { base, radius: radius.max(0.02), height: (hi.y - lo.y).max(0.02) })
    }
}

#[derive(Default)]
pub(super) struct AssetPreview {
    /// asset sous le curseur à cette image (rempli par les listes)
    hovered: Option<String>,
    /// asset survolé, et depuis quand
    since: Option<(String, Instant)>,
    /// chargements en cours : requête → asset
    loading: HashMap<u64, String>,
    /// aperçus prêts, le plus récent à la fin
    entries: VecDeque<Entry>,
    failed: HashMap<String, String>,
    /// asset dont l'image est dans la texture de l'aperçu
    shown: Option<String>,
    /// début du tour (au premier affichage d'un asset)
    turn_start: Option<Instant>,
}

impl AssetPreview {
    /// Bulle d'aperçu sur une entrée de liste qui désigne l'asset `asset`.
    pub(super) fn attach(&mut self, resp: egui::Response, asset: &str, tex: Option<egui::TextureId>) -> egui::Response {
        if resp.hovered() {
            self.hovered = Some(asset.to_string());
        }
        resp.on_hover_ui_at_pointer(|ui| self.tooltip(ui, asset, tex))
    }

    fn tooltip(&self, ui: &mut egui::Ui, asset: &str, tex: Option<egui::TextureId>) {
        ui.set_max_width(SIZE);
        ui.label(RichText::new(asset).strong().size(12.0));
        let (rect, _) = ui.allocate_exact_size(egui::vec2(SIZE, SIZE), Sense::hover());
        let p = ui.painter();
        p.rect_filled(rect, 4.0, Color32::from_rgb(28, 31, 36));
        match tex {
            Some(t) if self.shown.as_deref() == Some(asset) => {
                p.image(t, rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
            }
            _ => match self.failed.get(asset) {
                Some(e) => {
                    p.text(rect.center(), egui::Align2::CENTER_CENTER, tr!("Aperçu impossible", "Preview unavailable"), egui::FontId::proportional(13.0), Color32::LIGHT_RED);
                    ui.label(RichText::new(e).small().weak());
                }
                None => egui::Spinner::new().size(28.0).paint_at(ui, Rect::from_center_size(rect.center(), egui::vec2(28.0, 28.0))),
            },
        }
    }

    /// Cette requête du chargeur est-elle celle d'un aperçu ?
    pub(super) fn owns(&self, request: u64) -> bool {
        self.loading.contains_key(&request)
    }

    fn entry(&self, asset: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.asset == asset)
    }
}

impl StudioApp {
    /// Une fois par image, après les listes : charge l'asset survolé, puis dessine son aperçu.
    pub(super) fn update_asset_preview(&mut self, ctx: &egui::Context) {
        let hovered = self.asset_preview.hovered.take();
        let Some(asset) = hovered else {
            self.asset_preview.since = None;
            return;
        };
        let now = Instant::now();
        let since = match &self.asset_preview.since {
            Some((a, t)) if *a == asset => *t,
            _ => {
                self.asset_preview.since = Some((asset.clone(), now));
                now
            }
        };
        ctx.request_repaint();
        let ap = &self.asset_preview;
        let ready = ap.entry(&asset).is_some();
        if !ready && !ap.failed.contains_key(&asset) && !ap.loading.values().any(|a| *a == asset) {
            if let Some(m) = self.models.get(&asset).cloned() {
                // déjà dans la scène : rien à charger ni à libérer
                let fit = Fit::of(&m);
                self.push_preview(Entry { asset: asset.clone(), model: m, own_model: false, textures: Vec::new(), fit });
            } else if now - since >= HOVER_DELAY {
                if let Some(game) = self.game.clone() {
                    let id = self.next_request;
                    self.next_request += 1;
                    self.asset_preview.loading.insert(id, asset.clone());
                    let skip: Vec<String> = self.renderer.texture_names().into_iter().collect();
                    self.loader.load_model_quiet(game, asset.clone(), id, skip);
                }
            } else {
                ctx.request_repaint_after(HOVER_DELAY);
            }
        }
        self.render_asset_preview(ctx, &asset);
    }

    /// Modèle chargé pour l'aperçu (requête dont [`AssetPreview::owns`] est vrai).
    pub(super) fn take_preview_model(
        &mut self,
        request: u64,
        result: Result<(Arc<Model>, Vec<(String, forgelight::dds::Image)>), String>,
    ) {
        let Some(asset) = self.asset_preview.loading.remove(&request) else { return };
        match result {
            // déjà prêt (chargé entre-temps par un autre chemin) : rien à faire
            Ok(_) if self.asset_preview.entry(&asset).is_some() => {}
            Ok((model, textures)) => {
                let mut uploaded = Vec::new();
                for (name, img) in &textures {
                    if !self.renderer.has_texture(name) {
                        self.renderer.upload_texture(name, img);
                        uploaded.push(name.clone());
                    }
                }
                let own_model = !self.renderer.has_model(&asset);
                if own_model {
                    self.renderer.upload_model(&asset, &model);
                }
                let fit = Fit::of(&model);
                self.push_preview(Entry { asset, model, own_model, textures: uploaded, fit });
            }
            Err(e) => {
                self.asset_preview.failed.insert(asset, e);
            }
        }
    }

    /// Modèle déjà chargé par l'aperçu (ajout à la scène sans relecture).
    pub(super) fn preview_model(&self, asset: &str) -> Option<Arc<Model>> {
        self.asset_preview.entry(asset).map(|e| e.model.clone())
    }

    fn push_preview(&mut self, e: Entry) {
        self.asset_preview.entries.push_back(e);
        while self.asset_preview.entries.len() > KEEP {
            let old = self.asset_preview.entries.pop_front().unwrap();
            // repris par la scène entre-temps : son modèle reste
            let model = (old.own_model && !self.models.contains_key(&old.asset)).then_some(old.asset.as_str());
            self.renderer.forget_preview(model, &old.textures);
            if self.asset_preview.shown.as_deref() == Some(old.asset.as_str()) {
                self.asset_preview.shown = None;
            }
        }
    }

    fn render_asset_preview(&mut self, ctx: &egui::Context, asset: &str) {
        let Some(e) = self.asset_preview.entry(asset) else { return };
        let Some(fit) = e.fit else { return };
        let bones = e.model.skeleton.bones.len().max(1);
        if self.asset_preview.shown.as_deref() != Some(asset) {
            self.asset_preview.shown = Some(asset.to_string());
            self.asset_preview.turn_start = Some(Instant::now());
        }
        let t = self.asset_preview.turn_start.map_or(0.0, |s| s.elapsed().as_secs_f32());
        // posé sur le sol (y = 0), centré sur l'axe vertical, qui tourne autour de lui ; la caméra
        // garde dans le cadre la hauteur et la largeur de l'objet sous tous les angles
        let angle = std::f32::consts::TAU * (t / TURN_SECONDS) + 0.6;
        let place = Mat4::from_quat(Quat::from_rotation_y(angle)) * Mat4::from_translation(-fit.base);
        let fov = 30.0_f32;
        let half = (fov.to_radians() * 0.5).tan();
        let dist = (fit.height * 0.5).max(fit.radius) * 1.08 / half + fit.radius;
        let target = Vec3::new(0.0, fit.height * 0.5, 0.0);
        let eye = target + Vec3::new(0.0, 0.25, 1.0).normalize() * dist;
        let view = crate::camera::ViewParams { eye, target, fov_deg: fov };
        let item = DrawItem {
            id: PREVIEW_ID,
            asset: asset.to_string(),
            skin: vec![place; bones],
            highlight: false,
            camo: Default::default(),
            decal: Default::default(),
        };
        let px = (SIZE * ctx.pixels_per_point()).round() as u32;
        self.renderer.render_preview(&self.rs, px, view.view_proj(1.0), eye, &item);
    }
}
