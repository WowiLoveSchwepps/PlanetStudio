//! Fenêtre d'aide : prise en main pas à pas, souris et caméras, et réglage des raccourcis.
//!
//! Les textes utilisent un petit balisage : `{id}` affiche les touches de l'action `id`
//! (voir [`Action::id`]) telles que réglées, `[texte]` dessine une touche, `**texte**` met en gras.

use super::StudioApp;
use crate::keymap::{Action, Binding, Group, Keymap};
use eframe::egui::{self, Color32, RichText};

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum HelpTab {
    #[default]
    Start,
    Mouse,
    Keys,
}

#[derive(Default)]
pub(super) struct HelpState {
    pub open: bool,
    pub tab: HelpTab,
    /// raccourci en cours de modification : (action, emplacement 0 ou 1)
    pub capture: Option<(Action, usize)>,
}

impl HelpState {
    pub fn capturing(&self) -> bool {
        self.capture.is_some()
    }
}

impl StudioApp {
    pub(super) fn open_help(&mut self, tab: HelpTab) {
        self.help.open = true;
        self.help.tab = tab;
        self.help.capture = None;
    }

    /// Lit la nouvelle touche pendant la modification d'un raccourci. Appelée en tout début
    /// d'image : la touche est retirée des évènements et ne déclenche donc rien d'autre.
    pub(super) fn capture_binding(&mut self, ctx: &egui::Context) {
        let Some((action, slot)) = self.help.capture else { return };
        if !self.help.open || self.help.tab != HelpTab::Keys {
            self.help.capture = None;
            return;
        }
        let got = ctx.input_mut(|i| {
            let mut got = None;
            i.events.retain(|ev| match ev {
                egui::Event::Key { key, physical_key, pressed, repeat, modifiers } => {
                    if *pressed && !*repeat && got.is_none() {
                        got = Some((*key, *physical_key, *modifiers));
                    }
                    false
                }
                egui::Event::Text(_) => false,
                _ => true,
            });
            got
        });
        let Some((key, physical, mods)) = got else { return };
        self.help.capture = None;
        // le bouton garde sinon le focus clavier, et Espace le cliquerait de nouveau
        ctx.memory_mut(|m| {
            if let Some(id) = m.focused() {
                m.surrender_focus(id);
            }
        });
        if key == egui::Key::Escape {
            return;
        }
        let b = if action.is_fly() { Binding::key(physical.unwrap_or(key)) } else { Binding::from_mods(key, mods) };
        let stolen = self.config.keymap.set(action, slot, Some(b));
        self.config.save();
        let name = b.label(action.is_fly());
        self.status = match stolen {
            Some(o) => format!("{} : {name} (retirée de « {} »)", action.label(), o.label()),
            None => format!("{} : {name}", action.label()),
        };
    }

