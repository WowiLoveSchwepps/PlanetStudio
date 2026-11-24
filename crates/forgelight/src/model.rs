//! Conversion d'un .dme en modèle prêt pour le rendu (sommets décodés, os globaux).

use crate::dme::{Dme, RawMesh};
use crate::hash::jenkins_upper;
use crate::materials::{ElemType, InputLayout, LayoutEntry, MaterialLibrary};
use crate::skeleton::Skeleton;
use anyhow::Result;
use glam::{Vec2, Vec3, Vec4};
use std::collections::HashMap;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    pub joints: [u32; 4],
    pub weights: [f32; 4],
    /// tangente (xyz) et sens de la binormale (w = ±1) ; w = 0 si le maillage n'a pas de repère tangent
    pub tangent: [f32; 4],
    /// 2e et 3e jeux d'UV (`Texcoord` 1 et 2) ; copies de `uv` s'ils sont absents
    pub uv1: [f32; 2],
    pub uv2: [f32; 2],
}

/// Emplacements de texture des matériaux de personnage : hash du paramètre dans le DMAT, identique
/// au `semanticHash` des alias de texture des `.adr`.
pub mod slot {
    pub const COLOR: u32 = -1_295_769_314i32 as u32;
    pub const NORMAL: u32 = 789_998_085;
    pub const SPECULAR: u32 = 67_211_600;
    /// choix du relief de détail (`_DS`) : la direction codée par RGB désigne une face de la
    /// cubemap [`DETAIL`], A dose l'effet
    pub const DETAIL_SELECT: u32 = 1_716_414_136;
    /// motifs de relief de détail : cubemap (`*_DN.dds`, `detail_cube.dds`) dont chaque face est
    /// un motif répété sur le 2e jeu d'UV (G et A : relief ; R : joints et arêtes)
    pub const DETAIL: u32 = -125_639_093i32 as u32;
    /// répétitions des motifs de [`DETAIL`] sur le 2e jeu d'UV (paramètre flottant : 40 sur les
    /// tenues et les véhicules, 6 sur un petit objet de décor)
    pub const DETAIL_REPEAT: u32 = -560_293_637i32 as u32;
    /// couleur superposée (`*_OC.dds`, `grey.dds` = sans effet), répétée sur le 2e jeu d'UV et
    /// masquée par le canal B de la carte de relief (structures)
    pub const OVERLAY: u32 = -1_260_182_040i32 as u32;
    /// décalcomanie, lue en UV2
    pub const DECAL: u32 = -305_773_271i32 as u32;
    /// camouflage, lu en UV1 et masqué par le canal B de la carte de relief
    pub const CAMO: u32 = -1_880_953_979i32 as u32;
    /// masque de teinte de la flore du sol (`*_TM.dds`, blanc = teinté par la couleur de la
    /// plante, voir `forgelight::flora`)
    pub const TINT_MASK: u32 = 170_150_817;

    /// Eau (`Water2013.efb` : `SwampWater`, `StreamWater`, `Water`). Noms retrouvés par leur
    /// hash Jenkins. `BumpMap1` : rides (`water_ripple.dds`), relief DXT5nm répété et animé.
    pub const WATER_BUMP1: u32 = 1_092_061_151;
    /// `BumpMap2` : tourbillons (`water_twirl90.dds`), second relief animé
    pub const WATER_BUMP2: u32 = 1_949_429_259;
    /// `Foam` : couleur de la mousse (`water_foam_algae_C.dds` : bulles des algues d'Hossin)
    pub const WATER_FOAM: u32 = 1_617_206_093;
    /// `FoamNormal` : relief des bulles (`water_foam_N.dds`)
    pub const WATER_FOAM_NORMAL: u32 = 60_282_692;
    /// `Overlay` : nappe d'algues des marais (`Hossin_Props_Water_Algae_OC.dds`) ; RGB couleur des
    /// algues, A eau libre (blanc) ou couverte (gris)
    pub const WATER_OVERLAY: u32 = -1_261_849_043i32 as u32;
    /// `Tint` (RGBA) : couleur de l'eau (olive sombre sur les marais)
    pub const WATER_TINT: u32 = -1_721_105_697i32 as u32;
}

