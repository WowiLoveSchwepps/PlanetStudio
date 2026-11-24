//! Lecture des animations de squelette au format FBX binaire (exports Mixamo, Blender…).
//!
//! On ne lit que ce qui sert à rejouer un squelette : la hiérarchie des nœuds `Model`, leurs
//! propriétés de transformation (translation, pré/post-rotation, pivots, ordre des angles) et
//! les courbes d'animation `Lcl Translation` / `Lcl Rotation` / `Lcl Scaling` de la première pile
//! d'animation. Les maillages, matériaux et peaux sont ignorés.
//!
//! Format binaire : en-tête « Kaydara FBX Binary », puis des enregistrements imbriqués
//! (fin absolue, nombre de propriétés, taille des propriétés, nom). Les tableaux peuvent être
//! compressés en zlib. Depuis la version 7500, les champs d'en-tête passent sur 64 bits.

use anyhow::{bail, Context, Result};
use glam::{DMat4, DQuat, DVec3, EulerRot, Mat4};
use std::collections::HashMap;
use std::io::Read;

/// Nombre d'unités de temps FBX par seconde.
const TICKS_PER_SECOND: f64 = 46_186_158_000.0;

#[derive(Clone, Debug)]
enum Prop {
    Int(i64),
    Float(f64),
    Str(String),
    Raw,
    F64s(Vec<f64>),
    I64s(Vec<i64>),
}

impl Prop {
    fn as_i64(&self) -> Option<i64> {
        match self {
            Prop::Int(v) => Some(*v),
            Prop::Float(v) => Some(*v as i64),
            _ => None,
        }
    }
    fn as_f64(&self) -> Option<f64> {
        match self {
            Prop::Int(v) => Some(*v as f64),
            Prop::Float(v) => Some(*v),
            _ => None,
        }
    }
    fn as_str(&self) -> Option<&str> {
        match self {
            Prop::Str(s) => Some(s),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
struct Node {
    name: String,
    props: Vec<Prop>,
    children: Vec<Node>,
}

impl Node {
    fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }
    /// Valeurs d'un bloc `Properties70` : nom → valeurs (après les 4 champs de type).
    fn properties70(&self) -> HashMap<String, Vec<Prop>> {
        let mut out = HashMap::new();
        if let Some(p70) = self.child("Properties70") {
            for p in &p70.children {
                if let Some(name) = p.props.first().and_then(Prop::as_str) {
                    out.insert(name.to_string(), p.props.iter().skip(4).cloned().collect());
                }
            }
        }
        out
    }
}

struct Reader<'a> {
    d: &'a [u8],
    o: usize,
    wide: bool,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let s = self.d.get(self.o..self.o + n).context("FBX tronqué")?;
        self.o += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into()?))
    }
    fn header_field(&mut self) -> Result<u64> {
        if self.wide {
            self.u64()
        } else {
            Ok(self.u32()? as u64)
        }
    }

    fn array<T: Copy, const N: usize>(&mut self, conv: fn([u8; N]) -> T) -> Result<Vec<T>> {
        let count = self.u32()? as usize;
        let encoding = self.u32()?;
        let len = self.u32()? as usize;
        let raw = self.take(len)?;
        let bytes: Vec<u8> = match encoding {
            0 => raw.to_vec(),
            1 => {
                let mut out = Vec::with_capacity(count * N);
                flate2::read::ZlibDecoder::new(raw).read_to_end(&mut out).context("tableau FBX compressé illisible")?;
                out
            }
            e => bail!("codage de tableau FBX inconnu : {e}"),
        };
        if bytes.len() < count * N {
            bail!("tableau FBX trop court");
        }
        Ok(bytes.chunks_exact(N).take(count).map(|c| conv(c.try_into().unwrap())).collect())
    }

    fn prop(&mut self) -> Result<Prop> {
        let t = self.u8()?;
        Ok(match t {
            b'Y' => Prop::Int(i16::from_le_bytes(self.take(2)?.try_into()?) as i64),
            b'C' => Prop::Int(self.u8()? as i64),
            b'I' => Prop::Int(i32::from_le_bytes(self.take(4)?.try_into()?) as i64),
            b'L' => Prop::Int(i64::from_le_bytes(self.take(8)?.try_into()?)),
            b'F' => Prop::Float(f32::from_le_bytes(self.take(4)?.try_into()?) as f64),
            b'D' => Prop::Float(f64::from_le_bytes(self.take(8)?.try_into()?)),
            b'S' => {
                let n = self.u32()? as usize;
                Prop::Str(String::from_utf8_lossy(self.take(n)?).into_owned())
            }
            b'R' => {
                let n = self.u32()? as usize;
                self.take(n)?;
                Prop::Raw
            }
            b'f' => Prop::F64s(self.array::<f64, 4>(|b| f32::from_le_bytes(b) as f64)?),
            b'd' => Prop::F64s(self.array::<f64, 8>(f64::from_le_bytes)?),
            b'i' => Prop::I64s(self.array::<i64, 4>(|b| i32::from_le_bytes(b) as i64)?),
            b'l' => Prop::I64s(self.array::<i64, 8>(i64::from_le_bytes)?),
            b'b' => Prop::I64s(self.array::<i64, 1>(|b| b[0] as i64)?),
            t => bail!("propriété FBX de type inconnu {:?} à l'octet {}", t as char, self.o - 1),
        })
    }

    /// Lit un enregistrement ; `None` pour l'enregistrement nul qui termine une liste.
    fn node(&mut self) -> Result<Option<Node>> {
        let end = self.header_field()? as usize;
        let nprops = self.header_field()?;
        let _plen = self.header_field()?;
        let nlen = self.u8()? as usize;
        if end == 0 {
            return Ok(None);
        }
        if end > self.d.len() {
            bail!("FBX tronqué");
        }
        let name = String::from_utf8_lossy(self.take(nlen)?).into_owned();
        let mut props = Vec::with_capacity(nprops as usize);
        for _ in 0..nprops {
            props.push(self.prop()?);
        }
        let mut children = Vec::new();
        while self.o < end {
            match self.node()? {
                Some(c) => children.push(c),
                None => break,
            }
        }
        self.o = end;
        Ok(Some(Node { name, props, children }))
    }
}

