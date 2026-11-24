//! Fenêtre « 🎞 Caméra prédéfinie » : on choisit un mouvement (tour à 360°, arc, spirale,
//! travelling, grue) ou un plan fixe, on règle le cadrage, et « OK » écrit les clés sur la
//! caméra de prise, ciblées sur le personnage sélectionné.

use super::StudioApp;
use crate::anim::CamTrack;
use crate::shots::{self, Framing, Height, Move, ShotSettings, Side};
use eframe::egui::{self, Color32, RichText};

const ACCENT: Color32 = Color32::from_rgb(90, 200, 255);
const WARN: Color32 = Color32::from_rgb(255, 190, 90);

/// Aperçu du plan, tant que la fenêtre est ouverte : la caméra de prise affichée le suit,
/// sans rien écrire dans le projet. « OK » écrit exactement cette piste.
pub(super) struct ShotPreview {
    settings: ShotSettings,
    start: i32,
    subject: u64,
    /// caméra de prise réelle à partir de laquelle l'aperçu a été calculé
    base: CamTrack,
    /// dernière image du plan
    end: i32,
    /// piste montrée ; `None` : aperçu en pause (caméra modifiée à la main)
    pub(super) track: Option<CamTrack>,
}

impl StudioApp {
    /// Personnage visé : la sélection (ramenée au corps qui la porte), sinon le seul
    /// personnage de la scène.
    fn shot_subject_id(&self) -> Option<u64> {
        if let Some(id) = self.sel_actor.filter(|id| self.scene.actor(*id).is_some()) {
            return Some(shots::root_actor(&self.scene, id));
        }
        let mut roots = self.scene.actors.iter().filter(|a| a.visible && a.attachment.is_none());
        match (roots.next(), roots.next()) {
            (Some(a), None) => Some(a.id),
            _ => None,
        }
    }

    /// Bouton de la barre de la vue qui ouvre la fenêtre.
    pub(super) fn shots_button(&mut self, ui: &mut egui::Ui) {
        if ui
            .selectable_label(self.shots_open, tr!("🎞 Caméra prédéfinie", "🎞 Preset camera"))
            .on_hover_text(tr!("Tour à 360°, arc, spirale, travelling… autour du personnage sélectionné", "360° orbit, arc, spiral, dolly… around the selected character"))
            .clicked()
        {
            self.shots_open = !self.shots_open;
            if self.shots_open {
                self.shots_start = self.cur_frame();
            }
        }
    }

