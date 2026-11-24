//! Cartes (`.zone`, version 1 de PlanetSide 2) : objets du décor placés par le jeu.
//!
//! En-tête : `ZONE`, version (u32), puis six offsets absolus (écosystèmes, flore, murs
//! invisibles, objets, lumières, inconnu), puis la grille du terrain : quads par tuile, taille
//! d'une tuile (m), hauteur, sommets par tuile, tuiles par chunk, origine X/Y et nombre de
//! chunks en X/Y (en unités de tuile).
//!
//! Section objets : un nombre (u32), puis pour chaque acteur : son nom (`.adr`, chaîne terminée
//! par 0), sa distance d'affichage (f32) et ses instances (u32 + 57 octets chacune) :
//! position (4 × f32, w = 1), rotation (4 × f32, radians : x = lacet autour de Y, y = tangage
//! autour de X, z = roulis autour de Z), échelle (4 × f32), identifiant (u32), « pas d'ombre »
//! (u8) et multiplicateur de LOD (f32).

use anyhow::{bail, Context, Result};
use glam::{Mat4, Quat, Vec3};

#[derive(Clone, Debug)]
pub struct ZoneHeader {
    pub version: u32,
    pub quads_per_tile: u32,
    pub tile_size: f32,
    pub tile_height: f32,
    pub verts_per_tile: u32,
    pub tiles_per_chunk: u32,
    pub start_x: i32,
    pub start_y: i32,
    pub chunks_x: u32,
    pub chunks_y: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct Instance {
    pub position: Vec3,
    /// radians : x = lacet (autour de Y), y = tangage (autour de X), z = roulis (autour de Z)
    pub rotation: Vec3,
    pub scale: Vec3,
    pub id: u32,
    pub no_shadow: bool,
    pub lod_mul: f32,
}

impl Instance {
    pub fn rotation_quat(&self) -> Quat {
        Quat::from_rotation_y(self.rotation.x) * Quat::from_rotation_x(self.rotation.y) * Quat::from_rotation_z(self.rotation.z)
    }

    /// Matrice monde de l'instance (échelle, puis rotation, puis translation).
    pub fn matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation_quat(), self.position)
    }
}

#[derive(Clone, Debug)]
pub struct ZoneObject {
    /// acteur placé (`.adr`)
    pub actor: String,
    /// distance d'affichage (m)
    pub render_distance: f32,
    pub instances: Vec<Instance>,
}

/// Lumière placée sur la carte (lampe, néon, projecteur).
///
/// Section des lumières : un nombre (u32), puis pour chacune : nom et nom de teinte (chaînes
/// terminées par 0 : `PointLight_…`, `White`, `WarmLight`, `blue_ampstationlegs`…), type (u16 :
/// 1 ponctuelle, 2 projecteur), 3 octets à 0, position et rotation (2 × 4 f32 : x, y, z, 1 ;
/// lacet, tangage, roulis, 1, en radians comme les objets), portée (m) et intensité (2 × f32), couleur (4 octets A, R, V, B), 5 octets (le premier à 1 sur quelques lumières), cône
/// (4 f32 : angles intérieur et extérieur en radians, angle entier, puis exposant ; valeurs par
/// défaut π/4, π/3, 1 sur les ponctuelles), une chaîne (vide) et un identifiant (u32). Relu
/// sur Amerish : 7 166 lumières, la section finit exactement où commence la suivante.
#[derive(Clone, Debug)]
pub struct Light {
    pub name: String,
    pub color_name: String,
    /// projecteur (sinon ponctuelle)
    pub spot: bool,
    pub position: Vec3,
    /// radians : lacet, tangage (positif : vers le bas), roulis
    pub rotation: Vec3,
    /// portée (m)
    pub range: f32,
    /// second nombre après la portée : suit la taille de la lampe (0,12 pour un voyant, 1 pour une
    /// lampe d'arche, 2,8 à 4 pour un grand projecteur), lu comme son intensité
    pub inner_range: f32,
    /// couleur (sRGB 0..255)
    pub color: [u8; 3],
    /// cône du projecteur : angles entiers intérieur et extérieur (radians), exposant
    pub cone: [f32; 3],
    pub id: u32,
}

impl Light {
    /// Direction du faisceau d'un projecteur (vers l'avant de sa rotation, +Z tourné comme un objet).
    pub fn direction(&self) -> Vec3 {
        let q = Quat::from_rotation_y(self.rotation.x) * Quat::from_rotation_x(self.rotation.y) * Quat::from_rotation_z(self.rotation.z);
        (q * Vec3::Z).normalize_or_zero()
    }
}

