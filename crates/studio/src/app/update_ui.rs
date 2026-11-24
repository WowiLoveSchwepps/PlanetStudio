//! Bandeau « nouvelle version disponible » et options de mise à jour (menu Aide).

use super::StudioApp;
use crate::update::{self, State};
use eframe::egui::{self, Color32};
use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;

/// Réglages de mise à jour, enregistrés dans la configuration.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct UpdateSettings {
    /// vérifier au démarrage s'il existe une version plus récente (sans rien installer)
    pub check_on_start: bool,
    /// version dont l'utilisateur ne veut pas (« Ignorer cette version »)
    pub skipped: Option<String>,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self { check_on_start: true, skipped: None }
    }
}

impl StudioApp {
    /// Vérification au démarrage, si l'utilisateur ne l'a pas désactivée.
    pub(super) fn update_on_start(&self, ctx: &egui::Context) {
        if self.config.update.check_on_start && !update::is_source_build() {
            self.updater.check(ctx, false);
        }
    }

    /// Entrées du menu Aide.
    pub(super) fn update_menu(&mut self, ui: &mut egui::Ui) {
        ui.separator();
        ui.label(egui::RichText::new(format!("PlanetStudio {}", update::CURRENT)).weak());
        ui.hyperlink_to(
            tr!("Logiciel libre, licence GPL-3.0 (sans garantie)", "Free software, GPL-3.0 license (no warranty)"),
            format!("https://github.com/{}/blob/main/LICENSE", update::REPO),
        );
        let source = update::is_source_build();
        let resp = ui.add_enabled(!source && !self.updater.busy(), egui::Button::new(tr!("🔄 Rechercher des mises à jour", "🔄 Check for updates")));
        let resp = if source {
            resp.on_disabled_hover_text(tr!("Version compilée depuis les sources : mets-la à jour avec git", "Built from source: update it with git"))
        } else {
            resp
        };
        if resp.clicked() {
            self.update_hidden = false;
            self.updater.check(ui.ctx(), true);
            ui.close_menu();
        }
        if ui
            .checkbox(&mut self.config.update.check_on_start, tr!("Vérifier au démarrage", "Check on startup"))
            .on_hover_text(tr!("Demande à GitHub s'il existe une version plus récente. Rien ne s'installe sans ton accord.", "Asks GitHub whether a newer version exists. Nothing is installed without your consent."))
            .changed()
        {
            self.config.save();
        }
    }

    /// Bandeau sous la barre de menus, affiché seulement quand il y a quelque chose à dire.
    pub(super) fn update_banner(&mut self, ctx: &egui::Context) {
        let state = self.updater.state();
        let show = match &state {
            State::Idle | State::Checking { manual: false } | State::UpToDate { manual: false } => false,
            State::Failed { manual, .. } => *manual,
            State::Available(r) => !self.update_hidden && self.config.update.skipped.as_deref() != Some(r.version.as_str()),
            _ => true,
        };
        if !show {
            return;
        }
        let accent = Color32::from_rgb(70, 140, 220);
        egui::TopBottomPanel::top("update_banner")
            .frame(egui::Frame::side_top_panel(&ctx.style()).fill(Color32::from_rgb(28, 40, 56)))
            .show(ctx, |ui| {
                ui.add_space(3.0);
                match state {
                    State::Checking { .. } => {
                        ui.horizontal(|ui| {
                            ui.add(egui::Spinner::new().size(12.0));
                            ui.label(tr!("Recherche de mises à jour…", "Checking for updates…"));
                        });
                    }
                    State::UpToDate { .. } => {
                        ui.horizontal(|ui| {
                            ui.label(trf!("✔ PlanetStudio {} est à jour.", "✔ PlanetStudio {} is up to date.", update::CURRENT));
                            if ui.button("OK").clicked() {
                                self.updater.set_idle();
                            }
                        });
                    }
                    State::Available(rel) => {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(trf!("⬆ PlanetStudio {} est disponible", "⬆ PlanetStudio {} is available", rel.version))
                                    .color(accent)
                                    .strong(),
                            );
                            ui.label(egui::RichText::new(trf!("(tu as la {})", "(you have {})", update::CURRENT)).weak());
                            if ui.button(tr!("Mettre à jour", "Update")).clicked() {
                                self.updater.install(rel.clone(), ctx);
                            }
                            if ui.button(tr!("Plus tard", "Later")).on_hover_text(tr!("Le bandeau reviendra au prochain démarrage", "The banner will come back on next startup")).clicked() {
                                self.update_hidden = true;
                            }
                            if ui.button(tr!("Ignorer cette version", "Skip this version")).clicked() {
                                self.config.update.skipped = Some(rel.version.clone());
                                self.config.save();
                            }
                            if ui.link(tr!("Page GitHub", "GitHub page")).clicked() {
                                ctx.open_url(egui::OpenUrl::new_tab(&rel.page));
                            }
                        });
                        if !rel.notes.is_empty() {
                            egui::CollapsingHeader::new(tr!("Notes de version", "Release notes")).id_salt("update_notes").show(ui, |ui| {
                                egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
                                    ui.label(&rel.notes);
                                });
                            });
                        }
                    }
                    State::Downloading(rel) => {
                        let p = &self.updater.progress;
                        let bytes = p.bytes.load(Ordering::Relaxed);
                        let stage = p.stage.load(Ordering::Relaxed);
                        ui.horizontal(|ui| {
                            ui.add(egui::Spinner::new().size(12.0));
                            let text = match stage {
                                0 => trf!(
                                    "Téléchargement de PlanetStudio {} : {:.1} Mo",
                                    "Downloading PlanetStudio {}: {:.1} MB",
                                    rel.version,
                                    bytes as f64 / 1_048_576.0
                                ),
                                1 => tr!("Vérification du fichier…", "Checking the file…").to_string(),
                                _ => tr!("Installation…", "Installing…").to_string(),
                            };
                            ui.label(text);
                            if stage == 0 && ui.button(tr!("Annuler", "Cancel")).clicked() {
                                self.updater.cancel();
                            }
                        });
                        ctx.request_repaint_after(std::time::Duration::from_millis(150));
                    }
                    State::Installed(version) => {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(trf!("✔ PlanetStudio {version} est installé.", "✔ PlanetStudio {version} is installed."))
                                    .color(accent)
                                    .strong(),
                            );
                            ui.label(tr!("Il sera utilisé au prochain démarrage.", "It will be used on next startup."));
                            if ui
                                .button(tr!("Redémarrer maintenant", "Restart now"))
                                .on_hover_text(tr!("Ferme PlanetStudio (le projet est enregistré si l'enregistrement automatique est actif) et le relance", "Closes PlanetStudio (the project is saved if autosave is on) and starts it again"))
                                .clicked()
                            {
                                self.restart_after_exit = true;
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                            if ui.button(tr!("Plus tard", "Later")).clicked() {
                                self.updater.set_idle();
                            }
                        });
                    }
                    State::Failed { message, .. } => {
                        ui.horizontal(|ui| {
                            ui.colored_label(Color32::from_rgb(255, 120, 100), trf!("Mise à jour impossible : {message}", "Update failed: {message}"));
                            if ui.button(tr!("Fermer", "Close")).clicked() {
                                self.updater.set_idle();
                            }
                        });
                    }
                    State::Idle => {}
                }
                ui.add_space(3.0);
            });
    }
}
