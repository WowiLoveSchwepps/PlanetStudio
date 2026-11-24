//! Encodage vidéo par ffmpeg, programme externe facultatif : x264 ou SVT-AV1 sur le
//! processeur, NVENC (NVIDIA), AMF (AMD) ou Quick Sync (Intel) sur la carte graphique.
//!
//! Le studio convertit lui-même les images en YUV 4:2:0 (BT.709, plage limitée, comme ses
//! encodeurs intégrés) et les envoie à ffmpeg par son entrée standard ; ffmpeg encode et
//! écrit le MP4.

use crate::video::{Codec, FrameSource, RateMode, Speed, VideoParams};
use anyhow::{bail, Context as _, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufWriter, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Encodeur d'une vidéo MP4.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Backend {
    /// encodeurs intégrés au studio (OpenH264, rav1e)
    Builtin,
    /// ffmpeg sur le processeur : x264 ou SVT-AV1
    Cpu,
    /// ffmpeg sur une carte NVIDIA
    Nvenc,
    /// ffmpeg sur une carte ou un processeur graphique AMD
    Amf,
    /// ffmpeg sur un processeur graphique Intel
    Qsv,
}

impl Backend {
    pub const FFMPEG: [Backend; 4] = [Backend::Nvenc, Backend::Amf, Backend::Qsv, Backend::Cpu];

    /// Nom de l'encodeur ffmpeg.
    pub fn encoder(self, codec: Codec) -> Option<&'static str> {
        Some(match (self, codec) {
            (Backend::Builtin, _) => return None,
            (Backend::Cpu, Codec::H264) => "libx264",
            (Backend::Cpu, Codec::Av1) => "libsvtav1",
            (Backend::Nvenc, Codec::H264) => "h264_nvenc",
            (Backend::Nvenc, Codec::Av1) => "av1_nvenc",
            (Backend::Amf, Codec::H264) => "h264_amf",
            (Backend::Amf, Codec::Av1) => "av1_amf",
            (Backend::Qsv, Codec::H264) => "h264_qsv",
            (Backend::Qsv, Codec::Av1) => "av1_qsv",
        })
    }

    pub fn label(self, codec: Codec) -> &'static str {
        match (self, codec) {
            (Backend::Builtin, Codec::H264) => "Intégré · OpenH264 (processeur)",
            (Backend::Builtin, Codec::Av1) => "Intégré · rav1e (processeur)",
            (Backend::Cpu, Codec::H264) => "ffmpeg · x264 (processeur)",
            (Backend::Cpu, Codec::Av1) => "ffmpeg · SVT-AV1 (processeur)",
            (Backend::Nvenc, _) => "ffmpeg · NVENC (carte NVIDIA)",
            (Backend::Amf, _) => "ffmpeg · AMF (carte AMD)",
            (Backend::Qsv, _) => "ffmpeg · Quick Sync (Intel)",
        }
    }

    /// Nom court, pour le résumé de l'export.
    pub fn short(self, codec: Codec) -> &'static str {
        match (self, codec) {
            (Backend::Builtin, Codec::H264) => "OpenH264",
            (Backend::Builtin, Codec::Av1) => "rav1e",
            (Backend::Cpu, Codec::H264) => "x264",
            (Backend::Cpu, Codec::Av1) => "SVT-AV1",
            (Backend::Nvenc, _) => "NVENC",
            (Backend::Amf, _) => "AMF",
            (Backend::Qsv, _) => "Quick Sync",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Backend::Builtin => "Toujours disponible, sans rien installer ; le plus lent.",
            Backend::Cpu => "Meilleure compression à débit égal, plus rapide que l'encodeur intégré.",
            Backend::Nvenc | Backend::Amf | Backend::Qsv => {
                "Encodage par la carte graphique : de loin le plus rapide, compression un peu moins \
                 efficace que sur le processeur (montez légèrement le débit)."
            }
        }
    }

    /// Le VBR 2 passes est un vrai double encodage (images gardées dans un fichier
    /// temporaire) ; les cartes graphiques font leurs 2 passes au fil de l'encodage.
    pub fn true_two_pass(self) -> bool {
        matches!(self, Backend::Builtin | Backend::Cpu)
    }
}

/// ffmpeg trouvé sur la machine, et les encodeurs qui y fonctionnent vraiment.
#[derive(Clone, Debug)]
pub struct Ffmpeg {
    pub exe: PathBuf,
    pub version: String,
    working: Vec<(Codec, Backend)>,
}

