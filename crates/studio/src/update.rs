//! Mise à jour de PlanetStudio depuis les GitHub Releases du projet.
//!
//! Rien ne s'installe sans l'accord de l'utilisateur : la vérification signale seulement qu'une
//! version plus récente existe ; le téléchargement ne démarre que sur « Mettre à jour ».
//!
//! Format attendu d'une Release (voir `.github/workflows/release.yml`) :
//! - tag `vX.Y.Z` ;
//! - `PlanetStudio-windows-x64.zip`, qui contient `planet-studio.exe` ;
//! - `PlanetStudio-windows-x64.zip.sha256` : empreinte SHA-256 de l'archive.

use crate::ffmpeg::{self, DownloadProgress};
use anyhow::{bail, Context, Result};
use eframe::egui;
use serde::Deserialize;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

/// Dépôt GitHub dont les Releases sont suivies.
pub const REPO: &str = "WowiLoveSchwepps/PlanetStudio";
/// Version de cet exécutable.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
const ZIP_NAME: &str = "PlanetStudio-windows-x64.zip";
const EXE_NAME: &str = "planet-studio.exe";

/// Release plus récente que la version en cours.
#[derive(Clone, Debug)]
pub struct Release {
    /// version sans le « v » (ex. `0.2.0`)
    pub version: String,
    /// notes de version (texte de la Release)
    pub notes: String,
    /// page de la Release sur GitHub
    pub page: String,
    zip_url: String,
    zip_size: u64,
    sha_url: String,
}

#[derive(Clone, Debug)]
pub enum State {
    Idle,
    Checking { manual: bool },
    UpToDate { manual: bool },
    Available(Release),
    Downloading(Release),
    /// nouvelle version en place : effective au prochain démarrage
    Installed(String),
    Failed { message: String, manual: bool },
}

/// Vérification et installation en arrière-plan ; l'interface lit [`Updater::state`].
pub struct Updater {
    state: Arc<Mutex<State>>,
    pub progress: Arc<DownloadProgress>,
}

impl Updater {
    pub fn new() -> Self {
        cleanup_old();
        Self { state: Arc::new(Mutex::new(State::Idle)), progress: Arc::new(DownloadProgress::default()) }
    }

    pub fn state(&self) -> State {
        self.state.lock().unwrap().clone()
    }

    pub fn set_idle(&self) {
        *self.state.lock().unwrap() = State::Idle;
    }

    pub fn busy(&self) -> bool {
        matches!(self.state(), State::Checking { .. } | State::Downloading(_))
    }

    /// Demande à GitHub la dernière Release. `manual` : lancée par l'utilisateur (les erreurs et
    /// « déjà à jour » s'affichent ; au démarrage, elles restent silencieuses).
    pub fn check(&self, ctx: &egui::Context, manual: bool) {
        if self.busy() {
            return;
        }
        *self.state.lock().unwrap() = State::Checking { manual };
        let state = self.state.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let next = match latest_release() {
                Ok(Some(r)) => State::Available(r),
                Ok(None) => State::UpToDate { manual },
                Err(e) => State::Failed { message: format!("{e:#}"), manual },
            };
            *state.lock().unwrap() = next;
            ctx.request_repaint();
        });
    }

    /// Télécharge, vérifie et met en place la nouvelle version.
    pub fn install(&self, rel: Release, ctx: &egui::Context) {
        if self.busy() {
            return;
        }
        self.progress.bytes.store(0, Ordering::Relaxed);
        self.progress.stage.store(0, Ordering::Relaxed);
        self.progress.cancel.store(false, Ordering::Relaxed);
        *self.state.lock().unwrap() = State::Downloading(rel.clone());
        let state = self.state.clone();
        let progress = self.progress.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let next = match download_and_install(&rel, &progress) {
                Ok(()) => State::Installed(rel.version.clone()),
                Err(e) => State::Failed { message: format!("{e:#}"), manual: true },
            };
            *state.lock().unwrap() = next;
            ctx.request_repaint();
        });
    }

    pub fn cancel(&self) {
        self.progress.cancel.store(true, Ordering::Relaxed);
    }
}

/// Exécutable compilé depuis les sources (`cargo build`, dossier `target`) : pas de mise à jour,
/// elle remplacerait la version de développement.
pub fn is_source_build() -> bool {
    std::env::current_exe().is_ok_and(|p| p.components().any(|c| c.as_os_str().eq_ignore_ascii_case("target")))
}

