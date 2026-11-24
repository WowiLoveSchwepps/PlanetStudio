//! PlanetStudio : studio d'animation pour les modèles de PlanetSide 2.

// application à fenêtre : Windows n'ouvre plus de console au lancement (voir attach_parent_console)
#![windows_subsystem = "windows"]

mod anim;
mod app;
mod calc;
mod camo;
mod camera;
mod camrec;
mod capture;
mod clips;
mod dance;
mod decal;
mod decor;
mod environment;
mod export;
mod ffmpeg;
mod update;
mod keymap;
mod loader;
mod post;
mod quality;
mod render;
mod retarget;
mod scene;
mod shots;
mod particles;
mod skins;
mod sky;
mod video;
mod wake;

use eframe::egui;
use eframe::egui_wgpu::{self, wgpu};

/// Appareil wgpu d'egui, avec les textures compressées BC (celles du jeu) quand la carte les lit.
fn wgpu_options() -> egui_wgpu::WgpuConfiguration {
    let default = egui_wgpu::WgpuSetupCreateNew::default();
    let base = default.device_descriptor.clone();
    let setup = egui_wgpu::WgpuSetupCreateNew {
        device_descriptor: std::sync::Arc::new(move |adapter| {
            let mut d = base(adapter);
            // formats propres à la carte : MSAA 2× et 8× quand elle les accepte
            d.required_features |= adapter.features()
                & (wgpu::Features::TEXTURE_COMPRESSION_BC | wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES);
            d
        }),
        ..default
    };
    egui_wgpu::WgpuConfiguration { wgpu_setup: egui_wgpu::WgpuSetup::CreateNew(setup), ..Default::default() }
}

/// Lancé depuis un terminal, l'exe écrit dans ce terminal (messages, erreurs de --capture) ;
/// lancé par double-clic, il n'y a pas de console parente et rien ne s'affiche.
/// Une sortie déjà redirigée (`> journal.txt`) est laissée telle quelle.
#[cfg(windows)]
fn attach_parent_console() {
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    #[link(name = "kernel32")]
    extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
        fn GetStdHandle(std_handle: u32) -> *mut std::ffi::c_void;
    }
    unsafe {
        if GetStdHandle(STD_OUTPUT_HANDLE).is_null() {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

#[cfg(not(windows))]
fn attach_parent_console() {}

fn main() -> eframe::Result<()> {
    attach_parent_console();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--capture") {
        if let Err(e) = capture::run(&args[1..]) {
            eprintln!("Erreur : {e:#}");
            std::process::exit(1);
        }
        return Ok(());
    }
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: wgpu_options(),
        viewport: egui::ViewportBuilder::default()
            .with_title("PlanetStudio")
            .with_inner_size([1600.0, 950.0])
            .with_min_inner_size([900.0, 600.0]),
        // la fenêtre est agrandie après sa création (voir StudioApp::new) : agrandie dès sa construction,
        // Windows la marque « agrandie » mais lui garde une taille fausse (bord droit coupé)
        ..Default::default()
    };
    eframe::run_native("PlanetStudio", options, Box::new(|cc| Ok(Box::new(app::StudioApp::new(cc)))))
}