impl Ffmpeg {
    pub fn has(&self, codec: Codec, backend: Backend) -> bool {
        backend == Backend::Builtin || self.working.contains(&(codec, backend))
    }
}

/// Commande ffmpeg sans fenêtre de console.
pub(crate) fn command(exe: &Path) -> Command {
    let mut c = Command::new(exe);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    // SVT-AV1 écrit ses informations sur la sortie d'erreur : seulement les avertissements
    c.env("SVT_LOG", "2");
    c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    c
}

/// Dossier où le studio installe ffmpeg quand l'utilisateur le télécharge.
pub fn install_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|l| PathBuf::from(l).join("PlanetStudio").join("ffmpeg"))
}

/// Cherche ffmpeg : à côté du studio, dans son dossier de données, dans le PATH, puis là
/// où winget l'installe (le PATH du studio peut dater d'avant l'installation).
pub fn find() -> Option<PathBuf> {
    let mut list: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
        list.push(dir.join("ffmpeg.exe"));
        list.push(dir.join("ffmpeg").join("bin").join("ffmpeg.exe"));
    }
    if let Some(d) = install_dir() {
        list.push(d.join("bin").join("ffmpeg.exe"));
    }
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    if let Some(path) = std::env::var_os("PATH") {
        list.extend(std::env::split_paths(&path).map(|d| d.join("ffmpeg.exe")));
    }
    if let Some(l) = &local {
        let winget = l.join("Microsoft").join("WinGet");
        list.push(winget.join("Links").join("ffmpeg.exe"));
        if let Ok(packages) = std::fs::read_dir(winget.join("Packages")) {
            for p in packages.flatten().filter(|p| p.file_name().to_string_lossy().to_lowercase().contains("ffmpeg")) {
                for build in std::fs::read_dir(p.path()).into_iter().flatten().flatten() {
                    list.push(build.path().join("bin").join("ffmpeg.exe"));
                }
            }
        }
    }
    list.into_iter().find(|p| p.is_file())
}

/// Trouve ffmpeg et essaie chaque encodeur sur quelques images (en parallèle, quelques
/// secondes) : un encodeur listé par ffmpeg peut manquer de carte graphique ou de pilote.
pub fn probe() -> Option<Ffmpeg> {
    let exe = find()?;
    let out = command(&exe).arg("-version").stdout(Stdio::piped()).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let first = String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or("").to_string();
    let version = first.strip_prefix("ffmpeg version ").unwrap_or(&first).split_whitespace().next().unwrap_or("?").to_string();
    let candidates: Vec<(Codec, Backend)> =
        Backend::FFMPEG.iter().flat_map(|&b| [(Codec::H264, b), (Codec::Av1, b)]).collect();
    let working = std::thread::scope(|s| {
        let tests: Vec<_> = candidates
            .iter()
            .map(|&(codec, b)| {
                let exe = &exe;
                s.spawn(move || {
                    let name = b.encoder(codec).unwrap();
                    let mut c = command(exe);
                    c.args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "color=black:s=320x240:r=30"]);
                    c.args(["-frames:v", "3", "-pix_fmt", if b == Backend::Qsv { "nv12" } else { "yuv420p" }]);
                    c.args(["-c:v", name, "-f", "null", "-"]);
                    c.status().is_ok_and(|st| st.success()).then_some((codec, b))
                })
            })
            .collect();
        tests.into_iter().filter_map(|t| t.join().ok().flatten()).collect()
    });
    Some(Ffmpeg { exe, version, working })
}