/// Paramètre de couleur (RGBA) des matériaux de champ de force : la couleur de faction d'un
/// bouclier, donnée par la palette de l'acteur (TR : rouge, NC : bleu, VS : violet).
pub const FORCEFIELD_COLOR: u32 = -1_823_134_255i32 as u32;

#[derive(Clone, Debug)]
pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub material_definition: String,
    pub layout: String,
    /// textures du matériau (noms de fichiers)
    pub textures: Vec<String>,
    /// textures par emplacement du matériau : (hash du paramètre, nom). Le hash est le
    /// `semanticHash` des alias de texture des `.adr` (voir le module [`slot`]).
    pub texture_slots: Vec<(u32, String)>,
    /// texture de couleur choisie
    pub diffuse: Option<String>,
    /// carte de normales (`_N`, format DXT5nm : X dans A, Y dans G)
    pub normal_map: Option<String>,
    /// carte `_S` : R métal, G brillance (rugosité = 1 − G), B émissif, A occlusion spéculaire
    pub specular: Option<String>,
    /// camouflage par défaut du matériau (emplacement [`slot::CAMO`]) ; `None` si le matériau
    /// n'accepte pas de camouflage
    pub camo: Option<String>,
    /// décalcomanie par défaut du matériau (emplacement [`slot::DECAL`]) ; `None` si le
    /// matériau n'en accepte pas
    pub decal: Option<String>,
    /// terrain : textures de détail répétées sur le sol (nom, côté d'une répétition en m), la
    /// première pour le sol, la seconde pour la roche des pentes
    pub terrain_detail: Vec<(String, f32)>,
    /// terrain : côtés du quart où le voisin a un autre détail (ou n'en a pas) ; le détail s'y
    /// estompe pour ne pas dessiner le bord. Bits : 1 = x min, 2 = x max, 4 = z min, 8 = z max.
    pub terrain_edges: u8,
    /// terrain : carte de mélange des écosystèmes du quart (128 px, 2 × 2 tuiles de 64 m ; R, V,
    /// B, A : présence des écosystèmes 1 à 4 de chaque tuile, peints l'un sur l'autre par-dessus
    /// l'écosystème 0)
    pub terrain_blend: Option<String>,
    /// terrain : écosystèmes de chaque tuile du quart (4 tuiles × 5 : la tuile (ligne x, colonne
    /// z) en `(ligne * 2 + colonne) * 5`), chacun : couche de la texture de détail (−1 : aucun)
    /// et côté d'une répétition (m)
    pub terrain_ecos: Vec<(i32, f32)>,
    /// terrain : tableaux des textures de détail des écosystèmes (couleur, relief)
    pub terrain_layers: Option<[String; 2]>,
    /// couleur du matériau (paramètre [`FORCEFIELD_COLOR`]), pour les champs de force
    pub color: Option<[f32; 4]>,
    /// répétitions du détail sur le 2e jeu d'UV (paramètre [`slot::DETAIL_REPEAT`], 1 sans lui)
    pub detail_repeat: f32,
    /// eau : couleur (paramètre [`slot::WATER_TINT`])
    pub water_tint: Option<[f32; 4]>,
}

impl Mesh {
    /// Textures utilisées par le rendu (couleur, relief, spéculaire, camouflage et
    /// décalcomanie par défaut).
    pub fn render_textures(&self) -> impl Iterator<Item = &String> {
        [&self.diffuse, &self.normal_map, &self.specular, &self.camo, &self.decal]
            .into_iter()
            .flatten()
            .chain(self.terrain_detail.iter().map(|d| &d.0))
            .chain([slot::DETAIL_SELECT, slot::DETAIL, slot::OVERLAY, slot::TINT_MASK].into_iter().filter_map(|s| self.slot_texture(s)))
            .chain(self.water_textures())
    }

    /// Eau : textures de ses emplacements (rides, tourbillons, mousse, algues), sans les
    /// remplissages (`grey.dds`, couleurs unies).
    pub fn water_textures(&self) -> impl Iterator<Item = &String> {
        let water = self.material_definition.to_ascii_lowercase().contains("water");
        [slot::WATER_BUMP1, slot::WATER_BUMP2, slot::WATER_FOAM, slot::WATER_FOAM_NORMAL, slot::WATER_OVERLAY]
            .into_iter()
            .filter(move |_| water)
            .filter_map(|s| self.water_slot(s))
    }