fn parse_lights(r: &mut Reader, end: usize) -> Result<Vec<Light>> {
    let n = r.u32()? as usize;
    anyhow::ensure!(n <= 200_000, "{n} lumières");
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let name = r.cstr()?;
        let color_name = r.cstr()?;
        let kind = u16::from_le_bytes(r.take(2)?.try_into().unwrap());
        r.take(3)?;
        let p = r.vec4()?;
        let q = r.vec4()?;
        let range = r.f32()?;
        let inner_range = r.f32()?;
        let c = r.take(4)?;
        r.take(5)?;
        let cone = r.vec4()?;
        r.cstr()?;
        let id = r.u32()?;
        out.push(Light {
            name,
            color_name,
            spot: kind == 2,
            position: Vec3::new(p[0], p[1], p[2]),
            rotation: Vec3::new(q[0], q[1], q[2]),
            range,
            inner_range,
            color: [c[1], c[2], c[3]],
            cone: [cone[0], cone[1], cone[2]],
            id,
        });
    }
    if end != 0 && r.p != end {
        bail!("section lumières : fin à {:#x}, section suivante à {:#x}", r.p, end);
    }
    Ok(out)
}

/// Écosystème du terrain (herbe, roche, galets…) : sa texture de détail, répétée sur le sol.
#[derive(Clone, Debug)]
pub struct Eco {
    /// numéro, celui des listes d'écosystèmes des chunks
    pub index: u32,
    pub name: String,
    /// détail : couleur RGB, normale X dans l'alpha
    pub color_nx: String,
    /// détail : spéculaire, mélange (B) et normale Y dans l'alpha
    pub spec_blend_ny: String,
    /// répétitions de la texture de détail sur une tuile de 64 m
    pub detail_repeat: u32,
    pub blend_strength: f32,
    /// matériau physique (`grass`, `rockgrey`…)
    pub physics: String,
    /// couches de flore posées sur cet écosystème (instances dans les `.cnk0`)
    pub flora: Vec<FloraLayer>,
}

/// Couche de flore d'un écosystème : quelle plante, avec quelle densité et où.
#[derive(Clone, Debug)]
pub struct FloraLayer {
    pub density: f32,
    pub min_scale: f32,
    pub max_scale: f32,
    /// pente (radians) : pic et étendue
    pub slope_peak: f32,
    pub slope_extent: f32,
    /// altitude (m du jeu)
    pub min_elevation: f32,
    pub max_elevation: f32,
    /// présence minimale de l'écosystème (0 à 255)
    pub min_alpha: u8,
    /// nom de la flore (voir [`Flora`])
    pub flora: String,
    /// teintes (R, V, B, A) et leur part (%)
    pub tints: Vec<([u8; 4], u32)>,
}

/// Plante de la section flore : son modèle (`.dme`, sans `.adr`) et sa texture.
#[derive(Clone, Debug)]
pub struct Flora {
    pub name: String,
    pub texture: String,
    pub model: String,
    pub flag: bool,
    pub unknown: [f32; 2],
}

#[derive(Clone, Debug)]
pub struct Zone {
    pub header: ZoneHeader,
    /// lumières placées (lampes, projecteurs) ; vide si la section est illisible
    pub lights: Vec<Light>,
    /// écosystèmes du terrain (vide si la section est illisible)
    pub ecos: Vec<Eco>,
    /// plantes de la flore du sol (vide si la section est illisible)
    pub floras: Vec<Flora>,
    pub objects: Vec<ZoneObject>,
}

