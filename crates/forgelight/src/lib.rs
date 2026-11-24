//! Lecture des données du moteur Forgelight (PlanetSide 2).

pub mod adr;
pub mod dds;
pub mod dme;
pub mod fbx;
pub mod flora;
pub mod hash;
pub mod lzham;
pub mod materials;
pub mod model;
pub mod morpheme;
pub mod namelist;
pub mod pack;
pub mod skeleton;
pub mod terrain;
pub mod tga;
pub mod zone;

pub use model::{slot, Mesh, Model, Vertex};
pub use pack::AssetIndex;
pub use skeleton::{Skeleton, Transform};

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;

/// Palettes `.dma` imposées à certains matériaux d'un modèle (modèle, palette, matériaux).
/// Dôme du Biolab : le jeu habille ses nervures (matériau 0) d'Auraxium (`gold.dds`) ; on
/// reprend l'ancienne palette du même maillage, en métal gris des bâtiments. La coque garde
/// ses matériaux : la texture intérieure de l'ancienne palette y fait des taches turquoise.
const PALETTE_OVERRIDES: &[(&str, &str, &[usize])] = &[(
    "Common_Structures_FacilityBioLab_Shield2_LOD0.dme",
    "Common_Structures_FacilityBioLab_Shield2_LOD0.dma",
    &[0],
)];

/// Feuillage dont la texture de couleur ne correspond pas à la géométrie : le jeu reprend celle
/// d'un autre arbre (ici les branches enneigées d'Esamir, dont l'alpha dessine des plaques
/// blanches), alors que la forme des feuilles est dans le canal B du `_S` et leur couleur dans
/// les teintes de l'acteur. On compose une texture de couleur à partir du `_S` : alpha = B,
/// couleur = teinte sombre → claire selon A, ombrée par G.
pub struct MaskedFoliage {
    /// `_S` du feuillage
    pub spec: &'static str,
    /// nom donné à la texture composée (absente des archives du jeu)
    pub color: &'static str,
    /// teintes de l'acteur (`TintAliases` du `.adr`), en sRGB
    pub dark: [f32; 3],
    pub light: [f32; 3],
}

pub const MASKED_FOLIAGE: &[MaskedFoliage] = &[MaskedFoliage {
    spec: "Common_Props_Biolab_Flora_TallTree_Leaves_S.dds",
    color: "Common_Props_Biolab_Flora_TallTree_Leaves_C.dds",
    dark: [0.384, 0.216, 0.400],
    light: [0.539, 0.494, 0.0],
}];

/// Texture de couleur composée d'un [`MaskedFoliage`] à partir de son `_S` décodé.
pub fn compose_masked_foliage(f: &MaskedFoliage, spec: &dds::Image) -> dds::Image {
    let mips = spec
        .mips
        .iter()
        .map(|m| {
            let mut out = vec![0u8; m.len()];
            for (o, p) in out.chunks_exact_mut(4).zip(m.chunks_exact(4)) {
                let (g, b, a) = (p[1] as f32 / 255.0, p[2], p[3] as f32 / 255.0);
                // les reflets clairs du `_S` prennent la teinte claire, le reste la sombre
                let t = ((a - 0.7) / 0.25).clamp(0.0, 1.0) * 0.7;
                // G vaut ~0,5 au milieu des feuilles : ombrage autour de 1
                let shade = (g / 0.5).clamp(0.0, 1.6);
                for k in 0..3 {
                    let c = (f.dark[k] + (f.light[k] - f.dark[k]) * t) * shade;
                    o[k] = (c.clamp(0.0, 1.0) * 255.0).round() as u8;
                }
                o[3] = b;
            }
            out
        })
        .collect();
    dds::Image::rgba(spec.width, spec.height, mips)
}

/// Accès haut niveau aux données du jeu.
pub struct GameData {
    pub root: PathBuf,
    pub index: AssetIndex,
    pub materials: materials::MaterialLibrary,
}

impl GameData {
    /// `root` : dossier du jeu (contenant `Resources/Assets`) ou directement le dossier des .pack2.
    pub fn open(root: &Path) -> Result<Self> {
        let assets = if root.join("Resources").join("Assets").is_dir() {
            root.join("Resources").join("Assets")
        } else {
            root.to_path_buf()
        };
        let mut index = AssetIndex::open_dir(&assets)?;
        let xml = index.read("materials_3.xml").context("materials_3.xml introuvable")?;
        let materials = materials::MaterialLibrary::parse(&String::from_utf8_lossy(&xml))?;
        index.add_names(["materials_3.xml".to_string()]);
        Ok(Self { root: root.to_path_buf(), index, materials })
    }

    /// Charge (ou construit puis met en cache) la liste des noms.
    pub fn load_namelist(&mut self, cache: &Path, progress: &AtomicUsize) -> Result<()> {
        namelist::load_or_build(&mut self.index, cache, progress)
    }

    /// Lit un asset par son nom. Quelques matériaux citent un chemin
    /// (`..\GraphicsData\Resources\square.dds`) : faute de mieux, le nom seul.
    pub fn read(&self, name: &str) -> Result<Vec<u8>> {
        match self.index.read(name) {
            Err(e) => match name.rsplit(['\\', '/']).next().filter(|b| b.len() < name.len()) {
                Some(base) => self.index.read(base).map_err(|_| e),
                None => Err(e),
            },
            ok => ok,
        }
    }

    pub fn load_dme(&self, name: &str) -> Result<Model> {
        self.load_dme_with_palette(name, None)
    }