fn parse_nodes(data: &[u8]) -> Result<Vec<Node>> {
    const MAGIC: &[u8] = b"Kaydara FBX Binary  \0";
    if !data.starts_with(MAGIC) {
        if data.starts_with(b"; FBX") || data.iter().take(200).all(|b| b.is_ascii()) {
            bail!("FBX texte (ASCII) non pris en charge : exportez-le au format binaire");
        }
        bail!("ce n'est pas un fichier FBX");
    }
    let version = u32::from_le_bytes(data.get(23..27).context("FBX tronqué")?.try_into()?);
    let mut r = Reader { d: data, o: 27, wide: version >= 7500 };
    let mut nodes = Vec::new();
    while r.o < data.len() {
        match r.node()? {
            Some(n) => nodes.push(n),
            None => break,
        }
    }
    Ok(nodes)
}

/// Une courbe d'animation (un canal scalaire).
#[derive(Clone, Debug, Default)]
struct Curve {
    times: Vec<f64>,
    values: Vec<f64>,
}

impl Curve {
    fn eval(&self, t: f64) -> Option<f64> {
        let n = self.times.len().min(self.values.len());
        if n == 0 {
            return None;
        }
        if n == 1 || t <= self.times[0] {
            return Some(self.values[0]);
        }
        if t >= self.times[n - 1] {
            return Some(self.values[n - 1]);
        }
        let i = self.times[..n].partition_point(|&k| k <= t).max(1) - 1;
        let (t0, t1) = (self.times[i], self.times[i + 1]);
        let f = if t1 > t0 { (t - t0) / (t1 - t0) } else { 0.0 };
        Some(self.values[i] + (self.values[i + 1] - self.values[i]) * f)
    }
}

