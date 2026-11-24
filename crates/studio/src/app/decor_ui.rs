//! Interface du décor : choix d'une carte et d'un point sur sa vue d'ensemble, chargement
//! des objets autour de ce point.

use super::StudioApp;
use crate::decor::{DecorData, DecorSettings, ZONES};
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use forgelight::zone::Overview;
use std::sync::atomic::Ordering;

pub(super) struct DecorState {
    /// décor affiché (ou en cours de chargement)
    pub applied: Option<DecorSettings>,
    /// réglages en cours d'édition dans la fenêtre
    pub draft: DecorSettings,
    /// hauteur du sol estimée au chargement
    pub ground_auto: bool,
    pub request: u64,
    pub loading: bool,
    pub open: bool,
    /// vue d'ensemble de la carte du brouillon : (carte, texture, repère)
    overview: Option<(String, egui::TextureHandle, Overview)>,
    overview_error: Option<(String, String)>,
    /// grossissement de la vue d'ensemble (1 = carte entière)
    zoom: f32,
    /// centre de la vue d'ensemble (UV) quand l'utilisateur l'a déplacée ; `None` = centrée
    /// sur le lieu
    view: Option<[f32; 2]>,
    pub info: String,
    /// au prochain décor chargé, régler le soleil d'après son ciel (chargement depuis la fenêtre,
    /// pas à l'ouverture d'un projet qui a déjà son soleil)
    pub sun_on_load: bool,
    /// effet de particules dont les textures ont déjà été demandées (pas de nouvel essai à
    /// chaque image si l'une manque)
    particles_prepared: Option<crate::particles::Kind>,
    /// flore du sol du décor affiché, posée autour des caméras
    pub flora: Option<crate::decor::GroundFlora>,
}

impl Default for DecorState {
    fn default() -> Self {
        Self {
            applied: None,
            draft: DecorSettings::default(),
            ground_auto: true,
            request: 0,
            loading: false,
            open: false,
            overview: None,
            overview_error: None,
            zoom: 1.0,
            view: None,
            info: String::new(),
            sun_on_load: false,
            particles_prepared: None,
            flora: None,
        }
    }
}

impl StudioApp {
    /// Lance le chargement d'un décor (remplace le précédent à son arrivée).
    pub(super) fn request_decor(&mut self, settings: DecorSettings) {
        let Some(game) = self.game.clone() else {
            self.status = tr!("Ouvrez d'abord le dossier du jeu", "Open the game folder first").into();
            return;
        };
        self.decor.request += 1;
        self.decor.loading = true;
        self.decor.applied = Some(settings.clone());
        self.status = trf!("Chargement du décor de {}…", "Loading the {} scenery…", settings.zone);
        self.loader.load_decor(game, settings, self.decor.request, self.renderer.texture_names());
    }

    pub(super) fn clear_decor(&mut self) {
        self.decor.request += 1;
        self.decor.loading = false;
        self.decor.applied = None;
        self.decor.info.clear();
        self.decor.flora = None;
        self.renderer.clear_decor();
        self.force_render = true;
    }