/// Relance la nouvelle version (appelé à la fermeture, après l'enregistrement du projet).
pub fn relaunch() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).spawn();
    }
}

// ------------------------------------------------------------------ versions

/// `v1.2.3`, `1.2`, `1.2.3-beta` → (1, 2, 3).
fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.trim().trim_start_matches(['v', 'V']);
    let core = v.split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u64>());
    let major = it.next()?.ok()?;
    let minor = it.next().unwrap_or(Ok(0)).ok()?;
    let patch = it.next().unwrap_or(Ok(0)).ok()?;
    Some((major, minor, patch))
}

fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

// ------------------------------------------------------------------ réseau

/// `curl.exe` de Windows, jamais un autre trouvé dans le PATH.
fn curl() -> Result<PathBuf> {
    let system = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let curl = system.join("System32").join("curl.exe");
    if !curl.is_file() {
        bail!("curl.exe introuvable (Windows 10 version 1803 ou plus récente nécessaire)");
    }
    Ok(curl)
}

/// Petite requête HTTPS dont la réponse tient en mémoire (API GitHub, fichier .sha256).
fn fetch_text(url: &str) -> Result<String> {
    let mut c = ffmpeg::command(&curl()?);
    c.args(["--location", "--fail", "--silent", "--show-error", "--max-time", "20"])
        .args(["--header", "Accept: application/vnd.github+json"])
        .args(["--header", "User-Agent: PlanetStudio-updater"])
        .arg(url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = c.output().context("lancement de curl")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("404") {
            bail!("aucune version publiée sur GitHub pour l'instant");
        }
        bail!("connexion à GitHub impossible : {}", err.trim().lines().last().unwrap_or("erreur réseau"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    size: u64,
    browser_download_url: String,
}

/// Dernière Release publiée, si elle est plus récente que [`CURRENT`].
fn latest_release() -> Result<Option<Release>> {
    let json = fetch_text(&format!("https://api.github.com/repos/{REPO}/releases/latest"))?;
    parse_release(&json, CURRENT)
}

fn parse_release(json: &str, current: &str) -> Result<Option<Release>> {
    let gh: GhRelease = serde_json::from_str(json).context("réponse de GitHub illisible")?;
    if !is_newer(&gh.tag_name, current) {
        return Ok(None);
    }
    let zip = gh.assets.iter().find(|a| a.name == ZIP_NAME);
    let sha = gh.assets.iter().find(|a| a.name == format!("{ZIP_NAME}.sha256"));
    let (Some(zip), Some(sha)) = (zip, sha) else {
        bail!("la version {} ne contient pas {ZIP_NAME} et son fichier .sha256", gh.tag_name);
    };
    Ok(Some(Release {
        version: gh.tag_name.trim_start_matches(['v', 'V']).to_string(),
        notes: gh.body.unwrap_or_default().trim().to_string(),
        page: gh.html_url,
        zip_url: zip.browser_download_url.clone(),
        zip_size: zip.size,
        sha_url: sha.browser_download_url.clone(),
    }))
}

// ------------------------------------------------------------------ installation

fn update_dir() -> Result<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA").context("dossier LOCALAPPDATA introuvable")?;
    Ok(PathBuf::from(base).join("PlanetStudio").join("update"))
}

/// Ancien exécutable laissé par une mise à jour (Windows ne supprime pas un .exe en cours
/// d'utilisation) : supprimé au démarrage suivant.
fn old_exe(exe: &Path) -> PathBuf {
    exe.with_extension("exe.old")
}

fn cleanup_old() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(old_exe(&exe));
    }
}