    pub(super) fn help_window(&mut self, ctx: &egui::Context) {
        if !self.help.open {
            return;
        }
        let mut open = true;
        egui::Window::new("Aide")
            .open(&mut open)
            .default_size([660.0, 580.0])
            .min_width(500.0)
            .min_height(300.0)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for (tab, name) in [
                        (HelpTab::Start, "📖 Prise en main"),
                        (HelpTab::Mouse, "🖱 Souris et caméras"),
                        (HelpTab::Keys, "⌨ Raccourcis"),
                    ] {
                        let r = ui.selectable_label(self.help.tab == tab, RichText::new(name).size(15.0));
                        if r.clicked() && self.help.tab != tab {
                            self.help.tab = tab;
                            self.help.capture = None;
                        }
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.add_space(4.0);
                    match self.help.tab {
                        HelpTab::Start => start_tab(ui, &self.config.keymap),
                        HelpTab::Mouse => mouse_tab(ui, &self.config.keymap),
                        HelpTab::Keys => self.keys_tab(ui),
                    }
                    ui.add_space(8.0);
                });
            });
        if !open {
            self.help.open = false;
            self.help.capture = None;
        }
    }

    fn keys_tab(&mut self, ui: &mut egui::Ui) {
        let km = &self.config.keymap;
        rich(ui, km, "Cliquez sur une touche, puis appuyez sur la nouvelle. [Échap] annule, un [Clic droit] l'efface.");
        ui.add_space(2.0);
        ui.label(
            RichText::new(
                "Les touches de déplacement sont lues par leur position : elles restent sous les mêmes \
                 doigts en AZERTY et en QWERTY. Les réglages sont enregistrés tout de suite.",
            )
            .weak(),
        );

        for group in Group::ALL {
            heading(ui, group.title());
            egui::Grid::new(("keymap", group as u8))
                .num_columns(4)
                .spacing([10.0, 6.0])
                .striped(true)
                .min_col_width(0.0)
                .show(ui, |ui| {
                    for a in Action::ALL.into_iter().filter(|a| a.group() == group) {
                        ui.add_sized([210.0, 20.0], egui::Label::new(a.label()).truncate());
                        self.slot_button(ui, a, 0);
                        self.slot_button(ui, a, 1);
                        let km = &mut self.config.keymap;
                        let r = ui
                            .add_enabled(!km.is_default(a), egui::Button::new("↺").small())
                            .on_hover_text("Rétablir la touche par défaut");
                        if r.clicked() {
                            km.reset(a);
                            self.config.save();
                            self.help.capture = None;
                        }
                        ui.end_row();
                    }
                });
        }

        ui.add_space(12.0);
        ui.separator();
        ui.horizontal(|ui| {
            let is_default = self.config.keymap == Keymap::default();
            if ui.add_enabled(!is_default, egui::Button::new("↺ Tout rétablir par défaut")).clicked() {
                self.config.keymap = Keymap::default();
                self.config.save();
                self.help.capture = None;
                self.status = "Raccourcis par défaut rétablis".into();
            }
        });
    }

    fn slot_button(&mut self, ui: &mut egui::Ui, a: Action, slot: usize) {
        let capturing = self.help.capture == Some((a, slot));
        let km = &self.config.keymap;
        let clash = km.clash(a, slot);
        let text = if capturing {
            RichText::new("Appuyez sur une touche…").italics()
        } else {
            match km.slot_label(a, slot) {
                Some(t) if clash.is_some() => RichText::new(format!("⚠ {t}")).monospace().color(ui.visuals().warn_fg_color),
                Some(t) => RichText::new(t).monospace(),
                None => RichText::new("—").weak(),
            }
        };
        let r = ui.add(egui::Button::new(text).selected(capturing).min_size(egui::vec2(150.0, 0.0)));
        let r = match clash {
            Some(o) => r.on_hover_text(format!("Cette touche sert aussi à « {} »", o.label())),
            None => r.on_hover_text("Clic : changer   ·   Clic droit : effacer"),
        };
        if r.clicked() {
            self.help.capture = if capturing { None } else { Some((a, slot)) };
            r.surrender_focus();
        }
        if r.secondary_clicked() {
            self.config.keymap.set(a, slot, None);
            self.config.save();
            self.help.capture = None;
        }
    }
}

// ------------------------------------------------------------------ onglets

