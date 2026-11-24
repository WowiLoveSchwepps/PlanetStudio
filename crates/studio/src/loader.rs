//! Chargements en arrière-plan (données du jeu, modèles, textures).

use forgelight::{dds, GameData, Model};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

pub enum Msg {
    Game(Result<Arc<GameData>, String>),
    Network {
        name: String,
        result: Result<Arc<forgelight::morpheme::Network>, String>,
    },
    Model {
        request: u64,
        asset: String,
        result: Result<(Arc<Model>, Vec<(String, dds::Image)>), String>,
    },
    /// décor d'une carte
    Decor { request: u64, result: Result<crate::decor::DecorData, String> },
}

pub struct Loader {
    tx: Sender<Msg>,
    pub rx: Receiver<Msg>,
    pub progress: Arc<AtomicUsize>,
    pub total: Arc<AtomicUsize>,
    pub busy: Arc<AtomicUsize>,
    /// avancement du chargement du décor (fichiers lus / à lire)
    pub decor_progress: Arc<AtomicUsize>,
    pub decor_total: Arc<AtomicUsize>,
    ctx: eframe::egui::Context,
}

impl Loader {
    pub fn new(ctx: eframe::egui::Context) -> Self {
        let (tx, rx) = channel();
        Self {
            tx,
            rx,
            progress: Arc::new(AtomicUsize::new(0)),
            total: Arc::new(AtomicUsize::new(0)),
            busy: Arc::new(AtomicUsize::new(0)),
            decor_progress: Arc::new(AtomicUsize::new(0)),
            decor_total: Arc::new(AtomicUsize::new(0)),
            ctx,
        }
    }

    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::Relaxed) > 0
    }

    pub fn open_game(&self, root: PathBuf) {
        let tx = self.tx.clone();
        let progress = self.progress.clone();
        let total = self.total.clone();
        let busy = self.busy.clone();
        let ctx = self.ctx.clone();
        busy.fetch_add(1, Ordering::Relaxed);
        std::thread::spawn(move || {
            let r = (|| -> anyhow::Result<GameData> {
                let mut gd = GameData::open(&root)?;
                total.store(gd.index.len(), Ordering::Relaxed);
                progress.store(0, Ordering::Relaxed);
                gd.load_namelist(&forgelight::default_namelist_path(), &progress)?;
                Ok(gd)
            })();
            busy.fetch_sub(1, Ordering::Relaxed);
            let _ = tx.send(Msg::Game(r.map(Arc::new).map_err(|e| format!("{e:#}"))));
            ctx.request_repaint();
        });
    }

    /// Charge un réseau d'animation Morpheme (.mrn).
    pub fn load_network(&self, game: Arc<GameData>, name: String) {
        let tx = self.tx.clone();
        let busy = self.busy.clone();
        let ctx = self.ctx.clone();
        busy.fetch_add(1, Ordering::Relaxed);
        std::thread::spawn(move || {
            let result = game.load_network(&name).map(Arc::new).map_err(|e| format!("{e:#}"));
            busy.fetch_sub(1, Ordering::Relaxed);
            let _ = tx.send(Msg::Network { name, result });
            ctx.request_repaint();
        });
    }

    /// Charge un modèle et décode ses textures (sauf celles déjà en mémoire).
    pub fn load_model(&self, game: Arc<GameData>, asset: String, request: u64, skip_textures: Vec<String>) {
        self.spawn_model(game, asset, request, skip_textures, true);
    }

    /// Même chose sans compter dans l'activité du chargeur (aperçu au survol : pas de sablier).
    pub fn load_model_quiet(&self, game: Arc<GameData>, asset: String, request: u64, skip_textures: Vec<String>) {
        self.spawn_model(game, asset, request, skip_textures, false);
    }

    fn spawn_model(&self, game: Arc<GameData>, asset: String, request: u64, skip_textures: Vec<String>, counted: bool) {
        let tx = self.tx.clone();
        let busy = counted.then(|| self.busy.clone());
        let ctx = self.ctx.clone();
        if let Some(b) = &busy {
            b.fetch_add(1, Ordering::Relaxed);
        }
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<_> {
                let model = game.load_model(&asset)?;
                let mut textures = Vec::new();
                let mut seen = std::collections::HashSet::new();
                for t in model.meshes.iter().flat_map(|m| m.render_textures()) {
                    let key = t.to_ascii_lowercase();
                    if skip_textures.contains(&key) || !seen.insert(key) {
                        continue;
                    }
                    match game.load_texture_gpu(t) {
                        Ok(img) => textures.push((t.clone(), img)),
                        Err(e) => eprintln!("texture {t} : {e:#}"),
                    }
                }
                Ok((Arc::new(model), textures))
            })();
            if let Some(b) = &busy {
                b.fetch_sub(1, Ordering::Relaxed);
            }
            let _ = tx.send(Msg::Model { request, asset, result: result.map_err(|e| format!("{e:#}")) });
            ctx.request_repaint();
        });
    }
}

impl Loader {
    /// Charge le décor d'une carte autour d'un point (modèles et textures, en parallèle).
    pub fn load_decor(
        &self,
        game: Arc<GameData>,
        settings: crate::decor::DecorSettings,
        request: u64,
        skip_textures: std::collections::HashSet<String>,
    ) {
        let tx = self.tx.clone();
        let busy = self.busy.clone();
        let ctx = self.ctx.clone();
        let (progress, total) = (self.decor_progress.clone(), self.decor_total.clone());
        progress.store(0, Ordering::Relaxed);
        total.store(0, Ordering::Relaxed);
        busy.fetch_add(1, Ordering::Relaxed);
        std::thread::spawn(move || {
            let result = crate::decor::load(&game, &settings, &skip_textures, &progress, &total).map_err(|e| format!("{e:#}"));
            busy.fetch_sub(1, Ordering::Relaxed);
            let _ = tx.send(Msg::Decor { request, result });
            ctx.request_repaint();
        });
    }
}