/// Section des écosystèmes : un nombre (u32), puis pour chacun : numéro (u32), nom, textures
/// de détail `_cnx` et `_sbny` (chaînes terminées par 0), répétitions (u32), force du mélange,
/// spéculaire min/max, lissé min/max (5 × f32), matériau physique, puis ses couches de flore :
/// un nombre (u32), et pour chacune densité, échelle min/max, pente (pic, étendue), altitude
/// min/max (7 × f32), alpha min (u8), nom de la flore, et ses teintes (u32 + 8 octets chacune).
fn parse_ecos(r: &mut Reader, end: usize) -> Result<Vec<Eco>> {
    let n = r.u32()? as usize;
    anyhow::ensure!(n <= 256, "{n} écosystèmes");
    let mut ecos = Vec::with_capacity(n);
    for _ in 0..n {
        let index = r.u32()?;
        let name = r.cstr()?;
        let color_nx = r.cstr()?;
        let spec_blend_ny = r.cstr()?;
        let detail_repeat = r.u32()?;
        let blend_strength = r.f32()?;
        r.take(16)?;
        let physics = r.cstr()?;
        let layers = r.u32()? as usize;
        anyhow::ensure!(layers <= 256, "{layers} couches de flore");
        let mut flora = Vec::with_capacity(layers);
        for _ in 0..layers {
            let v: Vec<f32> = (0..7).map(|_| r.f32()).collect::<Result<_>>()?;
            let min_alpha = r.u8()?;
            let name = r.cstr()?;
            let tints = r.u32()? as usize;
            anyhow::ensure!(tints <= 256, "{tints} teintes");
            let tints = (0..tints)
                .map(|_| Ok((r.take(4)?.try_into().unwrap(), r.u32()?)))
                .collect::<Result<Vec<([u8; 4], u32)>>>()?;
            flora.push(FloraLayer {
                density: v[0],
                min_scale: v[1],
                max_scale: v[2],
                slope_peak: v[3],
                slope_extent: v[4],
                min_elevation: v[5],
                max_elevation: v[6],
                min_alpha,
                flora: name,
                tints,
            });
        }
        ecos.push(Eco { index, name, color_nx, spec_blend_ny, detail_repeat, blend_strength, physics, flora });
    }
    anyhow::ensure!(r.p == end, "écosystèmes : fin à {:#x}, attendue à {end:#x}", r.p);
    Ok(ecos)
}

/// Section flore : un nombre (u32), puis pour chaque plante son nom, sa texture et son modèle
/// (chaînes terminées par 0), un octet et deux f32 inconnus.
fn parse_floras(r: &mut Reader) -> Result<Vec<Flora>> {
    let n = r.u32()? as usize;
    anyhow::ensure!(n <= 4096, "{n} plantes");
    (0..n)
        .map(|_| {
            Ok(Flora {
                name: r.cstr()?,
                texture: r.cstr()?,
                model: r.cstr()?,
                flag: r.u8()? != 0,
                unknown: [r.f32()?, r.f32()?],
            })
        })
        .collect()
}

struct Reader<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.p + n > self.d.len() {
            bail!("fin de fichier inattendue à {:#x}", self.p);
        }
        let s = &self.d[self.p..self.p + n];
        self.p += n;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }
    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.u32()?))
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn vec4(&mut self) -> Result<[f32; 4]> {
        Ok([self.f32()?, self.f32()?, self.f32()?, self.f32()?])
    }
    fn cstr(&mut self) -> Result<String> {
        let rest = &self.d[self.p..];
        let n = rest.iter().position(|&b| b == 0).context("chaîne non terminée")?;
        let s = String::from_utf8_lossy(&rest[..n]).into_owned();
        self.p += n + 1;
        Ok(s)
    }
}

impl Zone {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut r = Reader { d: data, p: 0 };
        if r.take(4)? != b"ZONE" {
            bail!("signature ZONE absente");
        }
        let version = r.u32()?;
        if version != 1 {
            bail!("version de zone {version} non gérée");
        }
        let ecos_off = r.u32()? as usize;
        let floras_off = r.u32()? as usize;
        let _walls = r.u32()?;
        let objects_off = r.u32()? as usize;
        let lights_off = r.u32()? as usize;
        let unknown_off = r.u32()? as usize;
        let header = ZoneHeader {
            version,
            quads_per_tile: r.u32()?,
            tile_size: r.f32()?,
            tile_height: r.f32()?,
            verts_per_tile: r.u32()?,
            tiles_per_chunk: r.u32()?,
            start_x: r.i32()?,
            start_y: r.i32()?,
            chunks_x: r.u32()?,
            chunks_y: r.u32()?,
        };

        let mut er = Reader { d: data, p: ecos_off };
        let ecos = parse_ecos(&mut er, floras_off).unwrap_or_else(|e| {
            eprintln!("zone : écosystèmes illisibles : {e:#}");
            Vec::new()
        });
        let mut fr = Reader { d: data, p: floras_off };
        let floras = parse_floras(&mut fr).unwrap_or_else(|e| {
            eprintln!("zone : flore illisible : {e:#}");
            Vec::new()
        });