    /// Texture d'un emplacement de l'eau, sauf remplissage (`grey.dds`, `medium_slate_blue.dds`…).
    pub fn water_slot(&self, slot: u32) -> Option<&String> {
        self.slot_texture(slot).filter(|t| {
            let t = t.to_ascii_lowercase();
            t != "grey.dds" && !t.contains("slate_blue") && !t.contains("_bark_")
        })
    }

    /// Texture d'un emplacement du matériau.
    pub fn slot_texture(&self, slot: u32) -> Option<&String> {
        self.texture_slots.iter().find(|(k, _)| *k == slot).map(|(_, t)| t)
    }

    /// Champ de force (boucliers, portes IFF) : surface translucide et lumineuse.
    pub fn is_forcefield(&self) -> bool {
        self.material_definition.to_ascii_lowercase().starts_with("forcefield")
    }

    pub fn accepts_decal(&self) -> bool {
        self.decal.is_some()
    }

    /// Le camouflage a besoin de son emplacement et de la carte de relief, dont le canal B
    /// sert de masque. Un emplacement qui ne contient que `norm.dds` (relief plat, texture de
    /// remplissage) ne prend pas de camouflage : c'est le cas des combinaisons d'infiltrateur
    /// TR, VS et NSO, dont le canal B ne suit pas les pièces de la tenue (le motif tomberait en
    /// taches découpées au hasard).
    pub fn accepts_camo(&self) -> bool {
        self.camo.as_deref().is_some_and(|c| !is_placeholder_texture(c)) && self.normal_map.is_some()
    }
}

#[derive(Clone, Debug)]
pub struct Model {
    pub name: String,
    pub meshes: Vec<Mesh>,
    pub skeleton: Skeleton,
    pub bounds_min: Vec3,
    pub bounds_max: Vec3,
    pub warnings: Vec<String>,
    /// emplacement d'équipement de l'acteur (.adr `EquipType`), ex. `CustomizationHead` pour une tête
    pub equip_slot: Option<String>,
    /// emplacements qu'il masque quand il est porté (.adr `CoveredSlots`)
    pub covered_slots: Vec<String>,
}

fn read_elem(ty: ElemType, b: &[u8]) -> Vec4 {
    let f = |o: usize| f32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let h = |o: usize| half::f16::from_le_bytes([b[o], b[o + 1]]).to_f32();
    let s = |o: usize| i16::from_le_bytes([b[o], b[o + 1]]) as f32;
    match ty {
        ElemType::Float1 => Vec4::new(f(0), 0.0, 0.0, 1.0),
        ElemType::Float2 => Vec4::new(f(0), f(4), 0.0, 1.0),
        ElemType::Float3 => Vec4::new(f(0), f(4), f(8), 1.0),
        ElemType::Float4 => Vec4::new(f(0), f(4), f(8), f(12)),
        ElemType::Half2 => Vec4::new(h(0), h(2), 0.0, 1.0),
        ElemType::Half4 => Vec4::new(h(0), h(2), h(4), h(6)),
        ElemType::UByte4N => Vec4::new(b[0] as f32, b[1] as f32, b[2] as f32, b[3] as f32) / 255.0,
        ElemType::UByte4 | ElemType::D3dColor => Vec4::new(b[0] as f32, b[1] as f32, b[2] as f32, b[3] as f32),
        ElemType::Short2 => Vec4::new(s(0) / 32767.0, s(2) / 32767.0, 0.0, 1.0),
        ElemType::Short4 => Vec4::new(s(0), s(2), s(4), s(6)) / 32767.0,
        ElemType::Unknown => Vec4::ZERO,
    }
}

/// Devine une disposition si la définition de matériau est inconnue.
fn guess_layout(mesh: &RawMesh) -> InputLayout {
    let e = |stream, ty, usage: &str, ui| LayoutEntry { stream, ty, usage: usage.into(), usage_index: ui };
    let strides: Vec<usize> = mesh.streams.iter().map(|s| s.stride).collect();
    let mut entries = vec![e(0, ElemType::Float3, "Position", 0)];
    if strides.first() == Some(&20) {
        entries.push(e(0, ElemType::UByte4N, "BlendWeight", 0));
        entries.push(e(0, ElemType::D3dColor, "BlendIndices", 0));
    }
    if strides.len() > 1 {
        entries.push(e(1, ElemType::UByte4N, "Tangent", 0));
        entries.push(e(1, ElemType::UByte4N, "Binormal", 0));
        entries.push(e(1, ElemType::Half2, "Texcoord", 0));
    }
    InputLayout { name: "(devinée)".into(), entries }
}