    /// Comme [`Self::load_dme`], avec la palette `.dma` d'un acteur : c'est elle qui donne les
    /// variantes de faction d'un même modèle (écran `TREquipmentTerminal.dds` d'un terminal
    /// `_TR`, couleur d'un bouclier). Une palette qui ne correspond pas au modèle est ignorée.
    pub fn load_dme_with_palette(&self, name: &str, palette: Option<&str>) -> Result<Model> {
        let data = self.read(name)?;
        let mut dme = dme::Dme::parse(&data).with_context(|| format!("lecture de {name}"))?;
        let mut palette_warning = None;
        if let Some(pal) = palette.filter(|p| !p.is_empty()) {
            let all: Vec<usize> = (0..dme.materials.len()).collect();
            if let Err(e) = self.read(pal).and_then(|d| dme.apply_palette(&d, &all)) {
                palette_warning = Some(format!("palette {pal} ignorée : {e:#}"));
            }
        }
        if let Some((_, dma, only)) = PALETTE_OVERRIDES.iter().find(|(m, _, _)| m.eq_ignore_ascii_case(name)) {
            let pal = self.read(dma)?;
            dme.apply_palette(&pal, only).with_context(|| format!("palette {dma}"))?;
        }
        let mut model = Model::from_dme(name, &dme, &self.materials)?;
        model.warnings.extend(palette_warning);
        let esamir_model = name.to_ascii_lowercase().starts_with("esamir");
        for mesh in &mut model.meshes {
            // Hors d'Esamir, certains décors (sol et arbres mutants du Biolab) ont gardé la
            // couleur enneigée d'Esamir (variante d'alias « Default1 ») alors que leur relief est
            // le leur : on reprend la couleur assortie au relief quand elle existe.
            if !esamir_model {
                if let (Some(d), Some(n)) = (&mesh.diffuse, &mesh.normal_map) {
                    let own = n.len() > 6 && n[n.len() - 6..].eq_ignore_ascii_case("_n.dds");
                    if own && d.to_ascii_lowercase().starts_with("esamir_") {
                        let c = format!("{}_C.dds", &n[..n.len() - 6]);
                        if !c.eq_ignore_ascii_case(d) && self.index.contains(&c) {
                            mesh.diffuse = Some(c);
                        }
                    }
                }
                // textures enneigées d'Esamir qui ont une variante sans neige (`_NoSnow`)
                for t in [&mut mesh.diffuse, &mut mesh.normal_map, &mut mesh.specular].into_iter().flatten() {
                    if t.to_ascii_lowercase().starts_with("esamir_") && t.len() > 6 {
                        let (stem, suffix) = t.split_at(t.len() - 6);
                        let dry = format!("{stem}_NoSnow{suffix}");
                        if self.index.contains(&dry) {
                            *t = dry;
                        }
                    }
                }
            }
            let spec = mesh.specular.as_deref().unwrap_or("");
            if let Some(f) = MASKED_FOLIAGE.iter().find(|f| f.spec.eq_ignore_ascii_case(spec)) {
                mesh.diffuse = Some(f.color.to_string());
            }
        }
        Ok(model)
    }

    /// Charge un acteur (.adr) ou directement un modèle (.dme).
    pub fn load_model(&self, name: &str) -> Result<Model> {
        if name.to_ascii_lowercase().ends_with(".adr") {
            let adr = adr::Adr::parse(&self.read(name)?)?;
            let base = adr.base_model.clone().with_context(|| format!("{name} : pas de modèle de base"))?;
            let mut model = self.load_dme_with_palette(&base, adr.palette.as_deref())?;
            model.name = name.to_string();
            model.equip_slot = adr.equip_slot;
            model.covered_slots = adr.covered_slots;
            Ok(model)
        } else {
            self.load_dme(name)
        }
    }

    /// Charge un réseau d'animation Morpheme (ex: "ThirdPerson.mrn").
    pub fn load_network(&self, name: &str) -> Result<morpheme::Network> {
        morpheme::Network::parse(self.read(name)?).with_context(|| format!("réseau {name}"))
    }

    /// Décode une texture `.dds` ou `.tga` en RGBA8.
    pub fn load_texture(&self, name: &str) -> Result<dds::Image> {
        if let Some(f) = MASKED_FOLIAGE.iter().find(|f| f.color.eq_ignore_ascii_case(name)) {
            return Ok(compose_masked_foliage(f, &self.load_texture(f.spec)?));
        }
        let data = self.read(name)?;
        if name.to_ascii_lowercase().ends_with(".tga") {
            tga::decode(&data).with_context(|| format!("texture {name}"))
        } else {
            dds::decode(&data).with_context(|| format!("texture {name}"))
        }
    }

    /// Comme [`Self::load_texture`], mais garde les blocs BC1/BC2/BC3/BC7 compressés (voir
    /// [`dds::decode_compressed`]) : pour les textures envoyées telles quelles au GPU, dont on
    /// ne lit pas les pixels.
    pub fn load_texture_gpu(&self, name: &str) -> Result<dds::Image> {
        let lower = name.to_ascii_lowercase();
        if !lower.ends_with(".dds") || MASKED_FOLIAGE.iter().any(|f| f.color.eq_ignore_ascii_case(name)) {
            return self.load_texture(name);
        }
        dds::decode_compressed(&self.read(name)?).with_context(|| format!("texture {name}"))
    }
}

/// Dossier de données de l'application (cache de la namelist, préférences).
pub fn app_data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("XDG_DATA_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("PlanetStudio")
}

pub fn default_namelist_path() -> PathBuf {
    app_data_dir().join("namelist.txt")
}