        r.p = objects_off;
        let count = r.u32()? as usize;
        let mut objects = Vec::with_capacity(count.min(100_000));
        for _ in 0..count {
            let actor = r.cstr()?;
            let render_distance = r.f32()?;
            let n = r.u32()? as usize;
            if n * 57 > data.len() {
                bail!("{actor} : nombre d'instances aberrant ({n})");
            }
            let mut instances = Vec::with_capacity(n);
            for _ in 0..n {
                let p = r.vec4()?;
                let q = r.vec4()?;
                let s = r.vec4()?;
                instances.push(Instance {
                    position: Vec3::new(p[0], p[1], p[2]),
                    rotation: Vec3::new(q[0], q[1], q[2]),
                    scale: Vec3::new(s[0], s[1], s[2]),
                    id: r.u32()?,
                    no_shadow: r.u8()? != 0,
                    lod_mul: r.f32()?,
                });
            }
            objects.push(ZoneObject { actor, render_distance, instances });
        }
        if lights_off != 0 && r.p != lights_off {
            bail!("section objets : fin à {:#x}, lumières attendues à {:#x}", r.p, lights_off);
        }
        let lights = if lights_off == 0 {
            Vec::new()
        } else {
            let mut lr = Reader { d: data, p: lights_off };
            parse_lights(&mut lr, unknown_off).unwrap_or_else(|e| {
                eprintln!("zone : lumières illisibles : {e:#}");
                Vec::new()
            })
        };
        Ok(Self { header, lights, ecos, floras, objects })
    }

    /// Taille d'un côté de la carte (m).
    pub fn extent(&self) -> f32 {
        self.header.chunks_x as f32 * self.header.tile_size
    }

    pub fn instance_count(&self) -> usize {
        self.objects.iter().map(|o| o.instances.len()).sum()
    }
}

/// Vue d'ensemble d'une carte, assemblée à partir des tuiles de couleur du terrain
/// (`<carte>_Tile_<a>_<b>_LOD<n>.dds`).
///
/// Repère vérifié en superposant les objets de la carte aux bâtiments peints sur les tuiles :
/// dans le nom d'une tuile, `a` suit le z du monde et `b` le x (en tuiles de 64 m) ; dans
/// l'image d'une tuile, les colonnes suivent z croissant et les lignes x croissant.
#[derive(Clone, Debug)]
pub struct Overview {
    /// RGBA8 ; colonnes : z croissant vers la droite ; lignes : x croissant vers le haut
    pub rgba: Vec<u8>,
    pub size: u32,
    /// coin minimal (x, z) couvert et côté (m)
    pub origin: [f32; 2],
    pub extent: f32,
}

impl Overview {
    /// Pixel (u, v dans 0..1, v vers le bas) → position monde (x, z).
    pub fn to_world(&self, u: f32, v: f32) -> [f32; 2] {
        [self.origin[0] + (1.0 - v) * self.extent, self.origin[1] + u * self.extent]
    }
    /// Position monde (x, z) → pixel (u, v dans 0..1).
    pub fn to_uv(&self, x: f32, z: f32) -> [f32; 2] {
        [(z - self.origin[1]) / self.extent, 1.0 - (x - self.origin[0]) / self.extent]
    }
}

/// Nom d'une tuile de couleur du terrain : `a` en z, `b` en x, en tuiles de 64 m.
pub fn tile_name(zone: &str, a: i32, b: i32, lod: u32) -> String {
    format!("{zone}_Tile_{a:03}_{b:03}_LOD{lod}.dds")
}

impl Zone {
    /// Assemble la vue d'ensemble de la carte `name` avec le niveau de détail le plus grossier
    /// disponible. Une tuile LOD n couvre 4 × 2^n tuiles de terrain.
    pub fn overview(&self, name: &str, read: impl Fn(&str) -> Option<crate::dds::Image>) -> Option<Overview> {
        let h = &self.header;
        let span = h.chunks_x.max(1) as i32;
        for lod in (0..=3u32).rev() {
            let step = (4i32 << lod).min(span);
            let n = (span / step).max(1);
            let mut first = read(&tile_name(name, h.start_y, h.start_x, lod));
            let Some(ts) = first.as_ref().map(|f| f.width) else { continue };
            let size = ts * n as u32;
            let mut rgba = vec![0u8; (size * size * 4) as usize];
            // tuile (i en z, j en x) : colonne i, ligne (n - 1 - j) depuis le haut
            for i in 0..n {
                for j in 0..n {
                    let img = if i == 0 && j == 0 {
                        first.take()
                    } else {
                        read(&tile_name(name, h.start_y + i * step, h.start_x + j * step, lod))
                    };
                    let Some(img) = img else { continue };
                    if img.width != ts || img.height != ts {
                        continue;
                    }
                    let px0 = i as u32 * ts;
                    let py0 = (n - 1 - j) as u32 * ts;
                    let line = (ts * 4) as usize;
                    for row in 0..ts {
                        // la ligne 0 de la tuile est au x minimal : en bas de l'image
                        let r = (ts - 1 - row) as usize;
                        let o = (((py0 + row) * size + px0) * 4) as usize;
                        rgba[o..o + line].copy_from_slice(&img.mips[0][r * line..(r + 1) * line]);
                    }
                }
            }
            let extent = (n * step) as f32 * h.tile_size;
            let origin = [h.start_x as f32 * h.tile_size, h.start_y as f32 * h.tile_size];
            return Some(Overview { rgba, size, origin, extent });
        }
        None
    }
}