fn download_and_install(rel: &Release, progress: &DownloadProgress) -> Result<()> {
    let exe = std::env::current_exe().context("emplacement de PlanetStudio introuvable")?;
    let dir = update_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("création de {}", dir.display()))?;
    let archive = dir.join(format!("{ZIP_NAME}.part"));
    struct Remove(PathBuf);
    impl Drop for Remove {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _guard = Remove(archive.clone());

    // 1) téléchargement
    let expected = fetch_text(&rel.sha_url)?;
    let expected = expected.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
    if expected.len() != 64 || !expected.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("fichier .sha256 de la version {} invalide", rel.version);
    }
    let mut c = ffmpeg::command(&curl()?);
    c.args(["--location", "--fail", "--silent", "--show-error", "--retry", "2", "--output"])
        .arg(&archive)
        .args(["--header", "User-Agent: PlanetStudio-updater"])
        .arg(&rel.zip_url);
    c.stderr(Stdio::piped());
    let mut child = c.spawn().context("lancement de curl")?;
    let mut stderr = child.stderr.take().unwrap();
    let errors = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break st;
        }
        if progress.cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            bail!("mise à jour annulée");
        }
        progress.bytes.store(std::fs::metadata(&archive).map(|m| m.len()).unwrap_or(0), Ordering::Relaxed);
        std::thread::sleep(std::time::Duration::from_millis(150));
    };
    let log = errors.join().unwrap_or_default();
    if !status.success() {
        bail!("téléchargement impossible : {}", log.trim().lines().last().unwrap_or("erreur réseau"));
    }

    // 2) vérification : taille annoncée par GitHub, puis empreinte SHA-256
    progress.stage.store(1, Ordering::Relaxed);
    let size = std::fs::metadata(&archive)?.len();
    progress.bytes.store(size, Ordering::Relaxed);
    if size != rel.zip_size {
        bail!("fichier reçu inattendu ({size} octets au lieu de {})", rel.zip_size);
    }
    if ffmpeg::sha256_file(&archive)? != expected {
        bail!("empreinte SHA-256 incorrecte : fichier refusé");
    }

    // 3) extraction de l'exécutable à côté de l'actuel, puis échange des deux fichiers
    progress.stage.store(2, Ordering::Relaxed);
    let new_exe = exe.with_extension("exe.new");
    extract_exe(&archive, &new_exe)?;
    swap_exe(&exe, &new_exe)
}

fn extract_exe(archive: &Path, target: &Path) -> Result<()> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(archive)?).context("archive illisible")?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let name = entry.name().replace('\\', "/");
        if name.rsplit('/').next().is_some_and(|n| n.eq_ignore_ascii_case(EXE_NAME)) {
            let mut out = std::fs::File::create(target).with_context(|| {
                format!("écriture impossible dans {} (dossier protégé ?)", target.parent().unwrap_or(target).display())
            })?;
            std::io::copy(&mut entry, &mut out)?;
            return Ok(());
        }
    }
    bail!("{EXE_NAME} absent de l'archive")
}

/// Windows permet de renommer un .exe en cours d'exécution (pas de le supprimer) : l'actuel
/// devient `.exe.old`, le nouveau prend sa place.
fn swap_exe(exe: &Path, new_exe: &Path) -> Result<()> {
    let old = old_exe(exe);
    let _ = std::fs::remove_file(&old);
    if let Err(e) = std::fs::rename(exe, &old) {
        let _ = std::fs::remove_file(new_exe);
        return Err(e).context("remplacement de l'exécutable impossible");
    }
    if let Err(e) = std::fs::rename(new_exe, exe) {
        // remet l'ancienne version en place
        let _ = std::fs::rename(&old, exe);
        let _ = std::fs::remove_file(new_exe);
        return Err(e).context("remplacement de l'exécutable impossible");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("v1.0", "0.9.9"));
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("v0.0.9", "0.1.0"));
        assert!(!is_newer("nightly", "0.1.0"));
        assert_eq!(parse_version("v1.2.3-beta.1"), Some((1, 2, 3)));
    }

    #[test]
    fn release_json() {
        let json = r#"{
            "tag_name": "v0.2.0", "body": "Nouveautés", "html_url": "https://github.com/x/y/releases/tag/v0.2.0",
            "assets": [
                {"name": "PlanetStudio-windows-x64.zip", "size": 1234, "browser_download_url": "https://e/zip"},
                {"name": "PlanetStudio-windows-x64.zip.sha256", "size": 90, "browser_download_url": "https://e/sha"}
            ]}"#;
        let r = parse_release(json, "0.1.0").unwrap().unwrap();
        assert_eq!(r.version, "0.2.0");
        assert_eq!(r.zip_size, 1234);
        assert_eq!(r.sha_url, "https://e/sha");
        assert!(parse_release(json, "0.2.0").unwrap().is_none());
        let no_assets = r#"{"tag_name": "v9.0.0", "html_url": "u", "assets": []}"#;
        assert!(parse_release(no_assets, "0.1.0").is_err());
    }

    #[test]
    fn swap() {
        let dir = std::env::temp_dir().join("planet_studio_swap_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("planet-studio.exe");
        let new = exe.with_extension("exe.new");
        std::fs::write(&exe, b"ancien").unwrap();
        std::fs::write(&new, b"nouveau").unwrap();
        swap_exe(&exe, &new).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"nouveau");
        assert_eq!(std::fs::read(old_exe(&exe)).unwrap(), b"ancien");
        assert!(!new.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