/// Choisit la texture de couleur parmi les textures d'un matériau.
/// Texture de remplissage mise dans un emplacement inutilisé (`norm.dds` : relief plat, 32 px).
pub fn is_placeholder_texture(name: &str) -> bool {
    name.eq_ignore_ascii_case("norm.dds")
}

fn pick_diffuse(textures: &[String]) -> Option<String> {
    let lower: Vec<String> = textures.iter().map(|t| t.to_ascii_lowercase()).collect();
    for suf in ["_c.dds", "_d.dds", "_diffuse.dds", "_color.dds"] {
        if let Some(i) = lower.iter().position(|t| t.ends_with(suf)) {
            return Some(textures[i].clone());
        }
    }
    let bad = ["_n.dds", "_s.dds", "_ds.dds", "_nrm.dds", "tint_", "_m.dds", "_e.dds"];
    textures
        .iter()
        .zip(&lower)
        .find(|(_, l)| !bad.iter().any(|b| l.contains(b)))
        .map(|(t, _)| t.clone())
}

fn pick_normal(textures: &[String]) -> Option<String> {
    textures.iter().find(|t| t.to_ascii_lowercase().ends_with("_n.dds")).cloned()
}

fn pick_specular(textures: &[String]) -> Option<String> {
    textures.iter().find(|t| t.to_ascii_lowercase().ends_with("_s.dds")).cloned()
}