/// Cartes connues du jeu (le nom du fichier est `<nom>.zone`).
pub const KNOWN_ZONES: &[&str] = &["Indar", "Amerish", "Esamir", "Hossin", "VR", "Tutorial", "quickload"];

#[cfg(test)]
mod tests {
    use super::*;

    /// Les conversions pixel ↔ monde de la vue d'ensemble sont inverses l'une de l'autre, et
    /// le nord de l'image (v = 0) est au x maximal.
    #[test]
    fn overview_roundtrip() {
        let ov = Overview { rgba: Vec::new(), size: 1024, origin: [-4096.0, -4096.0], extent: 8192.0 };
        for (x, z) in [(0.0, 0.0), (139.0, -2376.0), (1098.0, 64.0), (-4000.0, 3900.0)] {
            let [u, v] = ov.to_uv(x, z);
            let [x2, z2] = ov.to_world(u, v);
            assert!((x - x2).abs() < 1e-2 && (z - z2).abs() < 1e-2, "{x},{z} -> {x2},{z2}");
        }
        assert_eq!(ov.to_world(0.5, 0.0), [4096.0, 0.0]);
        assert_eq!(tile_name("Indar", -4, 32, 0), "Indar_Tile_-04_032_LOD0.dds");
    }

    /// Lacet seul : rotation autour de Y dans le sens de D3DXMatrixRotationY.
    #[test]
    fn instance_yaw() {
        let i = Instance {
            position: Vec3::new(10.0, 2.0, -3.0),
            rotation: Vec3::new(std::f32::consts::FRAC_PI_2, 0.0, 0.0),
            scale: Vec3::ONE,
            id: 0,
            no_shadow: false,
            lod_mul: 1.0,
        };
        // D3D (vecteur ligne) : x' = x cos + z sin… soit +X → -Z pour un quart de tour
        let p = i.matrix().transform_point3(Vec3::X);
        assert!((p - Vec3::new(10.0, 2.0, -4.0)).length() < 1e-5, "{p}");
    }

    #[test]
    fn lights() {
        // une lumière comme sur Amerish : projecteur blanc orienté vers le bas
        let mut d = Vec::new();
        d.extend(1u32.to_le_bytes());
        d.extend(b"SpotLight_1 White ");
        d.extend(2u16.to_le_bytes());
        d.extend([0u8; 3]);
        for v in [10.0f32, 80.0, -5.0, 1.0, 0.0, 1.5707964, 0.0, 1.0, 50.0, 2.828] {
            d.extend(v.to_le_bytes());
        }
        d.extend([0xff, 0xfe, 0xfb, 0xd8]);
        d.extend([0u8; 5]);
        for v in [0.79f32, 1.4, 3.0, 0.0] {
            d.extend(v.to_le_bytes());
        }
        d.extend(b" ");
        d.extend(42u32.to_le_bytes());
        let end = d.len();
        let l = parse_lights(&mut Reader { d: &d, p: 0 }, end).unwrap();
        assert_eq!(l.len(), 1);
        let l = &l[0];
        assert!(l.spot && l.color == [0xfe, 0xfb, 0xd8] && l.range == 50.0 && l.id == 42, "{l:?}");
        assert_eq!((l.name.as_str(), l.color_name.as_str()), ("SpotLight_1", "White"));
        // tangage de 90° : faisceau vers le bas
        assert!((l.direction() - Vec3::NEG_Y).length() < 1e-4, "{}", l.direction());
        // fin de section décalée : erreur
        assert!(parse_lights(&mut Reader { d: &d, p: 0 }, end + 1).is_err());
    }
}