    /// Fenêtre de la caméra prédéfinie.
    pub(super) fn shots_window(&mut self, ctx: &egui::Context) {
        if !self.shots_open {
            if self.shot_preview.take().is_some() {
                self.force_render = true;
            }
            return;
        }
        let before = self.config.shots;
        let subject = self.shot_subject_id();
        let subject_name = subject.and_then(|id| self.scene.actor(id)).map(|a| a.name.clone());
        let mut start = self.shots_start;
        let (t0, cur) = (self.start, self.cur_frame());
        let fps = self.fps;
        let has_keys = !self.scene.camera.keys.is_empty();
        // champ de vision de la caméra de prise au départ, et le personnage (pour la distance auto)
        let cam_fov = self.real_shot(start as f32).fov_deg;
        let subj = subject.and_then(|id| shots::subject(&self.scene, id));
        let preview_end = self.shot_preview.as_ref().filter(|p| p.track.is_some()).map(|p| p.end);
        let timeline_end = self.end;
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
        let mut clear = false;
        let mut play = false;
        egui::Window::new(tr!("🎞 Caméra prédéfinie", "🎞 Preset camera"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(380.0)
            .show(ctx, |ui| {
                let s = &mut self.config.shots;

                // ---- 1. personnage
                step(ui, "1", tr!("Personnage filmé", "Filmed character"));
                match &subject_name {
                    Some(name) => ui.label(RichText::new(format!("🎯 {name}")).strong()),
                    None => ui.label(
                        RichText::new(tr!("Aucun : cliquez sur le personnage à filmer (dans la vue ou dans la liste Scène).", "None: click the character to film (in the viewport or in the Scene list)."))
                            .color(WARN),
                    ),
                };
                ui.add_space(8.0);

                // ---- 2. mouvement
                step(ui, "2", tr!("Mouvement de la caméra", "Camera move"));
                egui::Grid::new("shot_moves").num_columns(2).spacing([6.0, 4.0]).show(ui, |ui| {
                    let mut i = 0;
                    for choice in Move::ALL.into_iter().map(Some).chain([None]) {
                        let label = choice.map_or(tr!("📍 Plan fixe (sans mouvement)", "📍 Static shot (no move)"), Move::label);
                        if ui.add_sized([175.0, 24.0], egui::SelectableLabel::new(s.choice == choice, label)).clicked() {
                            s.choice = choice;
                        }
                        i += 1;
                        if i % 2 == 0 {
                            ui.end_row();
                        }
                    }
                });
                let hint = s.choice.map_or(tr!("La caméra se place devant le personnage et ne bouge pas.", "The camera goes in front of the character and stays still."), Move::hint);
                ui.label(RichText::new(hint).weak().italics());
                ui.add_space(8.0);

                // ---- 3. réglages
                step(ui, "3", tr!("Réglages", "Settings"));
                let rotates = matches!(s.choice, Some(Move::Orbit360 | Move::Orbit180 | Move::Arc90 | Move::Spiral));
                egui::Grid::new("shot_settings").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                    ui.label(tr!("Cadrage", "Framing"));
                    combo(ui, "shot_framing", &mut s.framing, &Framing::ALL, |f| f.label());
                    ui.end_row();
                    ui.label(tr!("Champ de vision", "Field of view"));
                    ui.horizontal(|ui| {
                        if ui
                            .selectable_label(!s.fov_manual, trf!("caméra ({cam_fov:.0}°)", "camera ({cam_fov:.0}°)"))
                            .on_hover_text(tr!("Garde le champ de vision actuel de la caméra de prise", "Keeps the shot camera's current field of view"))
                            .clicked()
                        {
                            s.fov_manual = false;
                        }
                        if ui.selectable_label(s.fov_manual, tr!("réglé", "set")).on_hover_text(tr!("Champ de vision propre au plan", "Field of view specific to this shot")).clicked() && !s.fov_manual {
                            s.fov_manual = true;
                            s.fov_deg = cam_fov.round().clamp(shots::MIN_FOV, shots::MAX_FOV);
                        }
                        if s.fov_manual {
                            ui.add(
                                crate::calc::drag(&mut s.fov_deg)
                                    .range(shots::MIN_FOV..=shots::MAX_FOV)
                                    .speed(0.3)
                                    .suffix("°")
                                    .max_decimals(1),
                            )
                            .on_hover_text(tr!("Petit : téléobjectif, fond rapproché.  Grand : grand-angle, perspective accentuée.", "Small: telephoto, compressed background.  Large: wide angle, stronger perspective."));
                        }
                    });
                    ui.end_row();
                    ui.label("Distance");
                    ui.horizontal(|ui| {
                        let auto = subj.map(|p| shots::auto_distance(s.framing, &p, s.fov(cam_fov)));
                        let auto_text = auto.map_or("auto".to_string(), |d| format!("auto ({})", meters(d)));
                        if ui
                            .selectable_label(!s.dist_manual, auto_text)
                            .on_hover_text(tr!("La distance qui donne le cadrage choisi avec ce champ de vision", "The distance that gives the chosen framing with this field of view"))
                            .clicked()
                        {
                            s.dist_manual = false;
                        }
                        if ui
                            .selectable_label(s.dist_manual, tr!("réglée", "set"))
                            .on_hover_text(tr!("Distance fixe entre la caméra et le point visé du personnage", "Fixed distance between the camera and the character's target point"))
                            .clicked()
                            && !s.dist_manual
                        {
                            s.dist_manual = true;
                            if let Some(d) = auto {
                                s.dist_m = ((d * 10.0).round() / 10.0).clamp(shots::MIN_DIST, shots::MAX_DIST);
                            }
                        }
                        if s.dist_manual {
                            ui.add(
                                crate::calc::drag(&mut s.dist_m)
                                    .range(shots::MIN_DIST..=shots::MAX_DIST)
                                    .speed(0.02)
                                    .suffix(" m")
                                    .max_decimals(2),
                            )
                            .on_hover_text(tr!("Mètres entre la caméra et le point visé (travelling : distance au plus près)", "Meters between the camera and the target point (dolly: closest distance)"));
                        }
                    });
                    ui.end_row();
                    ui.label(tr!("Hauteur", "Height"));
                    combo(ui, "shot_height", &mut s.height, &Height::ALL, |h| h.label());
                    ui.end_row();
                    ui.label(if rotates { tr!("Départ", "Start side") } else { tr!("Côté", "Side") });
                    combo(ui, "shot_side", &mut s.side, &Side::ALL, |c| c.label());
                    ui.end_row();
                    if rotates {
                        ui.label(tr!("Sens", "Direction"));
                        ui.horizontal(|ui| {
                            ui.selectable_value(&mut s.clockwise, false, tr!("↺ vers la gauche", "↺ to the left"));
                            ui.selectable_value(&mut s.clockwise, true, tr!("↻ vers la droite", "↻ to the right"));
                        });
                        ui.end_row();
                    }
                    ui.label(tr!("Début", "Start"));
                    ui.horizontal(|ui| {
                        ui.add(crate::calc::drag(&mut start).range(t0..=i32::MAX).prefix(tr!("image ", "frame ")))
                            .on_hover_text(tr!("Image où le plan commence", "Frame where the shot starts"));
                        if start != cur
                            && ui.small_button(tr!("= tête de lecture", "= playhead")).on_hover_text(trf!("Commencer à l'image {cur}", "Start at frame {cur}")).clicked()
                        {
                            start = cur;
                        }
                    });
                    ui.end_row();
                    if s.choice.is_some() {
                        ui.label(tr!("Durée", "Duration"));
                        ui.add(egui::Slider::new(&mut s.secs, 1.0..=60.0).logarithmic(true).suffix(" s").max_decimals(1));
                        ui.end_row();
                        ui.label(tr!("Vitesse", "Speed"));
                        ui.horizontal(|ui| {
                            ui.selectable_value(&mut s.ease, true, tr!("douce", "eased"))
                                .on_hover_text(tr!("Démarre et s'arrête doucement", "Starts and stops smoothly"));
                            ui.selectable_value(&mut s.ease, false, tr!("constante", "constant"))
                                .on_hover_text(tr!("Même vitesse du début à la fin : idéal pour une vidéo qui boucle", "Same speed from start to end: ideal for a looping video"));
                        });
                        ui.end_row();
                    }
                });
                if s.choice.is_some() {
                    ui.checkbox(&mut s.follow, tr!("Suivre le personnage s'il bouge (danse, marche)", "Follow the character if it moves (dance, walk)"));
                    ui.checkbox(&mut s.replace, tr!("Effacer les anciennes clés de caméra", "Erase the old camera keys")).on_hover_text(
                        tr!(
                            "Décoché : le mouvement est ajouté à partir de l'image courante, \
                             les clés de caméra avant et après sont gardées.",
                            "Unchecked: the move is added from the current frame, \
                             camera keys before and after are kept."
                        ),
                    );
                }
                ui.add_space(8.0);

                // ---- résumé
                egui::Frame::group(ui.style()).fill(Color32::from_black_alpha(60)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let fov = s.fov(cam_fov);
                    let dist = subj.map(|p| s.distance(&p, fov));
                    let text = summary(s, subject_name.as_deref(), start, fps, has_keys, fov, dist);
                    ui.label(RichText::new(text).color(Color32::from_gray(220)));
                });
                ui.add_space(4.0);

                // ---- aperçu
                ui.horizontal(|ui| {
                    let can_play = preview_end.is_some();
                    if ui
                        .add_enabled(can_play, egui::Button::new(tr!("▶ Lire l'aperçu", "▶ Play preview")))
                        .on_hover_text(tr!("Passe en vue Prise et lit le plan depuis son début", "Switches to Shot view and plays the shot from its start"))
                        .on_disabled_hover_text(tr!("Aperçu en pause : changez un réglage pour le relancer", "Preview paused: change a setting to restart it"))
                        .clicked()
                    {
                        play = true;
                    }
                    let note = match preview_end {
                        Some(_) => tr!("👁 Aperçu en direct dans la caméra de prise (rien n'est écrit avant OK)", "👁 Live preview in the shot camera (nothing is written before OK)"),
                        None if subject_name.is_some() => tr!("⏸ Aperçu en pause : la caméra a été déplacée à la main", "⏸ Preview paused: the camera was moved by hand"),
                        None => "",
                    };
                    ui.label(RichText::new(note).small().weak());
                });
                if let Some(end) = preview_end.filter(|e| *e > timeline_end) {
                    ui.label(
                        RichText::new(trf!(
                            "Le plan finit à l'image {end}, après la fin de la timeline ({timeline_end}) : \
                             OK l'allongera ; l'aperçu s'arrête à {timeline_end}.",
                "The shot ends at frame {end}, after the end of the timeline ({timeline_end}): OK will extend it; the preview stops at {timeline_end}."
                        ))
                        .small()
                        .color(WARN),
                    );
                }
                ui.add_space(6.0);

                // ---- OK / Annuler
                ui.horizontal(|ui| {
                    let ok = egui::Button::new(RichText::new("   ✔ OK   ").strong().color(Color32::BLACK))
                        .fill(ACCENT)
                        .min_size(egui::vec2(110.0, 28.0));
                    if ui
                        .add_enabled(subject_name.is_some(), ok)
                        .on_hover_text(tr!("Crée les clés de la caméra de prise", "Creates the shot camera keys"))
                        .on_disabled_hover_text(tr!("Sélectionnez d'abord le personnage à filmer", "First select the character to film"))
                        .clicked()
                    {
                        apply = true;
                    }
                    if ui.add(egui::Button::new(tr!("Annuler", "Cancel")).min_size(egui::vec2(90.0, 28.0))).clicked() {
                        cancel = true;
                    }
                });

                // ---- remise à zéro
                ui.add_space(4.0);
                ui.separator();
                let n = self.scene.camera.keys.len();
                let label = if n == 0 {
                    tr!("🗑 Effacer toutes les clés de caméra", "🗑 Erase all camera keys").to_string()
                } else {
                    trf!("🗑 Effacer toutes les clés de caméra ({n})", "🗑 Erase all camera keys ({n})")
                };
                if ui
                    .add_enabled(n > 0, egui::Button::new(label))
                    .on_hover_text(tr!("La caméra de prise ne bouge plus : elle reste là où elle est à l'image courante. Ctrl+Z pour annuler.", "The shot camera stops moving: it stays where it is on the current frame. Ctrl+Z to undo."))
                    .on_disabled_hover_text(tr!("La caméra de prise n'a aucune clé", "The shot camera has no keys"))
                    .clicked()
                {
                    clear = true;
                }
            });
        if self.config.shots != before {
            self.config.save();
        }
        self.shots_start = start;
        if clear {
            self.playing = false;
            self.clear_camera_keys();
        }
        if apply {
            self.make_shot(self.config.shots.choice);
        }
        self.shots_open = open && !apply && !cancel;
        if self.shots_open {
            self.update_shot_preview(subject);
            if play {
                if let Some(p) = &self.shot_preview {
                    self.shot_view = true;
                    self.set_frame(p.start as f32);
                    self.playing = true;
                    self.last_tick = std::time::Instant::now();
                }
            }
        } else if self.shot_preview.take().is_some() {
            self.force_render = true;
        }
    }

    /// Recalcule l'aperçu quand un réglage, le début ou le personnage change (ou la caméra
    /// réelle, par Ctrl+Z). En pause après une modification à la main, il attend le
    /// prochain changement de réglage.
    fn update_shot_preview(&mut self, subject: Option<u64>) {
        let (Some(id), None) = (subject, &self.cam_rec) else {
            if self.shot_preview.take().is_some() {
                self.force_render = true;
            }
            return;
        };
        let s = self.config.shots;
        let start = self.shots_start;
        let same = self.shot_preview.as_ref().filter(|p| p.settings == s && p.start == start && p.subject == id);
        if let Some(p) = same {
            if p.track.is_none() || p.base == self.scene.camera {
                return;
            }
        }
        // un réglage a changé (pas à l'ouverture) : on montre la caméra de prise
        let changed = self.shot_preview.is_some() && same.is_none();
        self.shot_preview = self.build_shot(id, s.choice, start).map(|(track, end)| ShotPreview {
            settings: s,
            start,
            subject: id,
            base: self.scene.camera.clone(),
            end,
            track: Some(track),
        });
        if changed {
            self.shot_view = true;
            // tête de lecture hors du plan : on la ramène à son début
            let end = self.shot_preview.as_ref().map_or(start, |p| p.end);
            if !self.playing && (self.cur_frame() < start || self.cur_frame() > end) {
                self.set_frame(start as f32);
            }
        }
        self.force_render = true;
    }

    /// Piste de la caméra de prise avec le plan (mouvement animé, ou cadrage fixe si `mv` est
    /// `None`) à partir de l'image `start`, et la dernière image du plan. Ne modifie pas le projet.
    fn build_shot(&mut self, id: u64, mv: Option<Move>, start: i32) -> Option<(CamTrack, i32)> {
        let s: ShotSettings = self.config.shots;
        self.scene.evaluate(start as f32);
        let Some(first) = shots::subject(&self.scene, id) else {
            self.scene.evaluate(self.frame);
            return None;
        };
        let fov = s.fov(self.real_shot(start as f32).fov_deg);
        let mut track = self.scene.camera.clone();
        let end = match mv {
            None => {
                let v = shots::pose(None, &s, &first, first.facing, 0.0, fov);
                track.set_pose(start, v.eye, v.target, v.fov_deg, self.interp);
                start
            }
            Some(mv) => {
                let scene = &mut self.scene;
                let keys = shots::plan(mv, &s, start, self.fps, fov, |f| {
                    if !s.follow {
                        return first;
                    }
                    scene.evaluate(f as f32);
                    shots::subject(scene, id).unwrap_or(first)
                });
                let end = keys.last().map_or(start, |k| k.frame);
                shots::insert(&mut track, keys, s.replace);
                end
            }
        };
        self.scene.evaluate(self.frame);
        Some((track, end))
    }

    /// Crée un plan (mouvement animé, ou cadrage fixe si `mv` est `None`) sur la caméra de prise.
    fn make_shot(&mut self, mv: Option<Move>) {
        let Some(id) = self.shot_subject_id() else {
            self.status = tr!("Sélectionnez d'abord le personnage à filmer.", "First select the character to film.").into();
            return;
        };
        if self.cam_rec.is_some() {
            self.stop_recording();
        }
        self.playing = false;
        self.shot_preview = None;
        let s: ShotSettings = self.config.shots;
        let start = self.shots_start;
        self.set_frame(start as f32);
        // la même piste que l'aperçu
        let Some((track, end)) = self.build_shot(id, mv, start) else {
            return;
        };
        self.begin_edit();
        self.scene.camera = track;
        self.scene.evaluate(self.frame);
        self.editing = false;
        self.end = self.end.max(end);
        self.status = match mv {
            None => trf!(
                "Caméra de prise placée à l'image {start} : {}, {}, {} (Ctrl+Z pour annuler)",
                "Shot camera placed at frame {start}: {}, {}, {} (Ctrl+Z to undo)",
                s.framing.label(),
                s.height.label(),
                s.side.label()
            ),
            Some(mv) => {
                let play = self.config.keymap.label(crate::keymap::Action::PlayPause);
                trf!(
                    "{} créé de l'image {start} à {end} — {play} pour le voir (Ctrl+Z pour annuler)",
                    "{} created from frame {start} to {end} — {play} to watch it (Ctrl+Z to undo)",
                    plain(mv.label())
                )
            }
        };
        self.shot_view = true;
        self.force_render = true;
    }
}

