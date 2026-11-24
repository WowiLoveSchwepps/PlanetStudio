//! Repères de la timeline du compositeur : entrée et sortie (la plage exportée « Entrée → Sortie »)
//! et marqueurs nommés. Menu du clic droit sur la timeline, comme dans un logiciel de montage.

use super::export_ui::RangeMode;
use super::StudioApp;
use crate::keymap::Action;
use crate::scene::Marker;
use eframe::egui::{self, RichText};

/// Repère qu'on fait glisser dans la timeline.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum MarkDrag {
    In,
    Out,
    Marker(usize),
}

impl StudioApp {
    /// Entrée sur l'image `f` ; la sortie reste en place (repoussée si elle passe avant).
    pub(super) fn mark_in(&mut self, f: i32) {
        let out = self.export.marks.map(|m| m.1).unwrap_or(self.end).max(f);
        self.export.marks = Some((f, out));
        self.config.export.video.range = RangeMode::InOut;
    }

    /// Sortie sur l'image `f` ; l'entrée reste en place (ramenée si elle passe après).
    pub(super) fn mark_out(&mut self, f: i32) {
        let inp = self.export.marks.map(|m| m.0).unwrap_or(self.start).min(f);
        self.export.marks = Some((inp, f));
        self.config.export.video.range = RangeMode::InOut;
    }

    fn mark_range(&mut self, a: i32, b: i32) {
        self.export.marks = Some((a.min(b), a.max(b)));
        self.config.export.video.range = RangeMode::InOut;
    }

    /// Sans entrée, la plage commence à Début.
    fn clear_mark_in(&mut self) {
        if let Some((_, b)) = self.export.marks {
            if b >= self.end {
                self.clear_in_out();
            } else {
                self.export.marks = Some((self.start, b));
            }
        }
    }

    /// Sans sortie, la plage finit à Fin.
    fn clear_mark_out(&mut self) {
        if let Some((a, _)) = self.export.marks {
            if a <= self.start {
                self.clear_in_out();
            } else {
                self.export.marks = Some((a, self.end));
            }
        }
    }

    pub(super) fn clear_in_out(&mut self) {
        self.export.marks = None;
        if self.config.export.video.range == RangeMode::InOut {
            self.config.export.video.range = RangeMode::Full;
        }
    }

    pub(super) fn go_to_in(&mut self) {
        if let Some((a, _)) = self.export.marks {
            self.playing = false;
            self.set_frame(a as f32);
        }
    }

    pub(super) fn go_to_out(&mut self) {
        if let Some((_, b)) = self.export.marks {
            self.playing = false;
            self.set_frame(b as f32);
        }
    }

    /// Ajoute un marqueur sur l'image `f` (rien s'il y en a déjà un) ; renvoie son indice.
    pub(super) fn add_marker(&mut self, f: i32) -> usize {
        if let Some(i) = self.markers.iter().position(|m| m.frame == f) {
            self.status = trf!("Il y a déjà un marqueur à l'image {f}", "There is already a marker at frame {f}");
            return i;
        }
        self.markers.push(Marker { frame: f, name: String::new() });
        self.markers.sort_by_key(|m| m.frame);
        self.status = trf!("Marqueur ajouté à l'image {f}", "Marker added at frame {f}");
        self.markers.iter().position(|m| m.frame == f).unwrap_or(0)
    }

    /// Va au marqueur suivant (`dir` > 0) ou précédent.
    fn go_to_marker(&mut self, dir: i32) {
        let cur = self.cur_frame();
        let target = if dir > 0 {
            self.markers.iter().map(|m| m.frame).filter(|&f| f > cur).min()
        } else {
            self.markers.iter().map(|m| m.frame).filter(|&f| f < cur).max()
        };
        if let Some(f) = target {
            self.playing = false;
            self.set_frame(f as f32);
        }
    }

    /// Fin d'un glisser de marqueur : on garde la liste triée et sans doublon.
    pub(super) fn tidy_markers(&mut self) {
        self.markers.sort_by_key(|m| m.frame);
        self.markers.dedup_by_key(|m| m.frame);
    }

    /// Images couvertes par le « clip » : les clés de la sélection, sinon celles de la caméra.
    fn clip_range(&self) -> Option<(i32, i32)> {
        let keys = if self.sel_actor.is_some() {
            self.selection_key_frames()
        } else {
            self.scene.camera.keys.iter().map(|k| k.frame).collect()
        };
        Some((*keys.iter().min()?, *keys.iter().max()?))
    }