/// Passe d'un encodage.
pub enum Pass<'a> {
    Single,
    /// première passe : statistiques écrites dans ce fichier (préfixe)
    First(&'a Path),
    Second(&'a Path),
}

/// Fichiers de statistiques des 2 passes (préfixe donné à ffmpeg).
pub fn pass_log_files(prefix: &Path) -> Vec<PathBuf> {
    let p = prefix.display().to_string();
    vec![PathBuf::from(format!("{p}-0.log")), PathBuf::from(format!("{p}-0.log.mbtree")), PathBuf::from(format!("{p}-0.log.temp"))]
}

/// Réglages de débit et de vitesse propres à chaque encodeur.
fn encoder_args(backend: Backend, p: &VideoParams, pass: &Pass) -> Vec<String> {
    let (t, m) = (p.target_bps, p.max_bps.max(p.target_bps));
    let max = p.codec.supports_max_bitrate();
    let mut a: Vec<String> = Vec::new();
    let mut push = |s: &[&str]| a.extend(s.iter().map(|x| x.to_string()));
    let bits = |v: u32| v.to_string();
    let crf = p.crf;
    match backend {
        Backend::Builtin => {}
        Backend::Cpu => match p.codec {
            Codec::H264 => {
                let preset = match p.speed {
                    Speed::Slow => "slow",
                    Speed::Medium => "medium",
                    Speed::Fast => "veryfast",
                };
                push(&["-preset", preset, "-profile:v", "high"]);
                match p.mode {
                    RateMode::Crf => push(&["-crf", &crf.to_string()]),
                    RateMode::Cbr => push(&["-b:v", &bits(t), "-minrate", &bits(t), "-maxrate", &bits(t), "-bufsize", &bits(t)]),
                    RateMode::Vbr1 | RateMode::Vbr2 => push(&["-b:v", &bits(t), "-maxrate", &bits(m), "-bufsize", &bits(m * 2)]),
                }
            }
            Codec::Av1 => {
                let preset = match p.speed {
                    Speed::Slow => "5",
                    Speed::Medium => "7",
                    Speed::Fast => "10",
                };
                push(&["-preset", preset]);
                match p.mode {
                    RateMode::Crf => push(&["-crf", &crf.min(63).to_string()]),
                    // SVT-AV1 : pas de débit maximal hors CRF, pas de CBR
                    _ => push(&["-b:v", &bits(t)]),
                }
            }
        },
        Backend::Nvenc => {
            let preset = match p.speed {
                Speed::Slow => "p7",
                Speed::Medium => "p5",
                Speed::Fast => "p2",
            };
            push(&["-preset", preset, "-tune", "hq"]);
            match p.mode {
                // 0 voudrait dire « automatique » pour NVENC
                RateMode::Crf => push(&["-rc", "vbr", "-cq", &crf.max(1).to_string(), "-b:v", "0"]),
                RateMode::Cbr => push(&["-rc", "cbr", "-b:v", &bits(t), "-maxrate", &bits(t), "-bufsize", &bits(t)]),
                RateMode::Vbr1 | RateMode::Vbr2 => {
                    push(&["-rc", "vbr", "-b:v", &bits(t)]);
                    if max {
                        push(&["-maxrate", &bits(m), "-bufsize", &bits(m * 2)]);
                    }
                    if p.mode == RateMode::Vbr2 {
                        push(&["-multipass", "fullres"]);
                    }
                }
            }
        }
        Backend::Amf => {
            let quality = match p.speed {
                Speed::Slow => "quality",
                Speed::Medium => "balanced",
                Speed::Fast => "speed",
            };
            push(&["-quality", quality]);
            match p.mode {
                RateMode::Crf => {
                    // AV1 : quantificateur 0..255
                    let q = if p.codec == Codec::Av1 { (crf as u32 * 4).min(255) } else { crf as u32 };
                    let q = q.to_string();
                    push(&["-rc", "cqp", "-qp_i", &q, "-qp_p", &q]);
                    if p.codec == Codec::H264 {
                        push(&["-qp_b", &q]);
                    }
                }
                RateMode::Cbr => push(&["-rc", "cbr", "-b:v", &bits(t), "-maxrate", &bits(t), "-bufsize", &bits(t)]),
                RateMode::Vbr1 | RateMode::Vbr2 => {
                    push(&["-rc", "vbr_peak", "-b:v", &bits(t), "-maxrate", &bits(if max { m } else { t * 3 / 2 })]);
                    if p.mode == RateMode::Vbr2 {
                        push(&["-preanalysis", "1"]);
                    }
                }
            }
        }
        Backend::Qsv => {
            let preset = match p.speed {
                Speed::Slow => "veryslow",
                Speed::Medium => "medium",
                Speed::Fast => "veryfast",
            };
            push(&["-preset", preset]);
            match p.mode {
                RateMode::Crf => push(&["-global_quality", &crf.max(1).to_string()]),
                RateMode::Cbr => push(&["-b:v", &bits(t), "-maxrate", &bits(t), "-bufsize", &bits(t)]),
                RateMode::Vbr1 | RateMode::Vbr2 => {
                    push(&["-b:v", &bits(t), "-maxrate", &bits(if max { m } else { t * 3 / 2 }), "-bufsize", &bits(m * 2)])
                }
            }
        }
    }
    if let Pass::First(log) | Pass::Second(log) = pass {
        let n = if matches!(pass, Pass::First(_)) { "1" } else { "2" };
        a.extend(["-pass".into(), n.into(), "-passlogfile".into(), log.display().to_string()]);
    }
    a
}

/// Encode les images de `next` avec ffmpeg. Une première passe n'écrit pas de vidéo.
pub fn encode(
    exe: &Path,
    backend: Backend,
    p: &VideoParams,
    out: &Path,
    pass: Pass,
    next: &mut FrameSource,
    done: &AtomicUsize,
) -> Result<()> {
    let name = backend.encoder(p.codec).context("encodeur intégré")?;
    let color = ["-color_range", "tv", "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709"];
    let mut c = command(exe);
    c.args(["-hide_banner", "-loglevel", "error", "-y"]);
    c.args(["-f", "rawvideo", "-pix_fmt", "yuv420p"]);
    c.args(["-s", &format!("{}x{}", p.width, p.height), "-framerate", &format!("{}/{}", p.rate.num, p.rate.den)]);
    c.args(color).args(["-i", "pipe:0", "-an", "-c:v", name]);
    if backend == Backend::Qsv {
        c.args(["-pix_fmt", "nv12"]);
    }
    c.args(encoder_args(backend, p, &pass)).args(color);
    match pass {
        Pass::First(_) => c.args(["-f", "null", "NUL"]),
        _ => c.args(["-movflags", "+faststart"]).arg(out),
    };
    c.stdin(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().with_context(|| format!("lancement de ffmpeg ({})", exe.display()))?;
    // la sortie d'erreur est lue à part : pleine, elle bloquerait ffmpeg
    let mut stderr = child.stderr.take().unwrap();
    let errors = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let mut stdin = BufWriter::with_capacity(1 << 22, child.stdin.take().unwrap());
    let mut sent = 0usize;
    let write = (|| -> Result<()> {
        while let Some(yuv) = next()? {
            match yuv.write_to(&mut stdin) {
                Ok(()) => {}
                // ffmpeg s'est arrêté : son message d'erreur dit pourquoi
                Err(e) if e.kind() == ErrorKind::BrokenPipe => return Ok(()),
                Err(e) => return Err(e.into()),
            }
            sent += 1;
            done.fetch_add(1, Ordering::Relaxed);
        }
        match stdin.flush() {
            Err(e) if e.kind() != ErrorKind::BrokenPipe => Err(e.into()),
            _ => Ok(()),
        }
    })();
    drop(stdin);
    let status = child.wait().context("attente de ffmpeg")?;
    let log = errors.join().unwrap_or_default();
    write?;
    if !status.success() {
        let msg: Vec<&str> = log.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("Svt[")).collect();
        let tail = msg[msg.len().saturating_sub(3)..].join(" / ");
        bail!("ffmpeg ({name}) a échoué : {}", if tail.is_empty() { format!("code {status}") } else { tail });
    }
    if sent == 0 {
        bail!("aucune image");
    }
    Ok(())
}

// ------------------------------------------------------------------ téléchargement

/// Version de ffmpeg que le studio propose de télécharger : build « essentials » de
/// gyan.dev (x264, SVT-AV1, NVENC, AMF, Quick Sync), publié sur GitHub. Taille et
/// empreinte fixées : un fichier différent est refusé.
pub struct Package {
    pub version: &'static str,
    pub url: &'static str,
    pub size: u64,
    pub sha256: &'static str,
    /// page d'origine, affichée à l'utilisateur
    pub source: &'static str,
}

pub const PACKAGE: Package = Package {
    version: "9.1",
    url: "https://github.com/GyanD/codexffmpeg/releases/download/9.1/ffmpeg-9.1-essentials_build.zip",
    size: 119_467_219,
    sha256: "b9bbbb3acece000cbeec994f856649354e52c773807841e78908374b24d658fd",
    source: "github.com/GyanD/codexffmpeg (builds de gyan.dev)",
};

/// Avancement d'un téléchargement, lu par l'interface.
#[derive(Default)]
pub struct DownloadProgress {
    /// octets reçus
    pub bytes: std::sync::atomic::AtomicU64,
    /// 0 = téléchargement, 1 = vérification, 2 = extraction
    pub stage: AtomicUsize,
    pub cancel: std::sync::atomic::AtomicBool,
}

/// Télécharge ffmpeg, vérifie l'archive et installe `ffmpeg.exe` (et sa licence) dans
/// [`install_dir`]. Le transfert passe par le `curl.exe` de Windows (Windows 10 1803 et
/// suivants). Renvoie le chemin de `ffmpeg.exe`.
pub fn download(progress: &DownloadProgress) -> Result<PathBuf> {
    download_package(&PACKAGE, &install_dir().context("dossier LOCALAPPDATA introuvable")?, progress)
}

fn download_package(pkg: &Package, dir: &Path, progress: &DownloadProgress) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("création de {}", dir.display()))?;
    let archive = dir.join("ffmpeg.zip.part");
    struct Remove(PathBuf);
    impl Drop for Remove {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _guard = Remove(archive.clone());

    // 1) téléchargement : le curl du système, jamais un autre trouvé dans le PATH
    let system = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let curl = system.join("System32").join("curl.exe");
    if !curl.is_file() {
        bail!("curl.exe introuvable (Windows 10 version 1803 ou plus récente nécessaire)");
    }
    let mut c = command(&curl);
    c.args(["--location", "--fail", "--silent", "--show-error", "--retry", "2", "--output"]).arg(&archive).arg(pkg.url);
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
            bail!("téléchargement annulé");
        }
        let got = std::fs::metadata(&archive).map(|m| m.len()).unwrap_or(0);
        progress.bytes.store(got, Ordering::Relaxed);
        std::thread::sleep(std::time::Duration::from_millis(150));
    };
    let log = errors.join().unwrap_or_default();
    if !status.success() {
        bail!("téléchargement impossible : {}", log.trim().lines().last().unwrap_or("erreur réseau"));
    }

    // 2) vérification : taille puis empreinte SHA-256
    progress.stage.store(1, Ordering::Relaxed);
    let size = std::fs::metadata(&archive)?.len();
    progress.bytes.store(size, Ordering::Relaxed);
    if size != pkg.size {
        bail!("fichier reçu inattendu ({size} octets au lieu de {})", pkg.size);
    }
    let hash = sha256_file(&archive)?;
    if !hash.eq_ignore_ascii_case(pkg.sha256) {
        bail!("empreinte SHA-256 incorrecte : fichier refusé");
    }

    // 3) extraction de ffmpeg.exe et de sa licence seulement
    progress.stage.store(2, Ordering::Relaxed);
    let mut zip = zip::ZipArchive::new(std::fs::File::open(&archive)?).context("archive illisible")?;
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin)?;
    let exe = bin.join("ffmpeg.exe");
    let mut found = false;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let name = entry.name().replace('\\', "/");
        let target = if name.ends_with("/bin/ffmpeg.exe") {
            found = true;
            exe.clone()
        } else if name.matches('/').count() == 1 && name.ends_with("/LICENSE") {
            dir.join("LICENSE.txt")
        } else {
            continue;
        };
        let tmp = target.with_extension("tmp");
        let mut out = std::fs::File::create(&tmp).with_context(|| format!("écriture de {}", tmp.display()))?;
        std::io::copy(&mut entry, &mut out)?;
        drop(out);
        std::fs::rename(&tmp, &target).with_context(|| format!("écriture de {}", target.display()))?;
    }
    if !found {
        bail!("ffmpeg.exe absent de l'archive");
    }
    Ok(exe)
}

pub(crate) fn sha256_file(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256() {
        let p = std::env::temp_dir().join("planet_studio_sha256_test.txt");
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(sha256_file(&p).unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let _ = std::fs::remove_file(p);
    }

    /// Téléchargement réel (119 Mo) dans un dossier temporaire : lancé à la main avec
    /// `cargo test -- --ignored download`.
    #[test]
    #[ignore]
    fn download_real() {
        let dir = std::env::temp_dir().join("planet_studio_ffmpeg_download_test");
        let _ = std::fs::remove_dir_all(&dir);
        let exe = download_package(&PACKAGE, &dir, &DownloadProgress::default()).unwrap();
        let out = command(&exe).arg("-version").stdout(Stdio::piped()).output().unwrap();
        let first = String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or("").to_string();
        println!("{first}");
        assert!(first.contains(PACKAGE.version));
        assert!(dir.join("LICENSE.txt").is_file());
        assert!(!dir.join("ffmpeg.zip.part").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