    pub(super) fn on_decor_loaded(&mut self, request: u64, result: Result<DecorData, String>) {
        if request != self.decor.request {
            return; // remplacé entre-temps
        }
        self.decor.loading = false;
        match result {
            Ok(mut data) => {
                crate::decor::upload(&mut self.renderer, &data);
                self.decor.flora = data.ground_flora.take();
                if std::mem::take(&mut self.decor.sun_on_load) && data.settings.sun_follows {
                    if let Some(sun) = data.sky.as_ref().and_then(|k| k.sun.clone()) {
                        self.apply_sky_sun(&sun);
                    }
                }
                let s = &data.settings;
                self.decor.info = trf!(
                    "{} objets, {} modèles, {} textures ; sol à {:.1} m",
                    "{} objects, {} models, {} textures; ground at {:.1} m",
                    data.instances.len(),
                    data.models.len(),
                    data.textures.len(),
                    s.ground.unwrap_or(0.0)
                );
                if !data.failed.is_empty() {
                    self.decor.info += &trf!(" ; {} acteurs illisibles", "; {} unreadable actors", data.failed.len());
                }
                self.status = trf!("Décor de {} chargé : {}", "{} scenery loaded: {}", s.zone, self.decor.info);
                // la hauteur estimée devient celle du brouillon (pour l'ajuster)
                if self.decor.draft.zone == s.zone && self.decor.draft.center == s.center {
                    self.decor.draft.ground = s.ground;
                }
                self.decor.applied = Some(data.settings);
            }
            Err(e) => {
                self.decor.applied = None;
                self.status = trf!("Impossible de charger le décor : {e}", "Could not load the scenery: {e}");
            }
        }
    }

    /// Pose la flore du sol autour de ces caméras (positions du studio) quand elles se sont
    /// éloignées de celle qui est affichée. La vue de travail compte toujours : elle reste à
    /// l'écran pendant les exports, et la flore ne fait pas ainsi l'aller-retour à chaque image.
    pub(super) fn follow_flora(&mut self, shot_eye: Option<glam::Vec3>) {
        let eyes: Vec<glam::Vec3> = std::iter::once(self.work_view().eye).chain(shot_eye).collect();
        let (Some(flora), Some(game)) = (self.decor.flora.as_mut(), self.game.as_ref()) else { return };
        if let Some(instances) = flora.update(game, &eyes) {
            self.renderer.set_flora(&instances);
        }
    }