/// Titre d'étape numérotée.
fn step(ui: &mut egui::Ui, n: &str, title: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!(" {n} ")).strong().color(Color32::BLACK).background_color(ACCENT));
        ui.label(RichText::new(title).strong().size(15.0));
    });
    ui.add_space(2.0);
}

/// Libellé sans son icône.
fn plain(label: &str) -> &str {
    label.split_once(' ').map_or(label, |(_, l)| l)
}

/// Distance lisible : « 4,2 m ».
fn meters(d: f32) -> String {
    let decimals = if d < 10.0 { 1 } else { 0 };
    format!("{d:.decimals$} m").replace('.', tr!(",", "."))
}

/// Ce que fera « OK », en une phrase.
fn summary(s: &ShotSettings, who: Option<&str>, start: i32, fps: f32, has_keys: bool, fov: f32, dist: Option<f32>) -> String {
    let who = who.unwrap_or(tr!("(personnage à choisir)", "(character to choose)"));
    let lens = match dist {
        Some(d) => trf!("\nChamp de vision {fov:.0}°, à {} du personnage.", "\nField of view {fov:.0}°, {} from the character.", meters(d)),
        None => trf!("\nChamp de vision {fov:.0}°.", "\nField of view {fov:.0}°."),
    };
    let text = match s.choice {
        None => trf!(
            "Avec OK : la caméra de prise se place sur {who} ({}, {}, {}) à l'image {start}.",
            "With OK: the shot camera goes onto {who} ({}, {}, {}) at frame {start}.",
            s.framing.label().to_lowercase(),
            s.height.label().to_lowercase(),
            s.side.label().to_lowercase()
        ),
        Some(mv) => {
            let end = start + ((s.secs.max(0.1) * fps.max(1.0)).round() as i32).max(2);
            let erase = if !has_keys {
                ""
            } else if s.replace {
                tr!("\nLes anciennes clés de caméra seront effacées.", "\nThe old camera keys will be erased.")
            } else {
                tr!("\nLes clés de caméra hors de ces images sont gardées.", "\nCamera keys outside these frames are kept.")
            };
            trf!(
                "Avec OK : {} autour de {who}, en {:.1} s, de l'image {start} à {end}. \
                 Ensuite, Espace pour le voir.{erase}",
                "With OK: {} around {who}, over {:.1} s, from frame {start} to {end}. \
                 Then press Space to watch it.{erase}",
                plain(mv.label()).to_lowercase(),
                s.secs
            )
        }
    };
    text + &lens
}

fn combo<T: Copy + PartialEq>(ui: &mut egui::Ui, id: &str, value: &mut T, all: &[T], label: impl Fn(T) -> &'static str) {
    egui::ComboBox::from_id_salt(id).selected_text(label(*value)).width(190.0).show_ui(ui, |ui| {
        for &v in all {
            ui.selectable_value(value, v, label(v));
        }
    });
}
