//! flcli : explorer et extraire les assets de PlanetSide 2.

use anyhow::{bail, Result};
use forgelight::GameData;
use glam::Vec3;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const USAGE: &str = "\
Usage : flcli <commande> <dossier_du_jeu> [args]

Commandes :
  namelist <jeu>                    construit la liste des noms (cache)
  list <jeu> [filtre]               liste les assets connus (filtre sur le nom)
  extract <jeu> <filtre> <dossier>  extrait les assets dont le nom contient <filtre>
  info <jeu> <modele.adr|.dme>      affiche maillages, matériaux et squelette
  obj <jeu> <modele> <sortie.obj>   exporte un modèle en OBJ (pose de repos)
  mrn <jeu> <reseau.mrn> [modele]   liste les animations d'un réseau Morpheme
  raw <jeu> <nom> <sortie>          extrait un asset par son nom exact (même hors namelist)
  versions <jeu> <nom> <dossier>    extrait chaque copie d'un asset présente dans les archives
  zone <jeu> <Nom> [filtre|@X,Z,R]  objets placés sur une carte (Indar, Amerish...)
  carte <jeu> <Nom> <sortie.png>    vue d'ensemble d'une carte avec ses objets
  png <jeu> <texture.dds> <sortie>  décode une texture en PNG (niveau 0)
  flore <jeu> <Nom> [a b]           plantes des .cnk0 et leur repère (toute la carte sans a b)
  terrain <jeu> <Nom> <a> <b>       lit des chunks .cnk1 et vérifie leur repère contre les objets
  normales <jeu> <Nom> <a> <b>      compare les normales d'un .cnk1 à celles de ses textures
  melange <jeu> <Nom> <a> <b>       cartes de mélange des écosystèmes d'un .cnk0 (PNG), vérifiées par la flore
  eco <jeu> <Nom> <numéro>          tuiles où un écosystème domine (position x, z du jeu)
  blocs <jeu> <fichier.cnkN>        décompresse un chunk dans <fichier.cnkN>.bin (dossier courant)
  magic <jeu>                       signatures des assets dont le nom est inconnu
  couleurs <jeu> <modele>...        canaux de la couleur des sommets (balancement des plantes)
  audit <jeu> [Nom...]              ce qui manque au décor des cartes (toutes sans Nom)
  fbx <fichier.fbx>                 squelette et animation d'un FBX (export Mixamo...)
";