    /// Menu du clic droit sur la timeline.
    pub(super) fn timeline_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(230.0);
        let km = self.config.keymap.clone();
        let item = |ui: &mut egui::Ui, enabled: bool, text: &str, a: Option<Action>| -> bool {
            let mut b = egui::Button::new(text);
            if let Some(a) = a {
                b = b.shortcut_text(km.label(a));
            }
            ui.add_enabled(enabled, b).clicked()
        };
        let cur = self.cur_frame();
        let marks = self.export.marks;

        // marqueur sous le clic : nom, atteindre, supprimer
        if let Some(i) = self.tl_menu_marker.filter(|&i| i < self.markers.len()) {
            let f = self.markers[i].frame;
            ui.label(RichText::new(trf!("Marqueur à l'image {f}", "Marker at frame {f}")).strong());
            ui.horizontal(|ui| {
                ui.label(tr!("Nom :", "Name:"));
                ui.add(egui::TextEdit::singleline(&mut self.markers[i].name).desired_width(150.0).hint_text(tr!("sans nom", "unnamed")));
            });
            if item(ui, true, tr!("Atteindre ce marqueur", "Go to this marker"), None) {
                self.playing = false;
                self.set_frame(f as f32);
                ui.close_menu();
            }
            if item(ui, true, tr!("Supprimer ce marqueur", "Delete this marker"), None) {
                self.markers.remove(i);
                self.tl_menu_marker = None;
                ui.close_menu();
            }
            ui.separator();
        }

        if item(ui, true, tr!("Marquer l'entrée", "Mark in"), Some(Action::MarkIn)) {
            self.mark_in(cur);
            ui.close_menu();
        }
        if item(ui, true, tr!("Marquer la sortie", "Mark out"), Some(Action::MarkOut)) {
            self.mark_out(cur);
            ui.close_menu();
        }
        let clip = self.clip_range();
        let r = ui.add_enabled(clip.is_some(), egui::Button::new(tr!("Marquer le clip", "Mark the clip"))).on_hover_text(
            tr!("Entrée et sortie autour des clés de la sélection\n(sans sélection : autour des clés de la caméra de prise)", "In and out around the selection's keys\n(no selection: around the shot camera's keys)"),
        );
        if r.clicked() {
            if let Some((a, b)) = clip {
                self.mark_range(a, b);
            }
            ui.close_menu();
        }
        let zoomed = self.tl_view.is_some();
        let r = ui
            .add_enabled(zoomed, egui::Button::new(tr!("Marquer la plage visible", "Mark the visible range")))
            .on_hover_text(tr!("Entrée et sortie aux bords de la timeline zoomée", "In and out at the edges of the zoomed timeline"));
        if r.clicked() {
            let (v0, v1) = self.tl_view_range();
            self.mark_range(v0.round() as i32, v1.round() as i32);
            ui.close_menu();
        }
        ui.separator();
        if item(ui, marks.is_some(), tr!("Atteindre l'entrée", "Go to in point"), Some(Action::GoToIn)) {
            self.go_to_in();
            ui.close_menu();
        }
        if item(ui, marks.is_some(), tr!("Atteindre la sortie", "Go to out point"), Some(Action::GoToOut)) {
            self.go_to_out();
            ui.close_menu();
        }
        ui.separator();
        if item(ui, marks.is_some_and(|m| m.0 > self.start), tr!("Effacer l'entrée", "Clear in point"), None) {
            self.clear_mark_in();
            ui.close_menu();
        }
        if item(ui, marks.is_some_and(|m| m.1 < self.end), tr!("Effacer la sortie", "Clear out point"), None) {
            self.clear_mark_out();
            ui.close_menu();
        }
        if item(ui, marks.is_some(), tr!("Effacer l'entrée et la sortie", "Clear in and out points"), Some(Action::ClearInOut)) {
            self.clear_in_out();
            ui.close_menu();
        }
        ui.separator();
        if item(ui, true, tr!("Ajouter un marqueur", "Add a marker"), Some(Action::AddMarker)) {
            let i = self.add_marker(cur);
            // le menu reste ouvert sur le nouveau marqueur pour pouvoir le nommer
            self.tl_menu_marker = Some(i);
        }
        let has = !self.markers.is_empty();
        if item(ui, self.markers.iter().any(|m| m.frame < cur), tr!("Marqueur précédent", "Previous marker"), None) {
            self.go_to_marker(-1);
            ui.close_menu();
        }
        if item(ui, self.markers.iter().any(|m| m.frame > cur), tr!("Marqueur suivant", "Next marker"), None) {
            self.go_to_marker(1);
            ui.close_menu();
        }
        if item(ui, has, tr!("Effacer tous les marqueurs", "Clear all markers"), None) {
            self.markers.clear();
            self.tl_menu_marker = None;
            ui.close_menu();
        }
    }
}