/// Propriétés de transformation d'un nœud FBX.
#[derive(Clone, Debug)]
struct NodeXform {
    translation: DVec3,
    rotation: DVec3,
    scaling: DVec3,
    pre_rotation: DVec3,
    post_rotation: DVec3,
    rotation_offset: DVec3,
    rotation_pivot: DVec3,
    scaling_offset: DVec3,
    scaling_pivot: DVec3,
    rotation_order: i64,
    rotation_active: bool,
}

fn euler_deg(v: DVec3, order: i64) -> DQuat {
    let (x, y, z) = (v.x.to_radians(), v.y.to_radians(), v.z.to_radians());
    // ordre FBX « XYZ » : X appliqué en premier, soit Rz·Ry·Rx
    match order {
        1 => DQuat::from_euler(EulerRot::YZX, y, z, x),
        2 => DQuat::from_euler(EulerRot::XZY, x, z, y),
        3 => DQuat::from_euler(EulerRot::ZXY, z, x, y),
        4 => DQuat::from_euler(EulerRot::YXZ, y, x, z),
        5 => DQuat::from_euler(EulerRot::XYZ, x, y, z),
        _ => DQuat::from_euler(EulerRot::ZYX, z, y, x),
    }
}

impl NodeXform {
    /// Matrice locale : T · Roff · Rp · Rpre · R · Rpost⁻¹ · Rp⁻¹ · Soff · Sp · S · Sp⁻¹.
    fn local(&self, t: DVec3, r: DVec3, s: DVec3) -> DMat4 {
        let rot = |v: DVec3| DMat4::from_quat(euler_deg(v, 0));
        let (pre, post) = if self.rotation_active {
            (rot(self.pre_rotation), rot(self.post_rotation).inverse())
        } else {
            (DMat4::IDENTITY, DMat4::IDENTITY)
        };
        DMat4::from_translation(t)
            * DMat4::from_translation(self.rotation_offset)
            * DMat4::from_translation(self.rotation_pivot)
            * pre
            * DMat4::from_quat(euler_deg(r, self.rotation_order))
            * post
            * DMat4::from_translation(-self.rotation_pivot)
            * DMat4::from_translation(self.scaling_offset)
            * DMat4::from_translation(self.scaling_pivot)
            * DMat4::from_scale(s)
            * DMat4::from_translation(-self.scaling_pivot)
    }
}

/// Animation de squelette lue dans un FBX.
#[derive(Clone, Debug)]
pub struct FbxAnimation {
    /// nom de la pile d'animation (souvent « mixamo.com »)
    pub take: String,
    /// noms des nœuds, sans le suffixe de classe FBX (ex. « mixamorig:Hips »)
    pub names: Vec<String>,
    pub parents: Vec<Option<usize>>,
    /// matrices monde de repos (propriétés des nœuds, ou pose de liaison si le fichier en a une)
    pub rest_world: Vec<Mat4>,
    pub fps: f32,
    /// matrices monde de chaque nœud, image par image
    pub frames: Vec<Vec<Mat4>>,
}

impl FbxAnimation {
    pub fn duration(&self) -> f32 {
        (self.frames.len().max(1) - 1) as f32 / self.fps.max(1.0)
    }