fn open(game: &str) -> Result<GameData> {
    let mut gd = GameData::open(&PathBuf::from(game))?;
    let progress = Arc::new(AtomicUsize::new(0));
    let total = gd.index.len();
    let cache = forgelight::default_namelist_path();
    if !cache.exists() {
        eprintln!("Construction de la liste des noms ({total} assets), une seule fois...");
        let p = progress.clone();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let s2 = stop.clone();
        let h = std::thread::spawn(move || {
            while !s2.load(Ordering::Relaxed) {
                eprint!("\r  {}/{}", p.load(Ordering::Relaxed), total);
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            eprintln!();
        });
        let r = gd.load_namelist(&cache, &progress);
        stop.store(true, Ordering::Relaxed);
        h.join().ok();
        r?;
        eprintln!("{} noms trouvés -> {}", gd.index.names.len(), cache.display());
    } else {
        gd.load_namelist(&cache, &progress)?;
    }
    Ok(gd)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        print!("{USAGE}");
        return Ok(());
    }
    if args[0] == "fbx" {
        return fbx_info(&args[1]);
    }
    let gd = open(&args[1])?;
    match args[0].as_str() {
        "namelist" => {
            println!("{} assets, {} noms connus", gd.index.len(), gd.index.names.len());
            let mut by_ext = std::collections::BTreeMap::new();
            for n in gd.index.names.values() {
                let ext = n.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
                *by_ext.entry(ext).or_insert(0) += 1;
            }
            for (e, c) in by_ext {
                println!("  .{e:<8} {c}");
            }
        }
        "list" => {
            let filter = args.get(2).map(|s| s.to_ascii_lowercase()).unwrap_or_default();
            let mut names: Vec<_> = gd.index.names.values().filter(|n| n.to_ascii_lowercase().contains(&filter)).collect();
            names.sort();
            let out = std::io::stdout();
            let mut out = out.lock();
            for n in names {
                writeln!(out, "{n}")?;
            }
        }
        "extract" => {
            let (Some(filter), Some(dir)) = (args.get(2), args.get(3)) else { bail!("{USAGE}") };
            let filter = filter.to_ascii_lowercase();
            std::fs::create_dir_all(dir)?;
            let mut n = 0;
            for name in gd.index.names.values().filter(|n| n.to_ascii_lowercase().contains(&filter)) {
                let data = gd.read(name)?;
                std::fs::write(PathBuf::from(dir).join(name), data)?;
                n += 1;
            }
            println!("{n} fichiers extraits dans {dir}");
        }
        "info" => {
            let Some(name) = args.get(2) else { bail!("{USAGE}") };
            let m = gd.load_model(name)?;
            println!("Modèle {} : {} maillage(s), {} os", m.name, m.meshes.len(), m.skeleton.bones.len());
            for w in &m.warnings {
                println!("  ATTENTION : {w}");
            }
            for (i, mesh) in m.meshes.iter().enumerate() {
                println!(
                    "  mesh {i}: {} sommets, {} triangles, matériau {}, layout {}, diffuse {:?}",
                    mesh.vertices.len(),
                    mesh.indices.len() / 3,
                    mesh.material_definition,
                    mesh.layout,
                    mesh.diffuse
                );
                println!("    textures : {:?}", mesh.textures);
                println!("    relief {:?}, spéculaire {:?}", mesh.normal_map, mesh.specular);
                for (slot, t) in &mesh.texture_slots {
                    println!("    emplacement {:>11} : {t}", *slot as i32);
                }
                // étendue de chaque jeu d'UV
                for (k, get) in [|v: &forgelight::Vertex| v.uv, |v: &forgelight::Vertex| v.uv1, |v: &forgelight::Vertex| v.uv2]
                    .iter()
                    .enumerate()
                {
                    let (mut lo, mut hi) = (glam::Vec2::splat(f32::MAX), glam::Vec2::splat(f32::MIN));
                    for v in &mesh.vertices {
                        let p = glam::Vec2::from(get(v));
                        lo = lo.min(p);
                        hi = hi.max(p);
                    }
                    println!("    UV{k} : u {:.3}..{:.3}, v {:.3}..{:.3}", lo.x, hi.x, lo.y, hi.y);
                }
                // repère tangent comparé aux directions des UV (u vers la droite, v vers le bas)
                let (mut tu, mut bv, mut c) = (0.0f32, 0.0f32, 0);
                for t in mesh.indices.chunks_exact(3) {
                    let v = [t[0], t[1], t[2]].map(|i| mesh.vertices[i as usize]);
                    if v[0].tangent[3] == 0.0 {
                        continue;
                    }
                    let p = v.map(|v| glam::Vec3::from(v.position));
                    let uv = v.map(|v| glam::Vec2::from(v.uv));
                    let (e1, e2, d1, d2) = (p[1] - p[0], p[2] - p[0], uv[1] - uv[0], uv[2] - uv[0]);
                    let det = d1.x * d2.y - d2.x * d1.y;
                    if det.abs() < 1e-12 {
                        continue;
                    }
                    let dpdu = ((e1 * d2.y - e2 * d1.y) / det).normalize_or_zero();
                    let dpdv = ((e2 * d1.x - e1 * d2.x) / det).normalize_or_zero();
                    let n = glam::Vec3::from(v[0].normal);
                    let tg = glam::Vec4::from(v[0].tangent);
                    let b = n.cross(tg.truncate()) * tg.w;
                    tu += dpdu.dot(tg.truncate());
                    bv += dpdv.dot(b);
                    c += 1;
                }
                if c > 0 {
                    println!(
                        "    repère tangent : T·dP/du = {:+.2}, B·dP/dv = {:+.2} ({c} triangles)",
                        tu / c as f32,
                        bv / c as f32
                    );
                }
            }
            // contrôle du skinning : distance moyenne sommet <-> os principal
            let bw: Vec<_> = m.skeleton.bones.iter().map(|b| b.bind_world.w_axis.truncate()).collect();
            if !bw.is_empty() {
                let (mut d, mut c) = (0.0f32, 0);
                for mesh in &m.meshes {
                    for v in &mesh.vertices {
                        let j = v.joints[0] as usize;
                        if j < bw.len() {
                            d += glam::Vec3::from(v.position).distance(bw[j]);
                            c += 1;
                        }
                    }
                }
                println!("  distance moyenne sommet/os principal : {:.3} m", d / c.max(1) as f32);
            }
            for (i, b) in m.skeleton.bones.iter().enumerate() {
                let p = b.parent.map(|p| m.skeleton.bones[p].name.clone()).unwrap_or_else(|| "-".into());
                let pos = b.bind_world.w_axis;
                println!("  os {i:3} {:<20} parent {:<20} pos ({:.3}, {:.3}, {:.3})", b.name, p, pos.x, pos.y, pos.z);
            }
        }
        "uvdump" => {
            // triangles de chaque jeu d'UV en texte : « k u0 v0 u1 v1 u2 v2 » (analyse des masques)
            let (Some(name), Some(out)) = (args.get(2), args.get(3)) else { bail!("{USAGE}") };
            let m = gd.load_model(name)?;
            let mut f = std::io::BufWriter::new(std::fs::File::create(out)?);
            for mesh in &m.meshes {
                for t in mesh.indices.chunks_exact(3) {
                    let v = [t[0], t[1], t[2]].map(|i| mesh.vertices[i as usize]);
                    for (k, uv) in [v.map(|v| v.uv), v.map(|v| v.uv1), v.map(|v| v.uv2)].iter().enumerate() {
                        writeln!(f, "{k} {} {} {} {} {} {}", uv[0][0], uv[0][1], uv[1][0], uv[1][1], uv[2][0], uv[2][1])?;
                    }
                }
            }
        }
        "obj" => {
            let (Some(name), Some(out)) = (args.get(2), args.get(3)) else { bail!("{USAGE}") };
            let m = gd.load_model(name)?;
            let mut f = std::io::BufWriter::new(std::fs::File::create(out)?);
            let mut base = 1;
            for (i, mesh) in m.meshes.iter().enumerate() {
                writeln!(f, "o mesh{i}")?;
                for v in &mesh.vertices {
                    writeln!(f, "v {} {} {}", v.position[0], v.position[1], v.position[2])?;
                }
                for v in &mesh.vertices {
                    writeln!(f, "vt {} {}", v.uv[0], 1.0 - v.uv[1])?;
                }
                for v in &mesh.vertices {
                    writeln!(f, "vn {} {} {}", v.normal[0], v.normal[1], v.normal[2])?;
                }
                for t in mesh.indices.chunks_exact(3) {
                    let (a, b, c) = (t[0] + base, t[1] + base, t[2] + base);
                    writeln!(f, "f {a}/{a}/{a} {b}/{b}/{b} {c}/{c}/{c}")?;
                }
                base += mesh.vertices.len() as u32;
            }
            println!("écrit {out}");
        }
        "mrn" => {
            let Some(name) = args.get(2) else { bail!("{USAGE}") };
            let net = gd.load_network(name)?;
            println!("{} squelette(s), {} table(s), {} clips", net.rigs.len(), net.maps.len(), net.clips.len());
            let rig = net.main_rig().unwrap();
            println!("squelette principal : {} os", rig.names.len());
            let clips = net.main_clips();
            println!("{} clips pour ce squelette", clips.len());
            let filter = std::env::var("FILTRE").unwrap_or_default().to_ascii_lowercase();
            for c in clips.iter().filter(|c| c.name.to_ascii_lowercase().contains(&filter)).take(40) {
                println!("  {:<60} {:<20} {:.2}s", c.name, c.category, c.duration);
            }
            // comparaison avec un modèle
            if let Some(model) = args.get(3) {
                let m = gd.load_model(model)?;
                let mut n = 0;
                let mut worst = 0.0f32;
                for b in &m.skeleton.bones {
                    if let Some(i) = rig.names.iter().position(|r| forgelight::hash::jenkins_upper(r) == b.hash) {
                        let ang = b.rest_local.rotation.angle_between(rig.bind_rot[i]).to_degrees();
                        let dp = b.rest_local.translation.distance(rig.bind_pos[i]);
                        worst = worst.max(ang);
                        if ang > 1.0 || dp > 0.005 {
                            println!("  {:<16} écart rot {:6.2}°  pos {:.4}", rig.names[i], ang, dp);
                        }
                        n += 1;
                    }
                }
                println!("{n}/{} os du modèle trouvés dans le squelette, écart max {worst:.2}°", m.skeleton.bones.len());
            }
            if let Some(c) = clips.first() {
                let t = std::time::Instant::now();
                let a = net.decode(c)?;
                println!("décodage de {} : {} images, {} canaux en {:?}", c.name, a.frames.len(), a.num_channels, t.elapsed());
            }
        }
        "clip" => {
            // clip <jeu> <reseau> <nom> : écart de chaque os à l'identité et à la liaison
            let (Some(net_name), Some(clip_name)) = (args.get(2), args.get(3)) else { bail!("{USAGE}") };
            let net = gd.load_network(net_name)?;
            let rig = net.main_rig().unwrap();
            let clip = net.main_clips().into_iter().find(|c| c.name.eq_ignore_ascii_case(clip_name)).cloned();
            let Some(clip) = clip else { bail!("clip introuvable") };
            let bc = net.bone_frames(&clip)?;
            for fi in [0, bc.frames.len() / 2, bc.frames.len() - 1] {
                println!("image {fi} :");
                for (i, (q, p)) in bc.frames[fi].iter().enumerate() {
                    let a_id = q.angle_between(glam::Quat::IDENTITY).to_degrees();
                    let a_bind = q.angle_between(rig.bind_rot[i]).to_degrees();
                    println!("  {:<16} vs identité {:6.1}°  vs liaison {:6.1}°  pos ({:.3},{:.3},{:.3}) liaison ({:.3},{:.3},{:.3})",
                        rig.names[i], a_id, a_bind, p.x, p.y, p.z, rig.bind_pos[i].x, rig.bind_pos[i].y, rig.bind_pos[i].z);
                }
            }
        }
        "addtest" => {
            // addtest <jeu> <reseau> <base> <additif> : quel ordre de composition donne la pose de liaison ?
            let (Some(net_name), Some(base), Some(add)) = (args.get(2), args.get(3), args.get(4)) else { bail!("{USAGE}") };
            let net = gd.load_network(net_name)?;
            let rig = net.main_rig().unwrap();
            let find = |n: &str| net.main_clips().into_iter().find(|c| c.name.eq_ignore_ascii_case(n)).cloned();
            let (Some(b), Some(a)) = (find(base), find(add)) else { bail!("clip introuvable") };
            let fb = net.bone_frames(&b)?;
            let fa = net.bone_frames(&a)?;
            let (mut e1, mut e2, mut e0) = (0.0f32, 0.0f32, 0.0f32);
            let last = fa.frames.len() - 1;
            for i in 0..rig.names.len() {
                let qb = fb.frames[0][i].0;
                let qa = fa.frames[last][i].0;
                let bind = rig.bind_rot[i];
                e0 += qb.angle_between(bind).to_degrees();
                e1 += (qb * qa).angle_between(bind).to_degrees();
                e2 += (qa * qb).angle_between(bind).to_degrees();
            }
            let n = rig.names.len() as f32;
            println!("écart moyen à la liaison : base seule {:.2}°, base*add {:.2}°, add*base {:.2}°", e0 / n, e1 / n, e2 / n);
        }
        "sansnom" => {
            // sansnom <jeu> [filtre] : acteurs (.adr) et modèles (.dme) dont le nom est inconnu
            let filter = args.get(2).map(|s| s.to_ascii_lowercase()).unwrap_or_default();
            let mut n = 0;
            for (h, e) in &gd.index.entries {
                if gd.index.names.contains_key(h) {
                    continue;
                }
                let Ok(data) = gd.index.read_entry(e) else { continue };
                let head = &data[..data.len().min(16)];
                if head.starts_with(b"<ActorRuntime") {
                    let adr = forgelight::adr::Adr::parse(&data).unwrap_or_default();
                    let tex: Vec<String> = adr.textures.iter().map(|t| t.texture.clone()).collect();
                    let line = format!("ADR {h:016X} base={:?} tex={:?}", adr.base_model, tex);
                    if line.to_ascii_lowercase().contains(&filter) {
                        println!("{line}");
                        n += 1;
                    }
                } else if head.starts_with(b"DMOD") {
                    if let Ok(d) = forgelight::dme::Dme::parse(&data) {
                        let line = format!("DME {h:016X} tex={:?} os={}", d.texture_names, d.bones.len());
                        if line.to_ascii_lowercase().contains(&filter) {
                            println!("{line}");
                            n += 1;
                        }
                    }
                }
            }
            eprintln!("{n} résultats");
        }
        "raw" => {
            // raw <jeu> <nom> <sortie> : extrait un asset par son nom, même absent de la namelist
            let (Some(name), Some(out)) = (args.get(2), args.get(3)) else { bail!("{USAGE}") };
            let data = gd.read(name)?;
            std::fs::write(out, &data)?;
            println!("{name} : {} octets -> {out}", data.len());
        }
        "versions" => {
            // versions <jeu> <nom> <dossier> : un même nom peut être dans plusieurs archives (le
            // pack commun et celui d'un continent) ; l'index n'en garde qu'une
            let (Some(name), Some(dir)) = (args.get(2), args.get(3)) else { bail!("{USAGE}") };
            std::fs::create_dir_all(dir)?;
            let h = forgelight::hash::name_hash(name);
            for (i, p) in gd.index.packs.iter().enumerate() {
                let pack = forgelight::pack::Pack2::open(p, i as u16)?;
                for e in pack.entries.iter().filter(|e| e.name_hash == h) {
                    let data = gd.index.read_entry(e)?;
                    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    let out = PathBuf::from(dir).join(format!("{stem}__{name}"));
                    std::fs::write(&out, &data)?;
                    let used = gd.index.entries.get(&h).is_some_and(|u| u.pack == e.pack && u.offset == e.offset);
                    println!("{} : {} octets{}", p.display(), data.len(), if used { " (lue)" } else { "" });
                }
            }
        }
        "zone" => {
            // zone <jeu> <Nom> [filtre] : contenu d'une carte
            let Some(name) = args.get(2) else { bail!("{USAGE}") };
            let file = if name.contains('.') { name.clone() } else { format!("{name}.zone") };
            let z = forgelight::zone::Zone::parse(&gd.read(&file)?)?;
            println!("{file} : {:?}", z.header);
            println!("{} acteurs, {} instances, côté {} m", z.objects.len(), z.instance_count(), z.extent());
            for e in &z.ecos {
                println!("  éco {:>2} {:<24} {} / {} ×{} ({})", e.index, e.name, e.color_nx, e.spec_blend_ny, e.detail_repeat, e.physics);
            }
            for f in &z.floras {
                let ok = match gd.load_model(&f.model) {
                    Ok(m) => format!("{} maillages", m.meshes.len()),
                    Err(e) => format!("ERREUR {e:#}"),
                };
                println!("  flore {:<20} {:<40} {:<40} {} {:?} : {ok}", f.name, f.model, f.texture, f.flag, f.unknown);
            }
            let filter = args.get(3).map(|s| s.to_ascii_lowercase()).unwrap_or_default();
            // « @X,Z,R » : acteurs ayant une instance à moins de R m du point (X, Z)
            let near: Option<Vec<f32>> = filter.strip_prefix('@').map(|p| p.split(',').filter_map(|v| v.parse().ok()).collect());
            let keep = |o: &&forgelight::zone::ZoneObject| match &near {
                Some(v) if v.len() == 3 => o.instances.iter().any(|i| (i.position.x - v[0]).hypot(i.position.z - v[1]) <= v[2]),
                Some(_) => false,
                None => o.actor.to_ascii_lowercase().contains(&filter),
            };
            let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
            let mut objs: Vec<_> = z.objects.iter().filter(keep).collect();
            objs.sort_by_key(|o| std::cmp::Reverse(o.instances.len()));
            let (mut missing, mut no_base) = (0, 0);
            for o in &objs {
                for i in &o.instances {
                    lo = lo.min(i.position);
                    hi = hi.max(i.position);
                }
                match gd.read(&o.actor) {
                    Err(_) => missing += 1,
                    Ok(d) => {
                        if forgelight::adr::Adr::parse(&d).ok().and_then(|a| a.base_model).is_none() {
                            no_base += 1
                        }
                    }
                }
            }
            println!("étendue des positions : {lo:.0} .. {hi:.0}");
            println!("acteurs introuvables : {missing}, sans modèle de base : {no_base}");
            for o in objs.iter().take(if filter.is_empty() { 40 } else { 400 }) {
                let i = &o.instances[0];
                println!(
                    "{:>6} × {:<60} vue {:>5.0} m  ex. pos {:.1} rot {:.3} éch {:.2}",
                    o.instances.len(),
                    o.actor,
                    o.render_distance,
                    i.position,
                    i.rotation,
                    i.scale
                );
            }
        }
        "flore" => {
            // flore <jeu> <Nom> [a b] : plantes des .cnk0, posées sur les triangles de leur tuile ;
            // vérifie le repère contre le terrain du .cnk1 (hauteur) et l'altitude imposée des
            // couches (nénuphars 0 à 4 m…). Sans (a, b) : toute la carte.
            let Some(name) = args.get(2) else { bail!("{USAGE}") };
            let z = forgelight::zone::Zone::parse(&gd.read(&format!("{name}.zone"))?)?;
            let half = (z.header.chunks_x as i32 * z.header.tile_size as i32) / 64 / 2;
            let list: Vec<(i32, i32)> = match (args.get(3), args.get(4)) {
                (Some(a), Some(b)) => vec![(a.parse()?, b.parse()?)],
                _ => (-half..half).step_by(4).flat_map(|a| (-half..half).step_by(4).map(move |b| (a, b))).collect(),
            };
            let (mut chunks, mut plants, mut lost) = (0, 0usize, 0usize);
            let mut per_layer: std::collections::BTreeMap<(u32, usize), usize> = Default::default();
            // écarts de hauteur au terrain .cnk1 : repère retenu, puis axes locaux échangés
            let (mut err, mut err_swap) = (Vec::new(), Vec::new());
            let (mut in_range, mut restricted) = (0usize, 0usize);
            for (a, b) in list {
                let Ok(f) = gd.read(&format!("{name}_{a}_{b}.cnk0")) else { continue };
                let tiles = forgelight::flora::parse_cnk0(&f)?;
                let chunk = gd.read(&forgelight::terrain::chunk_name(name, a, b, 1)).and_then(|d| forgelight::terrain::Chunk::parse(&d)).ok();
                chunks += 1;
                for t in &tiles {
                    for te in &t.ecos {
                        let eco = z.ecos.iter().find(|e| e.index == te.eco);
                        for (li, inst) in te.layers.iter().enumerate() {
                            plants += inst.len();
                            *per_layer.entry((te.eco, li)).or_insert(0) += inst.len();
                            let layer = eco.and_then(|e| e.flora.get(li));
                            let limited = layer.is_some_and(|l| !(l.min_elevation < -1000.0 && l.max_elevation > 1000.0));
                            for p in inst.iter().step_by(if limited { 1 } else { 40 }) {
                                let Some([x, h, zz]) = t.place(p) else {
                                    lost += 1;
                                    continue;
                                };
                                if let (true, Some(l)) = (limited, layer) {
                                    restricted += 1;
                                    if h >= l.min_elevation - 0.5 && h <= l.max_elevation + 0.5 {
                                        in_range += 1;
                                    }
                                }
                                let Some(c) = &chunk else { continue };
                                if let Some(g) = c.height_at(a, b, x, zz) {
                                    err.push((g - h).abs());
                                }
                                // axes locaux échangés
                                let (lx, lz) = (x - t.y as f32 * 64.0, zz - t.x as f32 * 64.0);
                                if let Some(g) = c.height_at(a, b, t.y as f32 * 64.0 + lz, t.x as f32 * 64.0 + lx) {
                                    err_swap.push((g - h).abs());
                                }
                            }
                        }
                    }
                }
            }
            println!("{chunks} chunks, {plants} plantes ({lost} hors de leur triangle)");
            for ((e, li), n) in &per_layer {
                let eco = z.ecos.iter().find(|x| x.index == *e);
                let layer = eco.and_then(|x| x.flora.get(*li));
                println!(
                    "  {:<20} couche {li} {:<14} {n:>9} plantes{}",
                    eco.map_or("?", |x| x.name.as_str()),
                    layer.map_or("?", |l| l.flora.as_str()),
                    layer.map_or(String::new(), |l| format!(" (altitude {:.0} à {:.0})", l.min_elevation, l.max_elevation))
                );
            }
            for (label, e) in [("repère retenu", &mut err), ("axes locaux échangés", &mut err_swap)] {
                e.sort_by(f32::total_cmp);
                if !e.is_empty() {
                    println!("{label} : écart au terrain .cnk1 médian {:.2} m, 90 % {:.2} m ({} points)", e[e.len() / 2], e[e.len() * 9 / 10], e.len());
                }
            }
            println!("couches à altitude imposée : {in_range}/{restricted} plantes dans la plage");
        }
        "terrain" => {
            // terrain <jeu> <Nom> <a> <b> : lit des chunks .cnk1 autour de (a, b) et cherche le repère
            // (axes, ordre des quarts) où les objets posés au sol tombent sur le terrain
            let (Some(name), Some(a0), Some(b0)) = (args.get(2), args.get(3), args.get(4)) else { bail!("{USAGE}") };
            let (a0, b0): (i32, i32) = (a0.parse()?, b0.parse()?);
            let z = forgelight::zone::Zone::parse(&gd.read(&format!("{name}.zone"))?)?;
            let ground: Vec<Vec3> = z
                .objects
                .iter()
                .filter(|o| {
                    let l = o.actor.to_ascii_lowercase();
                    l.contains("flora") || l.contains("rock")
                })
                .flat_map(|o| o.instances.iter().map(|i| i.position))
                .collect();
            // chunks lus (a, b, chunk)
            let mut chunks = Vec::new();
            for da in [-4, 0, 4] {
                for db in [-4, 0, 4] {
                    let n = forgelight::terrain::chunk_name(name, a0 + da, b0 + db, 1);
                    let t = std::time::Instant::now();
                    match gd.read(&n).and_then(|d| forgelight::terrain::Chunk::parse(&d)) {
                        Ok(c) => {
                            let nv: usize = c.quads.iter().map(|q| q.vertices.len()).sum();
                            println!("{n} : {} quarts, {nv} sommets ({:.0} ms)", c.quads.len(), t.elapsed().as_secs_f32() * 1000.0);
                            chunks.push((a0 + da, b0 + db, c));
                        }
                        Err(e) => println!("{n} : {e:#}"),
                    }
                }
            }
            let perms: Vec<[usize; 4]> = {
                let mut v = Vec::new();
                for p0 in 0..4 {
                    for p1 in 0..4 {
                        for p2 in 0..4 {
                            for p3 in 0..4 {
                                let p = [p0, p1, p2, p3];
                                if (0..4).all(|k| p.contains(&k)) {
                                    v.push(p);
                                }
                            }
                        }
                    }
                }
                v
            };
            let mut results = Vec::new();
            for name_swap in [false, true] {
                for perm in &perms {
                    for (vswap, fu, fw) in (0..8).map(|k| (k & 1 != 0, k & 2 != 0, k & 4 != 0)) {
                        // hauteurs par point de grille (2 m), toutes les cellules connues
                        let mut grid: std::collections::HashMap<(i32, i32), f32> = Default::default();
                        for (a, b, c) in &chunks {
                            let (x0, z0) = if name_swap { (*a * 64, *b * 64) } else { (*b * 64, *a * 64) };
                            for (q, quad) in c.quads.iter().enumerate() {
                                let cell = perm[q];
                                let (ci, cj) = ((cell & 1) as i32, (cell >> 1) as i32);
                                for v in &quad.vertices {
                                    let (mut u, mut w) = (v.x as i32, v.y as i32);
                                    if vswap {
                                        std::mem::swap(&mut u, &mut w);
                                    }
                                    if fu {
                                        u = 64 - u;
                                    }
                                    if fw {
                                        w = 64 - w;
                                    }
                                    let gx = x0 / 2 + ci * 64 + u;
                                    let gz = z0 / 2 + cj * 64 + w;
                                    grid.insert((gx, gz), v.height_near as f32 / 32.0);
                                }
                            }
                        }
                        let mut errs: Vec<f32> = Vec::new();
                        for p in &ground {
                            let (gx, gz) = ((p.x / 2.0).round() as i32, (p.z / 2.0).round() as i32);
                            let mut best: Option<f32> = None;
                            'r: for r in 0..4 {
                                for dx in -r..=r {
                                    for dz in -r..=r {
                                        if let Some(h) = grid.get(&(gx + dx, gz + dz)) {
                                            best = Some(*h);
                                            break 'r;
                                        }
                                    }
                                }
                            }
                            if let Some(h) = best {
                                errs.push((h - p.y).abs());
                            }
                        }
                        if errs.len() < 20 {
                            continue;
                        }
                        errs.sort_by(f32::total_cmp);
                        results.push((errs[errs.len() / 2], errs.len(), name_swap, *perm, vswap, fu, fw));
                    }
                }
            }
            results.sort_by(|a, b| a.0.total_cmp(&b.0));
            for r in results.iter().take(8) {
                println!(
                    "écart médian {:.2} m sur {} objets : noms {} ; quarts {:?} ; sommets échangés {} inversés {} {}",
                    r.0,
                    r.1,
                    if r.2 { "a = x, b = z" } else { "a = z, b = x" },
                    r.3,
                    r.4,
                    r.5,
                    r.6
                );
            }
        }
        "normales" => {
            // normales <jeu> <Nom> <a> <b> : compare la normale du maillage d'un chunk .cnk1 à celle
            // rangée dans l'alpha des textures ColorNX / SpecNY, pour chaque convention possible
            let (Some(name), Some(a), Some(b)) = (args.get(2), args.get(3), args.get(4)) else { bail!("{USAGE}") };
            let (a, b): (i32, i32) = (a.parse()?, b.parse()?);
            let n = forgelight::terrain::chunk_name(name, a, b, 1);
            let c = forgelight::terrain::Chunk::parse(&gd.read(&n)?)?;
            // (convention, somme des cos, nombre)
            let mut score = [0.0f64; 8];
            let mut count = 0usize;
            for q in 0..c.quads.len() {
                if c.quads[q].indices.is_empty() {
                    continue;
                }
                let m = c.quad_mesh(a, b, q);
                let cn = forgelight::dds::decode(&c.quads[q].color_dds)?;
                let sn = forgelight::dds::decode(&c.quads[q].spec_dds)?;
                for (k, (nrm, uv)) in m.normals.iter().zip(&m.uvs).enumerate() {
                    if k % 7 != 0 || nrm.y > 0.97 {
                        continue;
                    }
                    let px = |img: &forgelight::dds::Image| {
                        let (w, h) = (img.width as f32, img.height as f32);
                        let (x, y) = (((uv.x * w) as u32).min(img.width - 1), ((uv.y * h) as u32).min(img.height - 1));
                        img.mips[0][((y * img.width + x) * 4 + 3) as usize] as f32 / 127.5 - 1.0
                    };
                    let (tx, ty) = (px(&cn), px(&sn));
                    for conv in 0..8 {
                        let (mut u, mut v) = if conv & 1 == 0 { (tx, ty) } else { (ty, tx) };
                        if conv & 2 != 0 {
                            u = -u;
                        }
                        if conv & 4 != 0 {
                            v = -v;
                        }
                        let up = (1.0 - u * u - v * v).max(0.0).sqrt();
                        let t = Vec3::new(u, up, v).normalize_or_zero();
                        score[conv] += t.dot(*nrm) as f64;
                    }
                    count += 1;
                }
            }
            println!("{n} : {count} sommets en pente");
            let mut s: Vec<(usize, f64)> = score.iter().copied().enumerate().collect();
            s.sort_by(|x, y| y.1.total_cmp(&x.1));
            for (conv, v) in s {
                println!(
                    "cos moyen {:.3} : x monde = {}{}, z monde = {}{}",
                    v / count.max(1) as f64,
                    if conv & 2 != 0 { "-" } else { "+" },
                    if conv & 1 == 0 { "NX" } else { "NY" },
                    if conv & 4 != 0 { "-" } else { "+" },
                    if conv & 1 == 0 { "NY" } else { "NX" },
                );
            }
        }
        "melange" => {
            // melange <jeu> <Nom> <a> <b> : cartes de mélange des écosystèmes des tuiles d'un .cnk0
            // (une PNG par tuile, dans le dossier courant) et leurs écosystèmes
            let (Some(name), Some(a), Some(b)) = (args.get(2), args.get(3), args.get(4)) else { bail!("{USAGE}") };
            let z = forgelight::zone::Zone::parse(&gd.read(&format!("{name}.zone"))?)?;
            let tiles = forgelight::flora::parse_cnk0(&gd.read(&format!("{name}_{a}_{b}.cnk0"))?)?;
            for t in &tiles {
                let names: Vec<String> = t
                    .ecos
                    .iter()
                    .map(|e| format!("{}:{}", e.eco, z.ecos.iter().find(|x| x.index == e.eco).map_or("?", |x| x.name.as_str())))
                    .collect();
                let head = if t.blend_dds.len() >= 128 { String::from_utf8_lossy(&t.blend_dds[84..88]).to_string() } else { "-".into() };
                println!(
                    "tuile x {} y {} : {} octets ({head}), fin {} octets {:02x?}, écosystèmes {names:?}",
                    t.x,
                    t.y,
                    t.blend_dds.len(),
                    t.tail.len(),
                    &t.tail[..t.tail.len().min(24)]
                );
                if t.blend_dds.is_empty() {
                    continue;
                }
                let img = forgelight::dds::decode(&t.blend_dds)?.to_rgba()?;
                let px = &img.mips[0][..(img.width * img.height * 4) as usize];
                let mut sum = [0u64; 4];
                for p in px.chunks_exact(4) {
                    for k in 0..4 {
                        sum[k] += p[k] as u64;
                    }
                }
                let n = (img.width * img.height) as u64;
                println!("    {}×{} px, moyennes RGBA {:?}", img.width, img.height, sum.map(|s| s / n));
                // valeur moyenne de chaque canal sous les plantes de chaque écosystème
                // (orientation : colonne = z local, ligne = x local ; puis transposée)
                let w = img.width as usize;
                for te in &t.ecos {
                    let mut acc = [[0f64; 4]; 2];
                    let mut cnt = 0f64;
                    for inst in te.layers.iter().flatten().step_by(7) {
                        let Some([x, _, zz]) = t.place(inst) else { continue };
                        let (lx, lz) = (x - t.y as f32 * 64.0, zz - t.x as f32 * 64.0);
                        for (o, (col, row)) in [(lz, lx), (lx, lz)].into_iter().enumerate() {
                            let (c, r) = ((col.clamp(0.0, 63.9)) as usize, (row.clamp(0.0, 63.9)) as usize);
                            let p = &px[(r * w + c) * 4..(r * w + c) * 4 + 4];
                            for k in 0..4 {
                                acc[o][k] += p[k] as f64;
                            }
                        }
                        cnt += 1.0;
                    }
                    if cnt > 0.0 {
                        let f = |a: [f64; 4]| a.map(|v| (v / cnt) as u32);
                        let pos = t.tail.iter().position(|&e| e as u32 == te.eco);
                        println!("      éco {:>2} (fin #{pos:?}) {cnt:>6} plantes : {:?}  transposé {:?}", te.eco, f(acc[0]), f(acc[1]));
                    }
                }
                let out = format!("melange_{name}_{}_{}.png", t.x, t.y);
                let f = std::io::BufWriter::new(std::fs::File::create(&out)?);
                let mut enc = png::Encoder::new(f, img.width, img.height);
                enc.set_color(png::ColorType::Rgba);
                enc.write_header()?.write_image_data(px)?;
            }
            // couleurs des quarts du .cnk1 correspondant, pour comparer
            if let Ok(c) = gd.read(&forgelight::terrain::chunk_name(name, a.parse()?, b.parse()?, 1)).and_then(|d| forgelight::terrain::Chunk::parse(&d)) {
                for (q, quad) in c.quads.iter().enumerate() {
                    let img = forgelight::dds::decode(&quad.color_dds)?.to_rgba()?;
                    let px = &img.mips[0][..(img.width * img.height * 4) as usize];
                    let f = std::io::BufWriter::new(std::fs::File::create(format!("couleur_{name}_q{q}.png"))?);
                    let mut enc = png::Encoder::new(f, img.width, img.height);
                    enc.set_color(png::ColorType::Rgba);
                    enc.write_header()?.write_image_data(px)?;
                }
            }
        }
        "eco" => {
            // eco <jeu> <Nom> <numéro> : tuiles où cet écosystème domine (fond ou canal fort)
            let (Some(name), Some(num)) = (args.get(2), args.get(3)) else { bail!("{USAGE}") };
            let num: u32 = num.parse()?;
            let z = forgelight::zone::Zone::parse(&gd.read(&format!("{name}.zone"))?)?;
            let half = (z.header.chunks_x as i32 * z.header.tile_size as i32) / 64 / 2;
            let mut hits = Vec::new();
            for a in (-half..half).step_by(4) {
                for b in (-half..half).step_by(4) {
                    let Ok(f) = gd.read(&format!("{name}_{a}_{b}.cnk0")) else { continue };
                    let Ok(tiles) = forgelight::flora::parse_cnk0(&f) else { continue };
                    for t in &tiles {
                        let Some(k) = t.ecos.iter().position(|e| e.eco == num) else { continue };
                        if k == 0 || t.blend_dds.is_empty() {
                            continue;
                        }
                        let Ok(img) = forgelight::dds::decode(&t.blend_dds).and_then(|i| i.to_rgba()) else { continue };
                        let px = &img.mips[0][..(img.width * img.height * 4) as usize];
                        let m = px.chunks_exact(4).map(|p| p[(k - 1).min(3)] as u64).sum::<u64>() / (px.len() / 4) as u64;
                        hits.push((m, t.y * 64 + 32, t.x * 64 + 32));
                    }
                }
            }
            hits.sort_by(|a, b| b.cmp(a));
            for (m, x, zz) in hits.iter().take(15) {
                println!("x {x} z {zz} : présence moyenne {m}");
            }
        }
        "blocs" => {
            // blocs <jeu> <fichier.cnkN> : tailles et début des blocs d'un chunk décompressé
            let Some(name) = args.get(2) else { bail!("{USAGE}") };
            let file = gd.read(name)?;
            let out_len = u32::from_le_bytes(file[8..12].try_into().unwrap()) as usize;
            let comp_len = u32::from_le_bytes(file[12..16].try_into().unwrap()) as usize;
            let d = forgelight::lzham::decompress_zlib(&file[16..16 + comp_len], out_len)?;
            std::fs::write(format!("{name}.bin"), &d)?;
            println!("{} octets décompressés -> {name}.bin", d.len());
        }
        "carte" => {
            // carte <jeu> <Nom> <sortie.png> : vue d'ensemble, avec les objets en points
            let (Some(name), Some(out)) = (args.get(2), args.get(3)) else { bail!("{USAGE}") };
            let z = forgelight::zone::Zone::parse(&gd.read(&format!("{name}.zone"))?)?;
            let ov = z.overview(name, |t| gd.load_texture(t).ok()).ok_or_else(|| anyhow::anyhow!("pas de tuiles"))?;
            let mut rgba = ov.rgba.clone();
            let s = ov.size as i32;
            for o in &z.objects {
                let a = o.actor.to_ascii_lowercase();
                let color: [u8; 3] = if a.contains("flora") {
                    continue;
                } else if a.contains("rock") {
                    [80, 160, 255]
                } else {
                    [255, 40, 40]
                };
                for i in &o.instances {
                    let [u, v] = ov.to_uv(i.position.x, i.position.z);
                    let (px, py) = ((u * s as f32) as i32, (v * s as f32) as i32);
                    if px >= 0 && py >= 0 && px < s && py < s {
                        let k = ((py * s + px) * 4) as usize;
                        rgba[k..k + 3].copy_from_slice(&color);
                    }
                }
            }
            let f = std::io::BufWriter::new(std::fs::File::create(out)?);
            let mut enc = png::Encoder::new(f, ov.size, ov.size);
            enc.set_color(png::ColorType::Rgba);
            enc.write_header()?.write_image_data(&rgba)?;
            println!("{out} : {}×{} px, origine {:?}, côté {} m", ov.size, ov.size, ov.origin, ov.extent);
        }
        "png" => {
            // png <jeu> <texture.dds> <sortie.png> : décode une texture (mip 0) en PNG RGBA
            let (Some(name), Some(out)) = (args.get(2), args.get(3)) else { bail!("{USAGE}") };
            let img = gd.load_texture(name)?.to_rgba()?;
            let f = std::io::BufWriter::new(std::fs::File::create(out)?);
            let mut enc = png::Encoder::new(f, img.width, img.height);
            enc.set_color(png::ColorType::Rgba);
            let n = (img.width * img.height * 4) as usize;
            enc.write_header()?.write_image_data(&img.mips[0][..n])?;
            println!("{out} : {}×{} px", img.width, img.height);
        }
        "magic" => {
            // magic <jeu> [signature dossier [nombre]] : compte les assets sans nom par signature (4
            // premiers octets) ; avec une signature, extrait jusqu'à 5 exemples (ou `nombre`) dans le dossier
            let dump = args.get(2).zip(args.get(3));
            // 5e argument : nombre maximal d'exemples extraits (5 par défaut)
            let max_dump: usize = args.get(4).and_then(|n| n.parse().ok()).unwrap_or(5);
            let mut dumped = 0;
            let mut by = std::collections::BTreeMap::<String, (usize, u64)>::new();
            for (h, e) in &gd.index.entries {
                if gd.index.names.contains_key(h) {
                    continue;
                }
                let Ok(raw) = gd.index.read_raw(e) else { continue };
                let data = if raw.starts_with(&[0xA1, 0xB2, 0xC3, 0xD4]) {
                    forgelight::pack::decode_asset(raw).unwrap_or_default()
                } else {
                    raw
                };
                let m: String = data.iter().take(4).map(|&b| if b.is_ascii_graphic() { b as char } else { '.' }).collect();
                if let Some((sig, dir)) = dump {
                    if &m == sig && dumped < max_dump {
                        std::fs::create_dir_all(dir)?;
                        std::fs::write(PathBuf::from(dir).join(format!("{h:016X}.bin")), &data)?;
                        dumped += 1;
                    }
                }
                let s = by.entry(m).or_default();
                s.0 += 1;
                s.1 += data.len() as u64;
            }
            let mut v: Vec<_> = by.into_iter().collect();
            v.sort_by_key(|(_, (n, _))| std::cmp::Reverse(*n));
            for (m, (n, sz)) in v.iter().take(40) {
                println!("{m:<6} {n:>7} assets  {:>8} Mo", sz / 1_000_000);
            }
        }
        "couleurs" => {
            // couleurs <jeu> <modele>... : canaux de la couleur des sommets (poids du balancement des
            // plantes), comparés à la hauteur et à la distance à l'axe vertical
            for name in &args[2..] {
                let m = gd.load_model(name)?;
                println!("== {name}");
                for (mi, mesh) in m.meshes.iter().enumerate() {
                    let n = mesh.vertices.len() as f32;
                    if n == 0.0 {
                        continue;
                    }
                    let ys: Vec<f32> = mesh.vertices.iter().map(|v| v.position[1]).collect();
                    let rs: Vec<f32> = mesh.vertices.iter().map(|v| v.position[0].hypot(v.position[2])).collect();
                    let corr = |a: &[f32], b: &[f32]| {
                        let (ma, mb) = (a.iter().sum::<f32>() / n, b.iter().sum::<f32>() / n);
                        let (mut c, mut va, mut vb) = (0.0, 0.0, 0.0);
                        for (x, y) in a.iter().zip(b) {
                            c += (x - ma) * (y - mb);
                            va += (x - ma) * (x - ma);
                            vb += (y - mb) * (y - mb);
                        }
                        c / (va * vb).sqrt().max(1e-9)
                    };
                    println!(
                        "  mesh {mi} {} ({} sommets) y {:.2}..{:.2} r ..{:.2}",
                        mesh.material_definition,
                        mesh.vertices.len(),
                        ys.iter().cloned().fold(f32::MAX, f32::min),
                        ys.iter().cloned().fold(f32::MIN, f32::max),
                        rs.iter().cloned().fold(0.0, f32::max)
                    );
                    for k in 0..4 {
                        let c: Vec<f32> = mesh.vertices.iter().map(|v| v.color[k]).collect();
                        let mut distinct: Vec<u32> = c.iter().map(|v| (v * 255.0).round() as u32).collect();
                        distinct.sort();
                        distinct.dedup();
                        println!(
                            "    octet {k} : {:.3}..{:.3} moy {:.3}, {} valeurs, corr hauteur {:+.2}, corr rayon {:+.2}  ex. {:?}",
                            c.iter().cloned().fold(f32::MAX, f32::min),
                            c.iter().cloned().fold(f32::MIN, f32::max),
                            c.iter().sum::<f32>() / n,
                            distinct.len(),
                            corr(&c, &ys),
                            corr(&c, &rs),
                            &distinct[..distinct.len().min(8)]
                        );
                    }
                }
            }
        }
        "audit" => {
            // audit <jeu> [Nom...] : ce que le décor d'une carte ne peut pas montrer
            let zones: Vec<String> = if args.len() > 2 {
                args[2..].to_vec()
            } else {
                ["Indar", "Amerish", "Esamir", "Hossin", "VR", "Tutorial", "quickload"].map(String::from).to_vec()
            };
            for z in &zones {
                audit_zone(&gd, z)?;
            }
        }
        c => bail!("commande inconnue : {c}\n{USAGE}"),
    }
    Ok(())
}