fn start_tab(ui: &mut egui::Ui, km: &Keymap) {
    ui.label(
        RichText::new("Six étapes pour passer d'une scène vide à une vidéo. Les touches affichées suivent vos réglages.")
            .weak(),
    );
    ui.add_space(10.0);

    step(ui, km, 1, "Ajouter des modèles", "Cliquez sur **Créer le soldat**, ou double-cliquez sur un asset dans la liste de gauche.", &[
        "[Clic droit] sur un asset pour le **fusionner** (tête, casque) ou l'**attacher à un os** (une arme sur R_WEAPON).",
    ]);
    step(ui, km, 2, "Sélectionner un os", "[Clic gauche] sur une articulation dans la vue, ou sur un os dans l'arbre à droite.", &[]);
    step(ui, km, 3, "Poser", "Tournez l'os avec le gizmo, ou tapez ses angles dans l'inspecteur.", &[
        "Anneaux rouge, vert et bleu : axes locaux. Anneau gris : rotation libre.",
        "Gizmo de rotation {gizmo_rotate}, de translation {gizmo_translate}.",
        "Chaque modification pose une clé à l'image courante. {set_key} en pose une à la main.",
    ]);
    step(ui, km, 4, "Animer", "Changez d'image dans la timeline, puis reposez : la lecture interpole entre les clés.", &[
        "Image précédente {prev_frame}, suivante {next_frame}, lecture {play_pause}.",
        "Glissez un losange pour déplacer une clé, [Clic droit] dessus pour la supprimer.",
    ]);
    step(ui, km, 5, "Filmer", "La **caméra de prise** 🎥 filme, la **caméra de travail** 🛠 sert à poser. {toggle_camera} passe de l'une à l'autre.", &[
        "En vue Prise, le vol (clic droit + {fly_forward} {fly_left} {fly_back} {fly_right}), la molette, l'orbite et le cadrage {frame_selection} pilotent la caméra de prise. Si elle a des clés, chaque mouvement en pose une.",
        "**📷 Copier la vue** (timeline) place la caméra de prise sur la vue de travail.",
        "Animer la caméra : à chaque image voulue, cadrez puis **🔑 Clé** (Prise) ou {set_key} sans acteur sélectionné. En vue Travail, la clé reprend la vue de travail.",
        "Le moniteur en haut à droite montre ce qu'elle filme : cliquez dessus pour y passer.",
    ]);
    step(ui, km, 6, "Exporter", "{photo} ou Fichier > Photo : l'image courante en PNG.", &[
        "Fichier > Exporter : vidéo MP4 ou séquence PNG.",
        "{save} enregistre le projet. {undo} annule, {redo} rétablit.",
        "Le projet s'enregistre aussi tout seul (toutes les 2 min et à la fermeture) : réglage dans le menu Fichier.",
    ]);
}

fn mouse_tab(ui: &mut egui::Ui, km: &Keymap) {
    heading(ui, "🚁 Vol libre (drone)");
    ui.label(
        RichText::new(
            "La caméra se pilote comme un drone ou la caméra d'observateur d'un jeu : on regarde avec la              souris en tenant le clic droit, et on vole au clavier.",
        )
        .weak(),
    );
    ui.add_space(4.0);
    table(ui, km, "fly", &[
        ("Regarder autour", "[Clic droit] tenu (le curseur disparaît)"),
        ("Avancer / reculer", "{fly_forward} / {fly_back}"),
        ("Gauche / droite", "{fly_left} / {fly_right}"),
        ("Monter / descendre", "{fly_up_rmb} / {fly_down_rmb} en tenant le clic droit, ou {fly_up} / {fly_down}"),
        ("Plus vite", "[Maj] tenue"),
        ("Régler la vitesse", "[Clic droit] + [Molette], ou le menu 🚁 de la vue"),
        ("Avancer d'un cran", "[Molette]"),
    ]);
    ui.add_space(4.0);
    ui.label(
        RichText::new(
            "On avance dans la direction du regard. Le menu 🚁 en haut de la vue règle aussi la              sensibilité de la souris, l'inertie (accélération et freinage en douceur) et l'inversion verticale.",
        )
        .weak(),
    );

    heading(ui, "🖱 Autour de la sélection");
    table(ui, km, "mouse", &[
        ("Orbiter autour du point visé", "[Alt] + [Clic gauche]"),
        ("Glisser la vue", "[Clic milieu], ou [Alt] + [Maj] + [Clic gauche]"),
        ("Sélectionner un os", "[Clic gauche] sur une articulation"),
        ("Déplacer un personnage au sol", "[Clic gauche] tenu sur son corps, puis glisser ([Ctrl] : aimanté à la grille)"),
        ("Cadrer la sélection", "{frame_selection} (le point d'orbite passe sur elle)"),
        ("Copier la pose du personnage", "[Ctrl] + [C], ou clic droit sur lui dans la liste Scène"),
        ("Coller la pose sur un autre", "[Ctrl] + [V] ([Ctrl] + [Maj] + [V] : en miroir)"),
        ("Placer un personnage au même endroit qu'un autre", "Copier la pose du premier, puis clic droit > Coller la position"),
        ("Tourner la grille au sol", "Affichage > Grille > Rotation"),
    ]);

    heading(ui, "🎥 Les deux caméras");
    bullet(ui, km, "**Caméra de travail** 🛠 : libre, pour poser et régler la scène. Elle n'est jamais rendue.");
    bullet(ui, km, "**Caméra de prise** 🎥 : celle de la photo et de la vidéo. Fixe au départ, elle devient animée dès qu'on lui pose une clé.");
    bullet(ui, km, "{toggle_camera} bascule de l'une à l'autre. En vue Prise, souris et clavier pilotent la caméra de prise.");
}