impl Model {
    pub fn from_dme(name: &str, dme: &Dme, lib: &MaterialLibrary) -> Result<Self> {
        let mut warnings = Vec::new();
        let skeleton = Skeleton::from_dme(dme);
        let tex_by_hash: HashMap<u32, &String> =
            dme.texture_names.iter().map(|t| (jenkins_upper(t), t)).collect();

        let mut meshes = Vec::new();
        for (mi, raw) in dme.meshes.iter().enumerate() {
            let mat = dme.materials.get(mi);
            let def_hash = mat.map(|m| m.definition_hash).unwrap_or(0);
            let def_name = lib.defs.get(&def_hash).map(|d| d.name.clone()).unwrap_or_else(|| format!("{def_hash:08X}"));
            let mut layout = lib.layout_for(def_hash).cloned();
            // vérifie la cohérence des tailles de flux
            if let Some(l) = &layout {
                // certaines données ont des octets en plus en fin de sommet : on tolère
                let ok = raw.streams.iter().enumerate().all(|(s, st)| l.stream_stride(s) <= st.stride);
                if !ok {
                    warnings.push(format!(
                        "mesh {mi}: disposition {} incohérente avec les flux {:?}",
                        l.name,
                        raw.streams.iter().map(|s| s.stride).collect::<Vec<_>>()
                    ));
                    layout = None;
                }
            }
            let layout = layout.unwrap_or_else(|| guess_layout(raw));

            // offsets de chaque élément dans son flux
            let mut offs = vec![0usize; raw.streams.len().max(1)];
            let mut elems = Vec::new();
            for e in &layout.entries {
                if e.stream < raw.streams.len() {
                    elems.push((e, offs[e.stream]));
                    offs[e.stream] += e.ty.size();
                }
            }

            // table palette -> os global, par draw call
            let draws: Vec<_> = dme
                .draw_calls
                .iter()
                .skip(raw.draw_offset as usize)
                .take(raw.draw_count as usize)
                .collect();

            let mut vertices: Vec<Vertex> = Vec::with_capacity(raw.vertex_count);
            let mut raw_idx: Vec<Option<[u32; 4]>> = Vec::with_capacity(raw.vertex_count);
            let mut has_weights = false;
            for v in 0..raw.vertex_count {
                let mut pos = Vec3::ZERO;
                let mut normal: Option<Vec3> = None;
                let mut tangent: Option<Vec3> = None;
                let mut binormal: Option<Vec3> = None;
                let mut uv = Vec2::ZERO;
                let mut uv1: Option<Vec2> = None;
                let mut uv2: Option<Vec2> = None;
                let mut weights = Vec4::new(1.0, 0.0, 0.0, 0.0);
                let mut idx = [0u32; 4];
                let mut got_idx = false;
                for (e, off) in &elems {
                    let st = &raw.streams[e.stream];
                    let b = &st.data[v * st.stride + off..];
                    let val = read_elem(e.ty, b);
                    match (e.usage.as_str(), e.usage_index) {
                        ("Position", 0) => pos = val.truncate(),
                        ("Normal", 0) => {
                            normal = Some(if e.ty == ElemType::UByte4N { val.truncate() * 2.0 - 1.0 } else { val.truncate() })
                        }
                        ("Tangent", 0) => tangent = Some(val.truncate() * 2.0 - 1.0),
                        ("Binormal", 0) => binormal = Some(val.truncate() * 2.0 - 1.0),
                        ("Texcoord", 0) => uv = Vec2::new(val.x, val.y),
                        ("Texcoord", 1) => uv1 = Some(Vec2::new(val.x, val.y)),
                        ("Texcoord", 2) => uv2 = Some(Vec2::new(val.x, val.y)),
                        ("BlendWeight", 0) => {
                            weights = val;
                            has_weights = true;
                        }
                        ("BlendIndices", 0) => {
                            idx = [val.x as u32, val.y as u32, val.z as u32, val.w as u32];
                            got_idx = true;
                        }
                        _ => {}
                    }
                }
                let n = normal
                    .unwrap_or_else(|| binormal.unwrap_or(Vec3::Y).cross(tangent.unwrap_or(Vec3::X)))
                    .normalize_or_zero();
                // repère tangent : tangente orthogonalisée + sens de la binormale
                let tangent4 = match (tangent, binormal) {
                    (Some(t), Some(b)) => {
                        let t = (t - n * n.dot(t)).normalize_or_zero();
                        let w = if n.cross(t).dot(b) < 0.0 { -1.0 } else { 1.0 };
                        if t == Vec3::ZERO { [0.0; 4] } else { [t.x, t.y, t.z, w] }
                    }
                    _ => [0.0; 4],
                };

                let wsum = weights.x + weights.y + weights.z + weights.w;
                if !has_weights || wsum <= 0.0 {
                    weights = Vec4::new(1.0, 0.0, 0.0, 0.0);
                } else {
                    weights /= wsum;
                }
                raw_idx.push(got_idx.then_some(idx));
                vertices.push(Vertex {
                    position: pos.to_array(),
                    normal: n.to_array(),
                    uv: uv.to_array(),
                    joints: [0; 4],
                    weights: weights.to_array(),
                    tangent: tangent4,
                    uv1: uv1.unwrap_or(uv).to_array(),
                    uv2: uv2.unwrap_or(uv).to_array(),
                });
            }

            // indices de palette -> indices d'os globaux, draw call par draw call
            if !skeleton.bones.is_empty() {
                for d in &draws {
                    let map = &dme.bone_map[(d.bone_start as usize).min(dme.bone_map.len())
                        ..((d.bone_start + d.bone_count) as usize).min(dme.bone_map.len())];
                    let range = (d.vertex_offset as usize).min(vertices.len())
                        ..((d.vertex_offset + d.vertex_count) as usize).min(vertices.len());
                    let by_palette = palette_indexed(map, range.clone().filter_map(|v| raw_idx[v].map(|i| (i, vertices[v].weights))));
                    for v in range {
                        vertices[v].joints = match raw_idx[v] {
                            Some(idx) => idx.map(|i| {
                                let e = if by_palette { map.iter().find(|m| m.palette as u32 == i) } else { map.get(i as usize) };
                                e.map(|m| m.global as u32).unwrap_or(0)
                            }),
                            // mesh rigide : attaché au premier os du draw call
                            None => [map.first().map(|m| m.global as u32).unwrap_or(0), 0, 0, 0],
                        };
                    }
                }
            }

            let texture_slots: Vec<(u32, String)> = mat
                .map(|m| {
                    m.texture_hashes()
                        .iter()
                        .filter_map(|(slot, h)| tex_by_hash.get(h).map(|s| (*slot, s.to_string())))
                        .collect()
                })
                .unwrap_or_default();
            let textures: Vec<String> = texture_slots.iter().map(|(_, t)| t.clone()).collect();
            let textures = if textures.is_empty() { dme.texture_names.clone() } else { textures };
            // l'emplacement du matériau prime : sur les décors, la liste contient aussi des
            // textures de détail (`Shared_*_C.dds`) qui ne sont pas la couleur de base
            let in_slot = |s: u32| texture_slots.iter().find(|(k, _)| *k == s).map(|(_, t)| t.clone());
            meshes.push(Mesh {
                vertices,
                indices: raw.indices.clone(),
                material_definition: def_name,
                layout: layout.name.clone(),
                diffuse: in_slot(slot::COLOR).or_else(|| pick_diffuse(&textures)),
                normal_map: in_slot(slot::NORMAL).or_else(|| pick_normal(&textures)),
                specular: in_slot(slot::SPECULAR).or_else(|| pick_specular(&textures)),
                camo: texture_slots.iter().find(|(s, _)| *s == slot::CAMO).map(|(_, t)| t.clone()),
                decal: texture_slots.iter().find(|(s, _)| *s == slot::DECAL).map(|(_, t)| t.clone()),
                textures,
                texture_slots,
                terrain_detail: Vec::new(),
                terrain_edges: 0,
                terrain_blend: None,
                terrain_ecos: Vec::new(),
                terrain_layers: None,
                color: mat
                    .and_then(|m| m.params.iter().find(|p| p.name_hash == FORCEFIELD_COLOR && p.data.len() == 16))
                    .map(|p| std::array::from_fn(|k| f32::from_le_bytes(p.data[k * 4..k * 4 + 4].try_into().unwrap()))),
                detail_repeat: mat
                    .and_then(|m| m.params.iter().find(|p| p.name_hash == slot::DETAIL_REPEAT && p.data.len() == 4))
                    .map(|p| f32::from_le_bytes(p.data[..4].try_into().unwrap()))
                    .filter(|r| r.is_finite() && *r > 0.0)
                    .unwrap_or(1.0),
                water_tint: mat
                    .and_then(|m| m.params.iter().find(|p| p.name_hash == slot::WATER_TINT && p.data.len() == 16))
                    .map(|p| std::array::from_fn(|k| f32::from_le_bytes(p.data[k * 4..k * 4 + 4].try_into().unwrap()))),
            });
        }
        Ok(Self {
            name: name.to_string(),
            meshes,
            skeleton,
            bounds_min: dme.bounds_min,
            bounds_max: dme.bounds_max,
            warnings,
            equip_slot: None,
            covered_slots: Vec::new(),
        })
    }
}