/// Noms des balises d'un document XML (lecture grossière : `<Nom` sans `/`, `?` ni `!`).
fn xml_tags(text: &str) -> Vec<String> {
    text.split('<')
        .skip(1)
        .filter_map(|s| {
            let n: String = s.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
            (!n.is_empty()).then_some(n)
        })
        .collect()
}

/// Audit d'une carte : acteurs, modèles et textures introuvables, balises `.adr` et effets que le
/// décor ne lit pas, matériaux par nombre d'instances, lumières écartées.
fn audit_zone(gd: &GameData, name: &str) -> Result<()> {
    use std::collections::{BTreeMap, BTreeSet};
    let file = format!("{name}.zone");
    let z = forgelight::zone::Zone::parse(&gd.read(&file)?)?;
    println!("\n===== {file} : {} acteurs, {} instances, {} lumières", z.objects.len(), z.instance_count(), z.lights.len());
    let mut problems: Vec<(usize, String, String)> = Vec::new();
    let mut tags: BTreeMap<String, (usize, usize, String)> = BTreeMap::new();
    let mut effects: BTreeMap<String, (usize, usize, String)> = BTreeMap::new();
    let mut materials: BTreeMap<String, (usize, usize, String)> = BTreeMap::new();
    let mut missing_tex: BTreeMap<String, (usize, String)> = BTreeMap::new();
    let mut warnings: BTreeMap<String, usize> = BTreeMap::new();
    for o in &z.objects {
        let n = o.instances.len();
        let text = match gd.read(&o.actor) {
            Ok(d) => String::from_utf8_lossy(&d).into_owned(),
            Err(e) => {
                problems.push((n, o.actor.clone(), format!("adr introuvable : {e:#}")));
                continue;
            }
        };
        let seen: BTreeSet<String> = xml_tags(&text).into_iter().collect();
        for t in seen {
            let s = tags.entry(t).or_insert((0, 0, o.actor.clone()));
            s.0 += 1;
            s.1 += n;
        }
        match gd.load_model(&o.actor) {
            Err(e) => problems.push((n, o.actor.clone(), format!("{e:#}"))),
            Ok(m) => {
                for e in &m.effects {
                    let s = effects.entry(e.clone()).or_insert((0, 0, o.actor.clone()));
                    s.0 += 1;
                    s.1 += n;
                }
                for w in &m.warnings {
                    *warnings.entry(w.clone()).or_default() += n;
                }
                if m.meshes.is_empty() {
                    problems.push((n, o.actor.clone(), "aucun maillage".into()));
                }
                let mut mats = BTreeSet::new();
                for mesh in &m.meshes {
                    mats.insert(mesh.material_definition.clone());
                    for t in mesh.render_textures() {
                        // (comme `GameData::read` : un chemin `..\GraphicsData\…` est cherché par son nom seul)
                        let base = t.rsplit(['\\', '/']).next().unwrap_or(t);
                        if !gd.index.contains(t) && !gd.index.contains(base) {
                            let s = missing_tex.entry(t.clone()).or_insert((0, o.actor.clone()));
                            s.0 += n;
                        }
                    }
                }
                for mat in mats {
                    let s = materials.entry(mat).or_insert((0, 0, o.actor.clone()));
                    s.0 += 1;
                    s.1 += n;
                }
            }
        }
    }
    problems.sort_by_key(|p| std::cmp::Reverse(p.0));
    println!("-- acteurs non affichables : {}", problems.len());
    for (n, a, e) in &problems {
        println!("{n:>6} × {a:<60} {e}");
    }
    println!("-- textures introuvables : {}", missing_tex.len());
    for (t, (n, a)) in &missing_tex {
        println!("{n:>6} × {t:<50} (ex. {a})");
    }
    println!("-- avertissements des modèles");
    for (w, n) in &warnings {
        println!("{n:>6} × {w}");
    }
    let by_inst = |m: &BTreeMap<String, (usize, usize, String)>| {
        let mut v: Vec<_> = m.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        v.sort_by_key(|(_, (_, n, _))| std::cmp::Reverse(*n));
        v
    };
    println!("-- balises .adr (acteurs, instances, exemple)");
    for (t, (a, n, ex)) in by_inst(&tags) {
        println!("{a:>6} {n:>7}  {t:<28} {ex}");
    }
    println!("-- effets des acteurs (acteurs, instances, exemple)");
    for (t, (a, n, ex)) in by_inst(&effects) {
        println!("{a:>6} {n:>7}  {t:<40} {ex}");
    }
    println!("-- matériaux (acteurs, instances, exemple)");
    for (t, (a, n, ex)) in by_inst(&materials) {
        println!("{a:>6} {n:>7}  {t:<40} {ex}");
    }
    let spots = z.lights.iter().filter(|l| l.spot).count();
    let no_range: Vec<_> = z.lights.iter().filter(|l| l.range <= 0.0).collect();
    let dark = z.lights.iter().filter(|l| l.color == [0, 0, 0] || l.inner_range <= 0.0).count();
    println!("-- lumières : {} ponctuelles, {spots} projecteurs, {} sans portée (écartées), {dark} noires ou d'intensité nulle", z.lights.len() - spots, no_range.len());
    let mut names: BTreeMap<String, usize> = BTreeMap::new();
    for l in &z.lights {
        let base: String = l.name.trim_end_matches(|c: char| c.is_ascii_digit() || c == '_').to_string();
        *names.entry(format!("{base} [{}]", l.color_name)).or_default() += 1;
    }
    let mut v: Vec<_> = names.into_iter().collect();
    v.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    for (k, n) in v.iter().take(25) {
        println!("{n:>6} × {k}");
    }
    for l in no_range.iter().take(10) {
        println!("  sans portée : {} [{}] intensité {} pos {:.0}", l.name, l.color_name, l.inner_range, l.position);
    }
    Ok(())
}

fn fbx_info(path: &str) -> Result<()> {
    let anim = forgelight::fbx::FbxAnimation::parse(&std::fs::read(path)?)?;
    println!(
        "Prise « {} » : {} nœuds, {} images à {} i/s ({:.2} s)",
        anim.take,
        anim.names.len(),
        anim.frames.len(),
        anim.fps,
        anim.duration()
    );
    for (i, n) in anim.names.iter().enumerate() {
        let mut depth = 0;
        let mut p = anim.parents[i];
        while let Some(q) = p {
            depth += 1;
            p = anim.parents[q];
        }
        let rest = anim.rest_world[i].w_axis;
        let mid = anim.frames.get(anim.frames.len() / 2).map(|f| f[i].w_axis).unwrap_or(rest);
        println!(
            "  {:indent$}{n:<w$} repos ({:7.2}, {:7.2}, {:7.2})  milieu ({:7.2}, {:7.2}, {:7.2})",
            "",
            rest.x,
            rest.y,
            rest.z,
            mid.x,
            mid.y,
            mid.z,
            indent = depth * 2,
            w = 34usize.saturating_sub(depth * 2)
        );
    }
    Ok(())
}