// ------------------------------------------------------------------ mise en forme

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(10.0);
    ui.label(RichText::new(text).strong().size(16.0));
    ui.add_space(4.0);
}

/// Étape numérotée : pastille, titre, phrase principale et détails à puces.
fn step(ui: &mut egui::Ui, km: &Keymap, n: u32, title: &str, lead: &str, details: &[&str]) {
    ui.horizontal_top(|ui| {
        let accent = ui.visuals().selection.bg_fill;
        egui::Frame::new().fill(accent).corner_radius(11.0).inner_margin(egui::Margin::symmetric(7, 2)).show(ui, |ui| {
            ui.label(RichText::new(n.to_string()).strong().size(14.0).color(Color32::WHITE));
        });
        ui.add_space(4.0);
        ui.vertical(|ui| {
            ui.label(RichText::new(title).strong().size(15.5));
            ui.add_space(2.0);
            rich(ui, km, lead);
            for d in details {
                bullet(ui, km, d);
            }
        });
    });
    ui.add_space(12.0);
}

fn bullet(ui: &mut egui::Ui, km: &Keymap, text: &str) {
    ui.horizontal_top(|ui| {
        ui.label(RichText::new("•").weak());
        ui.vertical(|ui| rich(ui, km, text));
    });
}

/// Tableau à deux colonnes : action, contrôle.
fn table(ui: &mut egui::Ui, km: &Keymap, id: &str, rows: &[(&str, &str)]) {
    egui::Grid::new(id).num_columns(2).spacing([24.0, 8.0]).striped(true).show(ui, |ui| {
        for (what, how) in rows {
            ui.label(*what);
            rich(ui, km, how);
            ui.end_row();
        }
    });
}

/// Une touche dessinée comme sur un clavier.
fn keycap(ui: &mut egui::Ui, text: &str) {
    let v = ui.visuals();
    let fill = v.widgets.inactive.weak_bg_fill;
    let stroke = egui::Stroke::new(1.0_f32, v.widgets.noninteractive.bg_stroke.color);
    ui.add_space(1.0);
    egui::Frame::new().fill(fill).stroke(stroke).corner_radius(4.0).inner_margin(egui::Margin::symmetric(5, 0)).show(
        ui,
        |ui| {
            ui.add(egui::Label::new(RichText::new(text).monospace().strong()).extend());
        },
    );
    ui.add_space(1.0);
}

/// Texte avec touches en ligne (voir le balisage en tête de fichier).
fn rich(ui: &mut egui::Ui, km: &Keymap, text: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let mut rest = text;
        let mut bold = false;
        loop {
            let cut = rest
                .char_indices()
                .find(|&(i, c)| c == '{' || c == '[' || rest[i..].starts_with("**"))
                .map(|(i, _)| i);
            let (plain, tail) = rest.split_at(cut.unwrap_or(rest.len()));
            if !plain.is_empty() {
                let t = RichText::new(plain);
                ui.label(if bold { t.strong() } else { t });
            }
            if tail.is_empty() {
                break;
            }
            if let Some(t) = tail.strip_prefix("**") {
                bold = !bold;
                rest = t;
                continue;
            }
            let close = if tail.starts_with('{') { '}' } else { ']' };
            let Some(end) = tail.find(close) else {
                ui.label(tail);
                break;
            };
            let inner = &tail[1..end];
            if close == ']' {
                keycap(ui, inner);
            } else if let Some(a) = Action::from_id(inner) {
                action_keys(ui, km, a);
            } else {
                ui.label(inner);
            }
            rest = &tail[end + 1..];
        }
    });
}

fn action_keys(ui: &mut egui::Ui, km: &Keymap, a: Action) {
    let labels: Vec<String> = (0..2).filter_map(|s| km.slot_label(a, s)).collect();
    if labels.is_empty() {
        ui.label(RichText::new("(aucune touche)").weak().italics());
    }
    for (i, l) in labels.iter().enumerate() {
        if i > 0 {
            ui.label(" ou ");
        }
        keycap(ui, l);
    }
}