    pub(super) fn decor_menu(&mut self, ui: &mut egui::Ui) {
        if ui.button(tr!("🗺 Choisir une carte et un lieu…", "🗺 Choose a map and a place…")).clicked() {
            if let Some(a) = &self.decor.applied {
                self.decor.draft = a.clone();
                self.decor.ground_auto = false;
            }
            self.decor.open = true;
            ui.close_menu();
        }
        let mut visible = self.renderer.decor_visible;
        if ui.add_enabled(self.decor.applied.is_some(), egui::Checkbox::new(&mut visible, tr!("Afficher le décor", "Show scenery"))).changed() {
            self.renderer.decor_visible = visible;
            if let Some(a) = &mut self.decor.applied {
                a.visible = visible;
            }
            self.force_render = true;
        }
        self.sky_controls(ui);
        self.particles_controls(ui);
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(tr!("Ombres du décor sur", "Scenery shadows over"));
            ui.add(crate::calc::drag(&mut self.renderer.decor_shadow_range).range(10.0..=400.0).speed(1.0).suffix(" m"))
                .on_hover_text(
                    tr!("Demi-côté de la zone où le décor reçoit les ombres, autour des personnages. \
                     Plus grand : ombres plus lointaines mais moins précises.", "Half-width of the area where the scenery casts shadows, around the characters. Larger: shadows reach farther but are less precise."),
                );
        });
        if ui.add_enabled(self.decor.applied.is_some(), egui::Button::new(tr!("Retirer le décor", "Remove scenery"))).clicked() {
            self.clear_decor();
            ui.close_menu();
        }
    }

    /// Met le ciel de la carte à une heure, le soleil (ou la lune) et l'ambiance suivant le jeu.
    pub(super) fn set_sky_hour(&mut self, hour: f32) {
        let Some(a) = &self.decor.applied else { return };
        let mut s = a.clone();
        (s.sky, s.sky_hour, s.sun_follows) = (true, hour.rem_euclid(24.0), true);
        self.apply_sky_settings(s, true);
    }

    /// Ciel de la carte : affiché ou non, heure (dôme, et soleil s'il la suit), brouillard.
    /// S'applique tout de suite.
    fn sky_controls(&mut self, ui: &mut egui::Ui) {
        let Some(a) = &self.decor.applied else {
            ui.add_enabled(false, egui::Checkbox::new(&mut true, tr!("Ciel de la carte", "Map sky")));
            return;
        };
        let mut s = a.clone();
        let (mut reload, mut fog_changed) = (false, false);
        ui.horizontal(|ui| {
            reload |= ui
                .checkbox(&mut s.sky, tr!("Ciel de la carte", "Map sky"))
                .on_hover_text(tr!("Dôme du ciel peint par le jeu pour ce continent, derrière le décor", "Sky dome painted by the game for this continent, behind the scenery"))
                .changed();
            let h = ui.add_enabled(
                s.sky,
                egui::Slider::new(&mut s.sky_hour, 0.0..=24.0)
                    .step_by(0.5)
                    .custom_formatter(|v, _| format!("{}h{:02}", v as u32, ((v.fract()) * 60.0).round() as u32))
                    .text(tr!("heure", "hour")),
            );
            // le ciel est relu (fichier, texture) à la fin du glisser seulement
            reload |= h.drag_stopped() || (h.changed() && !h.dragged());
        });
        ui.add_enabled_ui(s.sky, |ui| {
            reload |= ui
                .checkbox(&mut s.sun_follows, tr!("Le soleil suit l'heure", "The sun follows the hour"))
                .on_hover_text(
                    tr!(
                        "Direction, couleur et force du soleil (la lune la nuit) et lumière ambiante \
                         prises dans le ciel du jeu à cette heure. Décochez pour régler le soleil à la main.",
                        "Sun direction, color and strength (the moon at night) and ambient light \
                         taken from the game's sky at this hour. Uncheck to set the sun by hand."
                    ),
                )
                .changed();
            ui.horizontal(|ui| {
                fog_changed |= ui
                    .checkbox(&mut s.fog, tr!("Brouillard", "Fog"))
                    .on_hover_text(
                        tr!(
                            "Brouillard du jeu : plus dense près du sol et au loin ; le décor lointain se \
                             fond dans le ciel là où s'arrête le terrain",
                            "The game's fog: denser near the ground and in the distance; far scenery \
                             fades into the sky where the terrain ends"
                        ),
                    )
                    .changed();
                fog_changed |= ui
                    .add_enabled(
                        s.fog,
                        egui::Slider::new(&mut s.fog_density, 0.0..=4.0).text(tr!("densité", "density")).custom_formatter(|v, _| format!("{v:.2}×")),
                    )
                    .changed();
            });
            ui.horizontal(|ui| {
                fog_changed |= ui
                    .checkbox(&mut s.sun_rays, tr!("Rayons du soleil", "Sun rays"))
                    .on_hover_text(
                        tr!(
                            "Brouillard ombré : l'air éclairé par le soleil s'illumine, coupé par l'ombre des \
                             arbres, des bâtiments et des personnages (surtout face au soleil). Seulement dans la \
                             zone des ombres du décor.",
                            "Shadowed fog: sunlit air glows, cut by the shadows of trees, buildings \
                             and characters (mostly when facing the sun). Only within the \
                             scenery's shadow area."
                        ),
                    )
                    .changed();
                fog_changed |= ui
                    .add_enabled(
                        s.sun_rays,
                        egui::Slider::new(&mut s.sun_rays_strength, 0.0..=4.0)
                            .text(tr!("force", "strength"))
                            .custom_formatter(|v, _| format!("{v:.2}×")),
                    )
                    .changed();
            });
        });
        ui.horizontal(|ui| {
            fog_changed |= ui
                .checkbox(&mut s.lamps, tr!("Lampes du décor", "Scenery lamps"))
                .on_hover_text(
                    tr!(
                        "Lampes, néons et projecteurs placés par le jeu sur la carte : ils éclairent le sol, \
                         les bâtiments et les personnages autour d'eux (surtout visibles la nuit). Les plus \
                         proches de la caméra seulement (128), sans ombre.",
                        "Lamps, neons and floodlights placed by the game on the map: they light the ground, \
                         buildings and characters around them (mostly visible at night). Only the ones \
                         closest to the camera (128), without shadows."
                    ),
                )
                .changed();
            fog_changed |= ui
                .add_enabled(
                    s.lamps,
                    egui::Slider::new(&mut s.lamps_strength, 0.0..=4.0).text(tr!("force", "strength")).custom_formatter(|v, _| format!("{v:.2}×")),
                )
                .changed();
        });
        if !reload && !fog_changed && Some(&s) == self.decor.applied.as_ref() {
            return;
        }
        self.apply_sky_settings(s, reload);
    }

    /// Applique des réglages du ciel ; `reload` relit le ciel (heure, affichage, soleil qui suit).
    fn apply_sky_settings(&mut self, s: crate::decor::DecorSettings, reload: bool) {
        // le brouillon suit (rechargement depuis la fenêtre)
        let d = &mut self.decor.draft;
        (d.sky, d.sky_hour, d.sun_follows, d.fog, d.fog_density) = (s.sky, s.sky_hour, s.sun_follows, s.fog, s.fog_density);
        (d.sun_rays, d.sun_rays_strength) = (s.sun_rays, s.sun_rays_strength);
        (d.lamps, d.lamps_strength) = (s.lamps, s.lamps_strength);
        self.decor.applied = Some(s.clone());
        self.renderer.set_fog_settings(&s);
        self.force_render = true;
        if !reload {
            return;
        }
        let sky = match (&self.game, s.sky) {
            (Some(game), true) => crate::decor::load_sky(game, &s),
            _ => None,
        };
        if let Some(k) = &sky {
            self.status = trf!("Ciel : {}", "Sky: {}", k.file);
        }
        self.renderer.set_sky(sky.as_ref());
        if s.sun_follows {
            if let Some(sun) = sky.as_ref().and_then(|k| k.sun.as_ref()) {
                self.apply_sky_sun(sun);
            }
        }
    }

    /// Continent dont « Auto » prend les particules : celui du décor affiché.
    fn particles_zone(&self) -> Option<&str> {
        self.decor.applied.as_ref().filter(|_| self.renderer.decor_visible).map(|a| a.zone.as_str())
    }

    /// Transmet l'effet de particules au rendu, en chargeant ses textures au besoin.
    pub(super) fn sync_particles(&mut self) {
        let kind = self.particles.kind.resolve(self.particles_zone());
        if kind.is_some() && self.decor.particles_prepared != kind {
            if let Some(game) = self.game.clone() {
                crate::particles::prepare(&mut self.renderer, &game, kind);
                self.decor.particles_prepared = kind;
            }
        }
        self.renderer.set_particles(kind, &self.particles);
    }

    /// Particules d'ambiance : effet (automatique selon le continent), densité, vent, taille.
    /// Elles suivent le temps de la timeline : lancez la lecture pour les voir bouger.
    fn particles_controls(&mut self, ui: &mut egui::Ui) {
        use crate::particles::Kind;
        ui.separator();
        let before = self.particles.clone();
        let p = &mut self.particles;
        let auto = Kind::Auto.resolve(self.decor.applied.as_ref().map(|a| a.zone.as_str()));
        let label = |k: Kind| match (k, auto) {
            (Kind::Auto, Some(a)) => trf!("Auto : {}", "Auto: {}", a.label()),
            (Kind::Auto, None) => tr!("Auto (rien sur ce continent)", "Auto (nothing on this continent)").to_string(),
            _ => k.label().to_string(),
        };
        ui.horizontal(|ui| {
            ui.label(tr!("Particules", "Particles"));
            egui::ComboBox::from_id_salt("particules").selected_text(label(p.kind)).width(230.0).show_ui(ui, |ui| {
                for k in Kind::ALL {
                    ui.selectable_value(&mut p.kind, k, label(k));
                }
            });
        })
        .response
        .on_hover_text(
            tr!(
                "Effets du jeu : feuilles qui tombent sur Hossin, vent de sable sur Indar, neige sur Esamir. \
                 « Auto » prend celui du continent du décor. Elles suivent la timeline : lancez la lecture \
                 pour les voir bouger ; l'export vidéo montre exactement les mêmes.",
                "Game effects: falling leaves on Hossin, sandstorm on Indar, snow on Esamir. \
                 “Auto” uses the one of the scenery's continent. They follow the timeline: start playback \
                 to see them move; the video export shows exactly the same ones."
            ),
        );
        let active = p.kind.resolve(self.decor.applied.as_ref().map(|a| a.zone.as_str())).is_some();
        ui.add_enabled_ui(active, |ui| {
            ui.add(egui::Slider::new(&mut p.density, 0.0..=4.0).text(tr!("densité", "density")).custom_formatter(|v, _| format!("{v:.2}×")));
            ui.add(egui::Slider::new(&mut p.wind, 0.0..=4.0).text(tr!("vent", "wind")).custom_formatter(|v, _| format!("{v:.2}×")))
                .on_hover_text(tr!("Force du vent (1× : celle de l'effet ; 0 : air calme, les flocons tombent droit)", "Wind strength (1×: the effect's own; 0: still air, flakes fall straight)"));
            ui.add(egui::Slider::new(&mut p.wind_dir, 0.0..=360.0).step_by(5.0).text(tr!("direction du vent", "wind direction")).suffix("°"))
                .on_hover_text(tr!("Direction vers laquelle souffle le vent, dans le plan du sol (0° : +X, 90° : +Z)", "Direction the wind blows toward, in the ground plane (0°: +X, 90°: +Z)"));
            ui.add(egui::Slider::new(&mut p.size, 0.25..=4.0).logarithmic(true).text(tr!("taille", "size")).custom_formatter(|v, _| format!("{v:.2}×")));
        });
        if self.particles != before {
            self.force_render = true;
        }
    }

    /// Place l'altitude `ground` du jeu sur la grille, en direct, si le décor affiché est chargé au
    /// lieu du brouillon (sinon le réglage attend « Charger le décor »).
    fn lift_decor(&mut self, ground: f32) {
        if self.decor.loading {
            return;
        }
        let d = &self.decor.draft;
        let Some(a) = self.decor.applied.as_mut().filter(|a| a.zone == d.zone && a.center == d.center) else { return };
        a.ground = Some(ground);
        self.renderer.set_decor_ground(ground);
        let at = tr!("sol à ", "ground at ");
        self.decor.info = match self.decor.info.rsplit_once(at) {
            Some((head, tail)) => {
                let rest = tail.find(" m").map_or("", |i| &tail[i + 2..]);
                format!("{head}{at}{ground:.2} m{rest}")
            }
            None => self.decor.info.clone(),
        };
        self.force_render = true;
    }

    /// Règle le soleil et l'ambiance d'après le ciel du jeu (clé à l'image courante s'ils sont animés).
    pub(super) fn apply_sky_sun(&mut self, sun: &crate::sky::Sun) {
        self.set_sun(sun.state());
        let mut amb = self.amb_at(self.frame);
        amb.intensity = sun.ambient();
        self.set_amb(amb);
        self.editing = false;
    }

    fn ensure_overview(&mut self, ctx: &egui::Context) {
        let zone = self.decor.draft.zone.clone();
        if self.decor.overview.as_ref().is_some_and(|o| o.0 == zone)
            || self.decor.overview_error.as_ref().is_some_and(|e| e.0 == zone)
        {
            return;
        }
        let Some(game) = &self.game else { return };
        match crate::decor::build_overview(game, &zone) {
            Ok(img) => {
                let size = [img.overview.size as usize; 2];
                let tex = ctx.load_texture(
                    format!("carte_{zone}"),
                    egui::ColorImage::from_rgba_unmultiplied(size, &img.rgba),
                    egui::TextureOptions::LINEAR,
                );
                self.decor.overview = Some((zone, tex, img.overview));
                self.decor.overview_error = None;
            }
            Err(e) => self.decor.overview_error = Some((zone, format!("{e:#}"))),
        }
    }

    pub(super) fn decor_window(&mut self, ctx: &egui::Context) {
        if !self.decor.open {
            return;
        }
        if self.game.is_none() {
            self.decor.open = false;
            self.status = tr!("Ouvrez d'abord le dossier du jeu", "Open the game folder first").into();
            return;
        }
        self.ensure_overview(ctx);
        let mut open = true;
        let mut load = false;
        egui::Window::new(tr!("🗺 Décor d'une carte", "🗺 Map scenery")).id(egui::Id::new("decor_window")).open(&mut open).resizable(false).show(ctx, |ui| {
            ui.label(
                egui::RichText::new(
                    tr!("Bâtiments, props, rochers et végétation placés par le jeu. Cliquez sur la carte pour \
                     choisir le lieu : il devient l'origine de la scène (là où sont les personnages).", "Buildings, props, rocks and vegetation placed by the game. Click the map to choose the place: it becomes the origin of the scene (where the characters are)."),
                )
                .small()
                .weak(),
            );
            ui.horizontal(|ui| {
                ui.label(tr!("Carte", "Map"));
                let label = ZONES.iter().find(|z| z.0 == self.decor.draft.zone).map_or("?", |z| crate::decor::zone_label(z.1));
                egui::ComboBox::from_id_salt("decor_zone").selected_text(label).show_ui(ui, |ui| {
                    for (k, l) in ZONES {
                        if ui.selectable_label(self.decor.draft.zone == *k, crate::decor::zone_label(l)).clicked() && self.decor.draft.zone != *k {
                            self.decor.draft.zone = k.to_string();
                            self.decor.draft.center = [0.0, 0.0];
                            self.decor.ground_auto = true;
                            self.decor.zoom = 1.0;
                            self.decor.view = None;
                        }
                    }
                });
                ui.label("Zoom");
                ui.add(egui::Slider::new(&mut self.decor.zoom, 1.0..=16.0).logarithmic(true).suffix("×"));
            });
            self.overview_widget(ui);
            ui.horizontal(|ui| {
                ui.label(tr!("Lieu (x, z du jeu)", "Place (game x, z)"));
                let a = ui.add(crate::calc::drag(&mut self.decor.draft.center[0]).speed(1.0).suffix(" m"));
                let b = ui.add(crate::calc::drag(&mut self.decor.draft.center[1]).speed(1.0).suffix(" m"));
                if a.changed() || b.changed() {
                    self.decor.ground_auto = true;
                    self.decor.view = None;
                }
            });
            ui.horizontal(|ui| {
                ui.label(tr!("Rayon", "Radius"));
                ui.add(egui::Slider::new(&mut self.decor.draft.radius, 30.0..=6000.0).suffix(" m").logarithmic(true))
                    .on_hover_text(
                        tr!(
                            "Les objets plus loin que ce rayon ne sont pas chargés. Au-delà de 1 km, \
                             le chargement est plus long et prend plus de mémoire",
                            "Objects farther than this radius are not loaded. Beyond 1 km, \
                             loading takes longer and uses more memory"
                        ),
                    );
                ui.checkbox(&mut self.decor.draft.flora, tr!("Végétation", "Vegetation"));
                ui.checkbox(&mut self.decor.draft.terrain, "Terrain").on_hover_text(
                    tr!(
                        "Relief et sol du jeu : détaillé (un point tous les 2 m) dans le rayon, jusqu'à 1 km, \
                         plus grossier (8 m) au-delà jusqu'à l'horizon, à 2,5 km (ou le rayon + 1 km)",
                        "The game's terrain: detailed (a point every 2 m) within the radius, up to 1 km, \
                         coarser (8 m) beyond that up to the horizon, at 2.5 km (or the radius + 1 km)"
                    ),
                );
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.decor.ground_auto, tr!("Hauteur du sol automatique", "Automatic ground height"))
                    .on_hover_text(
                        tr!(
                            "La hauteur du terrain au lieu choisi (ou, sans terrain, celle estimée d'après les objets \
                             posés autour) est ramenée à la grille, y = 0. Décochez pour la régler à la main, \
                             par exemple pour placer les personnages sur un toit ou un pad. Sur le décor \
                             déjà chargé à ce lieu, le réglage s'applique tout de suite.",
                            "The terrain height at the chosen place (or, without terrain, the one estimated from the \
                             objects around) is brought to the grid, y = 0. Uncheck to set it by hand, \
                             for example to put characters on a roof or a pad. On scenery already loaded \
                             at this place, the setting applies right away."
                        ),
                    );
                let mut g = self.decor.draft.ground.unwrap_or(0.0);
                let r = ui
                    .add_enabled(!self.decor.ground_auto, crate::calc::drag(&mut g).speed(0.05).fixed_decimals(2).suffix(" m"))
                    .on_hover_text(tr!("Altitude du jeu placée sur la grille (y = 0) ; glisser ou taper une valeur", "Game altitude placed on the grid (y = 0); drag or type a value"));
                if r.changed() {
                    self.decor.draft.ground = Some(g);
                    self.lift_decor(g);
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                let busy = self.decor.loading;
                if ui.add_enabled(!busy, egui::Button::new(tr!("Charger le décor", "Load scenery"))).clicked() {
                    load = true;
                }
                if busy {
                    ui.add(egui::Spinner::new());
                    let total = self.loader.decor_total.load(Ordering::Relaxed).max(1);
                    let p = self.loader.decor_progress.load(Ordering::Relaxed);
                    ui.add(egui::ProgressBar::new(p as f32 / total as f32).desired_width(160.0).show_percentage());
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(150));
                }
                if ui.add_enabled(self.decor.applied.is_some(), egui::Button::new(tr!("Retirer", "Remove"))).clicked() {
                    self.clear_decor();
                }
            });
            if !self.decor.info.is_empty() {
                ui.label(egui::RichText::new(&self.decor.info).small());
            }
        });
        if load {
            let mut s = self.decor.draft.clone();
            if self.decor.ground_auto {
                s.ground = None;
            }
            s.visible = true;
            self.renderer.decor_visible = true;
            self.decor.sun_on_load = true;
            self.request_decor(s);
        }
        self.decor.open = open;
    }

    /// Vue d'ensemble cliquable : le clic choisit le lieu, le glisser déplace la vue, la molette
    /// zoome (autour du pointeur).
    fn overview_widget(&mut self, ui: &mut egui::Ui) {
        if let Some((z, e)) = &self.decor.overview_error {
            ui.colored_label(Color32::from_rgb(255, 120, 100), format!("{z} : {e}"));
        }
        let Some((_, tex, ov)) = &self.decor.overview else { return };
        let side = 520.0;
        // click_and_drag : le glisser sur la carte est pris par elle, pas par la fenêtre
        let (rect, resp) = ui.allocate_exact_size(Vec2::splat(side), Sense::click_and_drag());
        let d = &mut self.decor.draft;
        // fenêtre visible de la carte (UV) : centrée sur le lieu, ou là où l'utilisateur l'a
        // déplacée, et bornée à la carte
        let zoom = self.decor.zoom.max(1.0);
        let half = 0.5 / zoom;
        let [cu, cv] = self.decor.view.unwrap_or_else(|| ov.to_uv(d.center[0], d.center[1]));
        let mut cu = cu.clamp(half, 1.0 - half);
        let mut cv = cv.clamp(half, 1.0 - half);
        if resp.dragged() {
            let delta = resp.drag_delta();
            cu = (cu - delta.x / rect.width() * 2.0 * half).clamp(half, 1.0 - half);
            cv = (cv - delta.y / rect.height() * 2.0 * half).clamp(half, 1.0 - half);
            self.decor.view = Some([cu, cv]);
        }
        if resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                let new_zoom = (zoom * (1.0 + scroll * 0.004)).clamp(1.0, 16.0);
                // garder sous le pointeur le même point de la carte
                if let Some(pos) = resp.hover_pos() {
                    let pu = cu - half + (pos.x - rect.min.x) / rect.width() * 2.0 * half;
                    let pv = cv - half + (pos.y - rect.min.y) / rect.height() * 2.0 * half;
                    let k = zoom / new_zoom;
                    let nh = 0.5 / new_zoom;
                    cu = (pu - (pu - cu) * k).clamp(nh, 1.0 - nh);
                    cv = (pv - (pv - cv) * k).clamp(nh, 1.0 - nh);
                    self.decor.view = Some([cu, cv]);
                }
                self.decor.zoom = new_zoom;
            }
        }
        let half = 0.5 / self.decor.zoom.max(1.0);
        let (cu, cv) = (cu.clamp(half, 1.0 - half), cv.clamp(half, 1.0 - half));
        let uv =Rect::from_min_max(Pos2::new(cu - half, cv - half), Pos2::new(cu + half, cv + half));
        let p = ui.painter_at(rect);
        p.image(tex.id(), rect, uv, Color32::WHITE);
        let to_screen = |u: f32, v: f32| {
            Pos2::new(
                rect.min.x + (u - uv.min.x) / uv.width() * rect.width(),
                rect.min.y + (v - uv.min.y) / uv.height() * rect.height(),
            )
        };
        let to_uv = |pos: Pos2| {
            (
                uv.min.x + (pos.x - rect.min.x) / rect.width() * uv.width(),
                uv.min.y + (pos.y - rect.min.y) / rect.height() * uv.height(),
            )
        };
        if let Some(pos) = resp.interact_pointer_pos().filter(|_| resp.clicked()) {
            let (u, v) = to_uv(pos);
            let [x, z] = ov.to_world(u.clamp(0.0, 1.0), v.clamp(0.0, 1.0));
            d.center = [x.round(), z.round()];
            self.decor.ground_auto = true;
            // la vue reste où elle est (pas de saut vers le nouveau lieu)
            self.decor.view = Some([cu, cv]);
        }
        if resp.hovered() && !resp.dragged() {
            if let Some(pos) = resp.hover_pos() {
                let (u, v) = to_uv(pos);
                let [x, z] = ov.to_world(u, v);
                resp.clone().on_hover_text_at_pointer(format!("x {x:.0}, z {z:.0}"));
            }
        }
        // lieu et rayon
        let [u, v] = ov.to_uv(d.center[0], d.center[1]);
        let c = to_screen(u, v);
        let r_px = d.radius / ov.extent / uv.width() * rect.width();
        p.circle_stroke(c, r_px, Stroke::new(1.5f32, Color32::from_rgb(80, 220, 255)));
        p.line_segment([c - Vec2::X * 6.0, c + Vec2::X * 6.0], Stroke::new(2.0f32, Color32::WHITE));
        p.line_segment([c - Vec2::Y * 6.0, c + Vec2::Y * 6.0], Stroke::new(2.0f32, Color32::WHITE));
        // décor chargé
        if let Some(a) = self.decor.applied.as_ref().filter(|a| a.zone == d.zone && a.center != d.center) {
            let [u, v] = ov.to_uv(a.center[0], a.center[1]);
            let r_px = a.radius / ov.extent / uv.width() * rect.width();
            p.circle_stroke(to_screen(u, v), r_px, Stroke::new(1.0f32, Color32::from_rgba_unmultiplied(255, 255, 255, 120)));
        }
    }
}