/// Les indices d'os d'un sommet désignent-ils l'entrée de la table dont le champ `palette` est
/// égal, plutôt que sa position dans la tranche du draw call ?
///
/// Les deux sont d'habitude identiques (palette 0, 1, 2…). Mais environ 490 modèles (pièces
/// « Hardlight », Aurax, lunettes, casques à visière, armes, véhicules…) ont un maillage dont la
/// palette continue celle du précédent : le 2e maillage de
/// `Armor_TR_Skins_Male_HeavyAssault_Hardlight01` numérote la sienne 42..52, et ses sommets
/// emploient ces valeurs-là. Lus comme des positions, ils tombaient hors de la table, donc sur
/// WORLDROOT : la pièce restait debout quand le corps bougeait. Dans tous ces modèles, les indices
/// employés sont des valeurs de la palette (même quand ils pourraient passer pour des positions :
/// palette 11..31 de `Common_Structures_TempestTower`, dont les positions 0..10 ne serviraient
/// jamais). On garde la lecture par position si un indice n'est pas une valeur de la palette.
fn palette_indexed(map: &[crate::dme::BoneMapEntry], used: impl Iterator<Item = ([u32; 4], [f32; 4])>) -> bool {
    if map.iter().enumerate().all(|(k, m)| m.palette as usize == k) {
        return false;
    }
    let mut used = used.peekable();
    used.peek().is_some()
        && used.all(|(idx, w)| (0..4).all(|k| (k > 0 && w[k] <= 0.0) || map.iter().any(|m| m.palette as u32 == idx[k])))
}