    pub fn find(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n == name)
    }

    pub fn parse(data: &[u8]) -> Result<Self> {
        let top = parse_nodes(data)?;
        let get = |n: &str| top.iter().find(|x| x.name == n);
        let objects = get("Objects").context("FBX sans section Objects")?;
        let connections = get("Connections").context("FBX sans section Connections")?;

        // valeurs par défaut des nœuds (modèle de propriétés « FbxNode »)
        let mut template: HashMap<String, Vec<Prop>> = HashMap::new();
        if let Some(defs) = get("Definitions") {
            for ot in defs.children.iter().filter(|c| c.name == "ObjectType") {
                if ot.props.first().and_then(Prop::as_str) == Some("Model") {
                    for pt in ot.children.iter().filter(|c| c.name == "PropertyTemplate") {
                        template.extend(pt.properties70());
                    }
                }
            }
        }

        // ---- nœuds Model
        let mut ids = Vec::new();
        let mut names = Vec::new();
        let mut xforms = Vec::new();
        for m in objects.children.iter().filter(|c| c.name == "Model") {
            let id = m.props.first().and_then(Prop::as_i64).context("Model sans identifiant")?;
            let name = m.props.get(1).and_then(Prop::as_str).unwrap_or("").split('\0').next().unwrap_or("").to_string();
            let mut p = template.clone();
            p.extend(m.properties70());
            let v3 = |k: &str, d: DVec3| {
                p.get(k)
                    .filter(|v| v.len() >= 3)
                    .map(|v| DVec3::new(v[0].as_f64().unwrap_or(d.x), v[1].as_f64().unwrap_or(d.y), v[2].as_f64().unwrap_or(d.z)))
                    .unwrap_or(d)
            };
            let int = |k: &str| p.get(k).and_then(|v| v.first()).and_then(Prop::as_i64).unwrap_or(0);
            xforms.push(NodeXform {
                translation: v3("Lcl Translation", DVec3::ZERO),
                rotation: v3("Lcl Rotation", DVec3::ZERO),
                scaling: v3("Lcl Scaling", DVec3::ONE),
                pre_rotation: v3("PreRotation", DVec3::ZERO),
                post_rotation: v3("PostRotation", DVec3::ZERO),
                rotation_offset: v3("RotationOffset", DVec3::ZERO),
                rotation_pivot: v3("RotationPivot", DVec3::ZERO),
                scaling_offset: v3("ScalingOffset", DVec3::ZERO),
                scaling_pivot: v3("ScalingPivot", DVec3::ZERO),
                rotation_order: int("RotationOrder"),
                rotation_active: int("RotationActive") != 0,
            });
            ids.push(id);
            names.push(name);
        }
        if ids.is_empty() {
            bail!("aucun nœud de squelette dans ce FBX");
        }
        let index: HashMap<i64, usize> = ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();

        // ---- courbes et nœuds de courbes
        let mut curves: HashMap<i64, Curve> = HashMap::new();
        let mut curve_nodes: HashMap<i64, [Option<f64>; 3]> = HashMap::new();
        // piles d'animation (prises), dans l'ordre du fichier
        let mut stacks: Vec<(i64, String)> = Vec::new();
        for o in &objects.children {
            let id = o.props.first().and_then(Prop::as_i64).unwrap_or(0);
            match o.name.as_str() {
                "AnimationCurve" => {
                    let times = match o.child("KeyTime").and_then(|k| k.props.first()) {
                        Some(Prop::I64s(v)) => v.iter().map(|&t| t as f64 / TICKS_PER_SECOND).collect(),
                        _ => Vec::new(),
                    };
                    let values = match o.child("KeyValueFloat").and_then(|k| k.props.first()) {
                        Some(Prop::F64s(v)) => v.clone(),
                        _ => Vec::new(),
                    };
                    curves.insert(id, Curve { times, values });
                }
                "AnimationCurveNode" => {
                    let p = o.properties70();
                    let d = |k: &str| p.get(k).and_then(|v| v.first()).and_then(Prop::as_f64);
                    curve_nodes.insert(id, [d("d|X"), d("d|Y"), d("d|Z")]);
                }
                "AnimationStack" => {
                    let name = o.props.get(1).and_then(Prop::as_str).unwrap_or("").split('\0').next().unwrap_or("").to_string();
                    stacks.push((id, name));
                }
                _ => {}
            }
        }

        // ---- connexions
        let mut parents = vec![None; ids.len()];
        // nœud de courbes → (modèle, propriété)
        let mut cn_target: HashMap<i64, (usize, String)> = HashMap::new();
        // nœud de courbes → courbes par axe
        let mut cn_curves: HashMap<i64, [Option<i64>; 3]> = HashMap::new();
        // calques de chaque pile, et nœuds de courbes de chaque calque
        let mut stack_layers: HashMap<i64, Vec<i64>> = HashMap::new();
        let mut layer_of_cn: HashMap<i64, i64> = HashMap::new();
        for c in connections.children.iter().filter(|c| c.name == "C") {
            let kind = c.props.first().and_then(Prop::as_str).unwrap_or("");
            let (Some(child), Some(parent)) = (c.props.get(1).and_then(Prop::as_i64), c.props.get(2).and_then(Prop::as_i64)) else {
                continue;
            };
            let prop = c.props.get(3).and_then(Prop::as_str).unwrap_or("");
            match kind {
                "OO" => {
                    if let (Some(&ci), Some(&pi)) = (index.get(&child), index.get(&parent)) {
                        parents[ci] = Some(pi);
                    } else if stacks.iter().any(|(id, _)| *id == parent) {
                        stack_layers.entry(parent).or_default().push(child);
                    } else if curve_nodes.contains_key(&child) {
                        layer_of_cn.insert(child, parent);
                    }
                }
                "OP" => {
                    if let (true, Some(&mi)) = (curve_nodes.contains_key(&child), index.get(&parent)) {
                        cn_target.insert(child, (mi, prop.to_string()));
                    } else if curves.contains_key(&child) && curve_nodes.contains_key(&parent) {
                        let axis = match prop {
                            "d|X" => 0,
                            "d|Y" => 1,
                            "d|Z" => 2,
                            _ => continue,
                        };
                        cn_curves.entry(parent).or_default()[axis] = Some(child);
                    }
                }
                _ => {}
            }
        }
        // garde-fou contre les cycles
        for i in 0..parents.len() {
            let mut cur = parents[i];
            let mut steps = 0;
            while let Some(p) = cur {
                steps += 1;
                if p == i || steps > parents.len() {
                    parents[i] = None;
                    break;
                }
                cur = parents[p];
            }
        }

        // Pile retenue : celle dont le premier calque anime le plus de nœuds. Un export Mixamo
        // « With Skin » contient une prise vide « Take 001 » (venue du personnage) avant la
        // vraie animation « mixamo.com » ; à égalité, la première du fichier.
        let animated = |layer: i64| cn_target.keys().filter(|cn| layer_of_cn.get(cn) == Some(&layer)).count();
        let mut chosen: Option<(i64, &str, usize)> = None;
        for (id, name) in &stacks {
            let n = stack_layers.get(id).and_then(|l| l.first()).map_or(0, |&l| animated(l));
            if chosen.is_none_or(|(_, _, best)| n > best) {
                chosen = Some((*id, name, n));
            }
        }
        let stack_name = chosen.map(|(_, n, _)| n.to_string()).unwrap_or_default();
        let layers_of_stack: Vec<i64> = chosen.and_then(|(id, _, _)| stack_layers.get(&id).cloned()).unwrap_or_default();

        // canaux animés par nœud : 0 = translation, 1 = rotation, 2 = échelle
        type Channel = [(Option<f64>, Option<Curve>); 3];
        let mut channels: Vec<[Option<Channel>; 3]> = vec![[None, None, None]; ids.len()];
        for (cn, (mi, prop)) in &cn_target {
            // seulement le premier calque de la première pile, s'il y en a une
            if !layers_of_stack.is_empty() && layer_of_cn.get(cn).is_some_and(|l| *l != layers_of_stack[0]) {
                continue;
            }
            let slot = match prop.as_str() {
                "Lcl Translation" => 0,
                "Lcl Rotation" => 1,
                "Lcl Scaling" => 2,
                _ => continue,
            };
            let defaults = curve_nodes[cn];
            let cs = cn_curves.get(cn).copied().unwrap_or_default();
            channels[*mi][slot] = Some(std::array::from_fn(|a| (defaults[a], cs[a].and_then(|c| curves.get(&c).cloned()))));
        }

        // ---- plage de temps et cadence
        let all: Vec<&Curve> = channels.iter().flatten().flatten().flat_map(|c| c.iter().filter_map(|(_, c)| c.as_ref())).collect();
        let t0 = all.iter().filter_map(|c| c.times.first()).copied().fold(f64::INFINITY, f64::min);
        let t1 = all.iter().filter_map(|c| c.times.last()).copied().fold(f64::NEG_INFINITY, f64::max);
        let (t0, t1) = if t0.is_finite() && t1 > t0 { (t0, t1) } else { (0.0, 0.0) };
        let densest = all.iter().map(|c| c.times.len()).max().unwrap_or(0);
        let fps = if densest > 2 && t1 > t0 {
            let est = ((densest - 1) as f64 / (t1 - t0)).round();
            if (10.0..=240.0).contains(&est) {
                est
            } else {
                30.0
            }
        } else {
            30.0
        };
        let nframes = (((t1 - t0) * fps).round() as usize + 1).min(20_000);

        // ---- évaluation
        let order = eval_order(&parents);
        let world_at = |time: Option<f64>| -> Vec<Mat4> {
            let mut world = vec![DMat4::IDENTITY; ids.len()];
            for &i in &order {
                let x = &xforms[i];
                let sample = |slot: usize, base: DVec3| -> DVec3 {
                    let (Some(t), Some(ch)) = (time, &channels[i][slot]) else { return base };
                    let v = |a: usize| {
                        let (def, curve) = &ch[a];
                        curve.as_ref().and_then(|c| c.eval(t)).or(*def).unwrap_or(base[a])
                    };
                    DVec3::new(v(0), v(1), v(2))
                };
                let local = x.local(sample(0, x.translation), sample(1, x.rotation), sample(2, x.scaling));
                world[i] = match parents[i] {
                    Some(p) => world[p] * local,
                    None => local,
                };
            }
            world.iter().map(|m| m.as_mat4()).collect()
        };
        let props_world = world_at(None);
        let mut rest_world = props_world.clone();
        // une pose de liaison explicite (exports avec peau) prime sur les propriétés
        let mut in_pose = vec![false; ids.len()];
        for pose in objects.children.iter().filter(|c| c.name == "Pose") {
            for pn in pose.children.iter().filter(|c| c.name == "PoseNode") {
                let node = pn.child("Node").and_then(|n| n.props.first()).and_then(Prop::as_i64);
                let mat = pn.child("Matrix").and_then(|m| m.props.first());
                if let (Some(&i), Some(Prop::F64s(m))) = (node.and_then(|n| index.get(&n)), mat) {
                    if m.len() == 16 {
                        rest_world[i] = DMat4::from_cols_slice(m).as_mat4();
                        in_pose[i] = true;
                    }
                }
            }
        }
        // Nœuds absents de la pose de liaison (os terminaux « _end » des exports Blender, par
        // exemple) : on garde leur transformation locale, mais sous la pose de liaison du parent,
        // sinon ils restent dans la pose des propriétés, loin de leur parent.
        if in_pose.iter().any(|&b| b) {
            for &i in &order {
                if let (false, Some(p)) = (in_pose[i], parents[i]) {
                    rest_world[i] = rest_world[p] * (props_world[p].inverse() * props_world[i]);
                }
            }
        }
        let frames = (0..nframes).map(|f| world_at(Some(t0 + f as f64 / fps))).collect();
        Ok(Self { take: stack_name, names, parents, rest_world, fps: fps as f32, frames })
    }
}

fn eval_order(parents: &[Option<usize>]) -> Vec<usize> {
    let n = parents.len();
    let mut done = vec![false; n];
    let mut order = Vec::with_capacity(n);
    while order.len() < n {
        let before = order.len();
        for i in 0..n {
            if !done[i] && parents[i].is_none_or(|p| done[p]) {
                done[i] = true;
                order.push(i);
            }
        }
        if order.len() == before {
            break;
        }
    }
    order
}
