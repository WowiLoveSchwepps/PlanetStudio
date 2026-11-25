//! Mode capture sans fenêtre : rend des modèles dans un PNG (tests, vignettes).
//!
//! planet-studio --capture <dossier_jeu> <sortie.png> <asset> [asset fusionné...] [--pose OS=rx,ry,rz ...] [--yaw degrés]
//!   [--zoom facteur] [--aim mètres] [--light x,y,z] [--no-relief] [--no-emissive]
//!   [--quality apercu|equilibre|final] [--msaa 1|2|4|8] [--fxaa] [--no-specular-aa]
//!   [--look neutre|cinema] [--exposure IL] [--tonemap aucun|doux|aces|agx] [--contrast f] [--saturation f]
//!   [--temperature -1..1] [--vignette 0..1] [--grain 0..1] [--lens-dirt 0..1] [--lens-curve 0..1] [--sharpen 0..1] [--dof auto|mètres]
//!   [--dof-blur %] [--texture-sharpness 0..1] [--no-bicubic]
//!   [--flora-quality basse|haute] [--water-quality basse|haute] [--terrain-quality basse|moyenne|haute|ultra]
//!   [--shadow-quality off|low|medium|high|ultra]
//!   [--brightness facteur] [--supersample 1..4] [--bloom force] [--no-bloom] [--ao force]
//!   [--ao-radius mètres] [--no-ao]
//!   [--no-shadows] [--ground-shadow 0..1]
//!   [--sun azimut,hauteur] [--sun-color r,v,b] [--sun-int facteur] [--sun-preset studio|midi|matin|coucher|nuit]
//!   [--sun-key IMAGE:préréglage|azimut,hauteur[,r,v,b[,intensité]] ...] (soleil animé, interpolation lissée)
//!   [--background r,v,b] [--amb-key IMAGE:intensité[,rotation[,r,v,b]] ...] (ambiance et fond animés)
//!   [--ambiance studio|indar|indar-savane|amerish|esamir|nexus|dome|exterieur] [--ambiance-rot degrés] [--ambiance-int facteur]
//!   [--camo aucun|defaut|TEXTURE] [--camo-scale répétitions] [--camo-int 0..1]
//!   [--decal aucune|defaut|TEXTURE] [--decal-opacity 0..1]
//!   [--decor ZONE:X,Z[,RAYON]] [--no-flora] [--no-terrain] [--decor-ground HAUTEUR] [--decor-shadow MÈTRES]
//!   [--no-sky] [--sky-hour HEURE] (ciel de la carte, choisi selon l'heure, 0 à 24 ; sans --sun*,
//!   le soleil et l'ambiance suivent aussi l'heure) [--no-fog] [--fog-density facteur]
//!   [--no-sun-rays] [--sun-rays facteur] (rayons du soleil : l'air éclairé, coupé par les ombres)
//!   [--particles auto|aucune|feuilles|sable|neige|papillons] [--particles-density facteur] [--particles-size facteur]
//!   [--wind facteur] [--wind-dir degrés] (particules d'ambiance ; « auto » suit le continent du décor)
//!   (décor d'une carte autour du point X,Z du jeu ; sans asset, seul le décor est rendu)
//!   [--eye x,y,z] [--target x,y,z] [--fov degrés] [--size LxH] (caméra et taille d'image imposées)
//!   [--anim CLIP|fichier.fbx] (clip du jeu, ou animation FBX Mixamo transférée sur le squelette)

use crate::camera::ViewParams;
use crate::camo::{Camo, TextureChoice};
use crate::decal::Decal;
use crate::anim::{AmbKey, AmbTrack, Interp, SunKey, SunTrack};
use crate::environment::{preset_by_name, preset_state, Ambiance, Lighting, SUN_PRESETS};
use crate::quality::{Preset, Quality, ShadowQuality};
use crate::render::{DrawItem, Lines, Renderer};
use crate::scene::{quat_from_euler_deg, Attachment, Scene};
use anyhow::{Context, Result};
use eframe::wgpu;
use forgelight::GameData;
use glam::Vec3;
use std::path::PathBuf;
use std::sync::Arc;

pub fn run(args: &[String]) -> Result<()> {
    let game_dir = args.first().context("dossier du jeu manquant")?;
    let out = args.get(1).context("fichier de sortie manquant")?;
    let mut assets = Vec::new();
    let mut poses = Vec::new();
    let mut yaw = 20.0f32;
    // couches d'animation : (nom, additif, partie du corps)
    let mut layers: Vec<(String, bool, crate::clips::BodyPart)> = Vec::new();
    let mut part = crate::clips::BodyPart::All;
    let mut fade_in = 0i32;
    let mut dance: Option<f32> = None;
    // vidéo : (fichier, nombre d'images)
    let mut video: Option<(String, i32)> = None;
    let mut anim_time = 0.0f32;
    let mut bake_frame: Option<f32> = None;
    let mut loops = 1u32;
    // gros plan : rapprochement de la caméra et décalage vertical de la visée
    let mut zoom = 1.0f32;
    let mut aim = 0.0f32;
    // qualité « Équilibré » sans suréchantillonnage : le rendu de capture d'origine
    let mut quality = Quality { supersample: 1, ..Quality::default() };
    // post-traitement du projet (neutre par défaut)
    let mut fx = crate::post::PostFx::default();
    let mut light: Option<Vec3> = None;
    let mut lighting = Lighting::default();
    let mut sun = SunTrack::default();
    let mut amb = AmbTrack::default();
    let mut camo = Camo::default();
    let mut decal = Decal::default();
    let mut decor: Option<crate::decor::DecorSettings> = None;
    let mut no_flora = false;
    let mut no_terrain = false;
    let mut no_sky = false;
    let mut sky_hour = crate::sky::DEFAULT_HOUR;
    // le soleil suit l'heure du ciel : --sky-hour donné, sans réglage de soleil explicite
    let mut sky_sun = false;
    let mut sun_given = false;
    let mut no_fog = false;
    let mut fog_density = 1.0f32;
    let mut renderer_haze = 1.0f32;
    let mut renderer_glow = 1.0f32;
    let mut decor_lights = 1.0f32;
    let mut sun_rays = 1.0f32;
    let mut decor_ground: Option<f32> = None;
    let mut decor_shadow: Option<f32> = None;
    let mut eye_arg: Option<Vec3> = None;
    let mut target_arg: Option<Vec3> = None;
    let mut fov = 40.0f32;
    let mut size_arg: Option<(u32, u32)> = None;
    let mut particles = crate::particles::Particles::default();
    let mut it = args[2..].iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--decor" => decor = Some(crate::decor::parse_arg(it.next().context("--decor ZONE:X,Z[,RAYON]")?)?),
            "--no-flora" => no_flora = true,
            "--no-terrain" => no_terrain = true,
            "--no-sky" => no_sky = true,
            "--sky-hour" => {
                sky_hour = it.next().context("--sky-hour HEURE")?.parse()?;
                sky_sun = true;
            }
            "--no-fog" => no_fog = true,
            "--fog-density" => fog_density = it.next().context("--fog-density FACTEUR")?.parse()?,
            "--haze" => renderer_haze = it.next().context("--haze FACTEUR")?.parse()?,
            "--sun-glow" => renderer_glow = it.next().context("--sun-glow FACTEUR")?.parse()?,
            "--no-sun-rays" => sun_rays = 0.0,
            "--decor-lights" => decor_lights = it.next().context("--decor-lights FORCE")?.parse()?,
            "--sun-rays" => sun_rays = it.next().context("--sun-rays FACTEUR")?.parse()?,
            "--particles" => {
                let k = it.next().context("--particles auto|aucune|feuilles|sable|neige|papillons")?;
                particles.kind = crate::particles::Kind::from_key(k).with_context(|| format!("particules « {k} » inconnues"))?;
            }
            "--particles-density" => particles.density = it.next().context("--particles-density FACTEUR")?.parse()?,
            "--particles-size" => particles.size = it.next().context("--particles-size FACTEUR")?.parse()?,
            "--wind" => particles.wind = it.next().context("--wind FACTEUR")?.parse()?,
            "--wind-dir" => particles.wind_dir = it.next().context("--wind-dir DEGRÉS")?.parse()?,
            "--decor-ground" => decor_ground = Some(it.next().context("--decor-ground HAUTEUR")?.parse()?),
            "--decor-shadow" => decor_shadow = Some(it.next().context("--decor-shadow MÈTRES")?.parse()?),
            "--eye" => eye_arg = Some(Vec3::from_slice(&floats(it.next().context("--eye X,Y,Z")?, 3)?)),
            "--target" => target_arg = Some(Vec3::from_slice(&floats(it.next().context("--target X,Y,Z")?, 3)?)),
            "--fov" => fov = it.next().context("--fov DEGRÉS")?.parse()?,
            "--size" => {
                let s = it.next().context("--size LxH")?;
                let (w, h) = s.split_once('x').context("--size LxH")?;
                size_arg = Some((w.parse()?, h.parse()?));
            }
            "--pose" => poses.push(it.next().context("--pose OS=rx,ry,rz")?.clone()),
            "--yaw" => yaw = it.next().context("--yaw")?.parse()?,
            "--anim" => layers.push((it.next().context("--anim NOM")?.clone(), false, part)),
            "--add" => layers.push((it.next().context("--add NOM")?.clone(), true, part)),
            "--upper" => part = crate::clips::BodyPart::Upper,
            "--lower" => part = crate::clips::BodyPart::Lower,
            "--dance" => dance = Some(it.next().context("--dance SECONDES")?.parse()?),
            "--fade" => fade_in = it.next().context("--fade")?.parse()?,
            "--video" => {
                let file = it.next().context("--video FICHIER.mp4 IMAGES")?.clone();
                let n = it.next().context("--video FICHIER.mp4 IMAGES")?.parse()?;
                video = Some((file, n));
            }
            "--time" => anim_time = it.next().context("--time")?.parse()?,
            "--loops" => loops = it.next().context("--loops")?.parse()?,
            "--bake-frame" => bake_frame = Some(it.next().context("--bake-frame")?.parse()?),
            "--zoom" => zoom = it.next().context("--zoom FACTEUR")?.parse()?,
            "--aim" => aim = it.next().context("--aim MÈTRES")?.parse()?,
            "--no-relief" => quality.normal_maps = false,
            "--no-emissive" => quality.emissive = false,
            "--emissive" => quality.emissive_strength = it.next().context("--emissive INTENSITÉ")?.parse()?,
            "--quality" => {
                let k = it.next().context("--quality apercu|equilibre|final")?;
                quality = quality.with_preset(Preset::from_key(k).with_context(|| format!("qualité « {k} » inconnue"))?);
            }
            "--msaa" => quality.msaa = it.next().context("--msaa 1|2|4|8")?.parse()?,
            "--fxaa" => quality.fxaa = true,
            "--look" => {
                let v = it.next().context("--look neutre|cinema|jeu")?;
                fx = match v.as_str() {
                    "neutre" => crate::post::PostFx::default(),
                    "cinema" | "cinéma" => crate::post::PostFx::cinema(),
                    "jeu" | "live" => crate::post::PostFx::game(),
                    _ => anyhow::bail!("--look : neutre, cinema ou jeu"),
                };
            }
            "--exposure" => fx.exposure = it.next().context("--exposure IL")?.parse()?,
            "--tonemap" => {
                let v = it.next().context("--tonemap aucun|doux|aces|agx")?;
                fx.tonemap = crate::post::ToneMap::from_key(v).with_context(|| format!("rendu des tons « {v} » inconnu"))?;
            }
            "--contrast" => fx.contrast = it.next().context("--contrast FACTEUR")?.parse()?,
            "--saturation" => fx.saturation = it.next().context("--saturation FACTEUR")?.parse()?,
            "--temperature" => fx.temperature = it.next().context("--temperature -1..1")?.parse()?,
            "--tint" => fx.tint = it.next().context("--tint -1..1")?.parse()?,
            "--game-grading" => fx.game_grading = true,
            "--no-game-grading" => fx.game_grading = false,
            "--vignette" => fx.vignette = it.next().context("--vignette 0..1")?.parse()?,
            "--grain" => fx.grain = it.next().context("--grain 0..1")?.parse()?,
            "--lens-dirt" => fx.lens_dirt = it.next().context("--lens-dirt 0..1")?.parse()?,
            "--lens-curve" => fx.lens_curve = it.next().context("--lens-curve 0..1")?.parse()?,
            "--sharpen" => fx.sharpen = it.next().context("--sharpen 0..1")?.parse()?,
            "--dof" => {
                // distance de mise au point, ou « auto » (la cible de la caméra)
                let v = it.next().context("--dof auto|MÈTRES")?;
                fx.dof = true;
                fx.focus_auto = v == "auto";
                if !fx.focus_auto {
                    fx.focus = v.parse()?;
                }
            }
            "--dof-blur" => fx.blur = it.next().context("--dof-blur % de la hauteur")?.parse()?,
            "--texture-sharpness" => quality.texture_sharpness = it.next().context("--texture-sharpness 0..1")?.parse()?,
            "--no-bicubic" => quality.texture_hq = false,
            "--no-fxaa" => quality.fxaa = false,
            "--no-specular-aa" => quality.specular_aa = false,
            "--flora-quality" => quality.flora_hq = it.next().context("--flora-quality basse|haute")?.as_str() != "basse",
            "--water-quality" => quality.water_hq = it.next().context("--water-quality basse|haute")?.as_str() != "basse",
            "--terrain-quality" => {
                let v = it.next().context("--terrain-quality basse|moyenne|haute|ultra")?;
                quality.terrain = match v.as_str() {
                    "basse" => crate::quality::TerrainQuality::Low,
                    "moyenne" => crate::quality::TerrainQuality::Medium,
                    "haute" => crate::quality::TerrainQuality::High,
                    "ultra" => crate::quality::TerrainQuality::Ultra,
                    _ => anyhow::bail!("--terrain-quality : basse, moyenne, haute ou ultra"),
                };
            }
            "--supersample" => quality.supersample = it.next().context("--supersample 1..4")?.parse()?,
            "--brightness" => quality.brightness = it.next().context("--brightness FACTEUR")?.parse()?,
            "--bloom" => quality.bloom = it.next().context("--bloom FORCE")?.parse()?,
            "--ao" => quality.ao = it.next().context("--ao FORCE")?.parse()?,
            "--ao-radius" => quality.ao_radius = it.next().context("--ao-radius MÈTRES")?.parse()?,
            "--no-bloom" => quality.bloom = 0.0,
            "--no-ao" => quality.ao = 0.0,
            "--shadow-quality" => {
                let k = it.next().context("--shadow-quality off|low|medium|high|ultra")?.to_lowercase();
                quality.shadows = match k.as_str() {
                    "off" | "aucune" => ShadowQuality::Off,
                    "low" | "basse" => ShadowQuality::Low,
                    "medium" | "moyenne" => ShadowQuality::Medium,
                    "high" | "haute" => ShadowQuality::High,
                    "ultra" => ShadowQuality::Ultra,
                    _ => anyhow::bail!("qualité d'ombre « {k} » inconnue"),
                };
            }
            "--no-shadows" => lighting.shadows = false,
            "--actors-lit" => lighting.decor_shades_actors = false,
            "--ground-shadow" => lighting.ground_shadow = it.next().context("--ground-shadow 0..1")?.parse()?,
            "--ambiance" => {
                let k = it.next().context("--ambiance NOM")?;
                lighting.ambiance = Ambiance::from_key(k).with_context(|| {
                    let names: Vec<_> = Ambiance::ALL.iter().map(|a| a.key()).collect();
                    format!("ambiance « {k} » inconnue ({})", names.join(", "))
                })?;
            }
            "--camo" => camo.choice = Camo::choice_from_arg(it.next().context("--camo aucun|defaut|TEXTURE")?),
            "--camo-scale" => camo.scale = it.next().context("--camo-scale RÉPÉTITIONS")?.parse()?,
            "--decal" => decal.choice = Decal::choice_from_arg(it.next().context("--decal aucune|defaut|TEXTURE")?),
            "--decal-opacity" => decal.opacity = it.next().context("--decal-opacity 0..1")?.parse()?,
            "--camo-int" => camo.strength = it.next().context("--camo-int 0..1")?.parse()?,
            "--ambiance-rot" => lighting.rotation = it.next().context("--ambiance-rot DEGRÉS")?.parse()?,
            "--ambiance-int" => lighting.intensity = it.next().context("--ambiance-int FACTEUR")?.parse()?,
            "--light" => {
                let v = floats(it.next().context("--light X,Y,Z")?, 3)?;
                light = Some(Vec3::new(v[0], v[1], v[2]));
            }
            "--sun" => {
                sun_given = true;
                let v = floats(it.next().context("--sun AZIMUT,HAUTEUR")?, 2)?;
                (lighting.sun_azimuth, lighting.sun_elevation) = (v[0], v[1]);
            }
            "--sun-color" => {
                sun_given = true;
                let v = floats(it.next().context("--sun-color R,V,B")?, 3)?;
                lighting.sun_color = [v[0], v[1], v[2]];
            }
            "--sun-int" => {
                sun_given = true;
                lighting.sun_intensity = it.next().context("--sun-int FACTEUR")?.parse()?;
            }
            "--sun-preset" => {
                sun_given = true;
                let i = preset_index(it.next().context("--sun-preset NOM")?)?;
                lighting.apply_preset(i);
            }
            // IMAGE:PRÉRÉGLAGE ou IMAGE:AZIMUT,HAUTEUR[,R,V,B[,INTENSITÉ]] (le reste vient du soleil fixe)
            "--sun-key" => {
                sun_given = true;
                let arg = it.next().context("--sun-key IMAGE:PRÉRÉGLAGE|AZIMUT,HAUTEUR[,R,V,B[,INTENSITÉ]]")?;
                let (f, v) = arg.split_once(':').context("--sun-key attend IMAGE:…")?;
                let frame: i32 = f.trim().parse()?;
                let st = if v.chars().next().is_some_and(|c| c.is_alphabetic()) {
                    preset_state(preset_index(v)?).unwrap()
                } else {
                    let n: Vec<f32> = v.split(',').map(|x| x.trim().parse()).collect::<Result<_, _>>()?;
                    anyhow::ensure!(matches!(n.len(), 2 | 5 | 6), "--sun-key : 2, 5 ou 6 nombres attendus");
                    let mut st = lighting.sun_state();
                    (st.azimuth, st.elevation) = (n[0], n[1]);
                    if n.len() >= 5 {
                        st.color = [n[2], n[3], n[4]];
                    }
                    if n.len() == 6 {
                        st.intensity = n[5];
                    }
                    st
                };
                sun.set_key(SunKey::new(frame, st, Interp::Smooth));
            }
            "--background" => {
                let v = floats(it.next().context("--background R,V,B")?, 3)?;
                lighting.background = [v[0], v[1], v[2]];
            }
            // IMAGE:INTENSITÉ[,ROTATION[,R,V,B]] (le reste vient des réglages fixes)
            "--amb-key" => {
                let arg = it.next().context("--amb-key IMAGE:INTENSITÉ[,ROTATION[,R,V,B]]")?;
                let (f, v) = arg.split_once(':').context("--amb-key attend IMAGE:…")?;
                let n: Vec<f32> = v.split(',').map(|x| x.trim().parse()).collect::<Result<_, _>>()?;
                anyhow::ensure!(matches!(n.len(), 1 | 2 | 5), "--amb-key : 1, 2 ou 5 nombres attendus");
                let mut st = lighting.amb_state();
                st.intensity = n[0];
                if let Some(r) = n.get(1) {
                    st.rotation = *r;
                }
                if n.len() == 5 {
                    st.background = [n[2], n[3], n[4]];
                }
                amb.set_key(AmbKey::new(f.trim().parse()?, st, Interp::Smooth));
            }
            _ => assets.push(a.clone()),
        }
    }

    let mut gd = GameData::open(&PathBuf::from(game_dir))?;
    gd.load_namelist(&forgelight::default_namelist_path(), &Default::default())?;

    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .context("aucun GPU compatible")?;
    let desc = wgpu::DeviceDescriptor {
        required_features: adapter.features()
            & (wgpu::Features::TEXTURE_COMPRESSION_BC | wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES),
        ..Default::default()
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&desc, None))?;
    let mut renderer = Renderer::from_device(device, queue);
    renderer.detect_sample_counts(&adapter);

    lighting_for(&mut renderer, &lighting, &sun, &amb, light, bake_frame.unwrap_or(anim_time * 30.0));
    let env = crate::environment::build(&gd, lighting.ambiance)?;
    // AMBIANCE_PNG=fichier.png : enregistre le panorama d'ambiance construit (contrôle visuel)
    if let (Some(e), Some(p)) = (&env, std::env::var_os("AMBIANCE_PNG")) {
        crate::app::write_png(std::path::Path::new(&p), e.width, e.height, &e.mips[0])?;
    }
    renderer.set_environment(env.as_ref());
    quality.apply(&mut renderer, lighting.shadows);
    renderer.post_settings.fx = fx;
    let ss = quality.export_supersample();
    // mise au point : la cible de la caméra, ou la distance donnée
    let focus = |view: &ViewParams| if fx.focus_auto { (view.target - view.eye).length() } else { fx.focus };
    renderer.ground_shadow = lighting.ground_shadow;
    renderer.decor_shades_actors = lighting.decor_shades_actors;

    // particules : « auto » suit le continent du décor
    let kind = particles.kind.resolve(decor.as_ref().map(|d| d.zone.as_str()));
    crate::particles::prepare(&mut renderer, &gd, kind);
    renderer.set_particles(kind, &particles);
    if let Some(k) = kind {
        println!("particules : {}", k.key());
    }

    // flore du sol du décor, posée autour de la caméra juste avant chaque rendu
    let mut ground_flora: Option<crate::decor::GroundFlora> = None;
    let follow_flora = |flora: &mut Option<crate::decor::GroundFlora>, renderer: &mut Renderer, eye: Vec3| {
        if let Some(i) = flora.as_mut().and_then(|f| f.update(&gd, &[eye])) {
            renderer.set_flora(&i);
        }
    };
    if let Some(mut s) = decor.take() {
        s.flora = !no_flora;
        s.terrain = !no_terrain;
        s.sky = !no_sky;
        s.sky_hour = sky_hour;
        s.fog = !no_fog;
        s.fog_density = fog_density;
        s.sun_rays = sun_rays > 0.0;
        s.sun_rays_strength = sun_rays.max(0.0);
        s.lamps = decor_lights > 0.0;
        s.lamps_strength = decor_lights.max(0.0);
        s.ground = decor_ground;
        let t0 = std::time::Instant::now();
        let (p, t) = (Default::default(), Default::default());
        let mut data = crate::decor::load(&gd, &s, &Default::default(), &p, &t)?;
        println!(
            "décor {} autour de ({}, {}) : {} instances, {} modèles, {} textures, {} lampes, sol à {:.1} m ({:.1}s){}",
            s.zone,
            s.center[0],
            s.center[1],
            data.instances.len(),
            data.models.len(),
            data.textures.len(),
            data.lights.len(),
            data.settings.ground.unwrap_or(0.0),
            t0.elapsed().as_secs_f32(),
            if data.failed.is_empty() { String::new() } else { format!(", {} acteurs illisibles", data.failed.len()) }
        );
        crate::decor::upload(&mut renderer, &data);
        ground_flora = data.ground_flora.take();
        renderer.haze_scale = renderer_haze.max(0.0);
        renderer.sun_glow = renderer_glow.max(0.0);
        if sky_sun && !sun_given {
            if let Some(sky_sun) = data.sky.as_ref().and_then(|k| k.sun.as_ref()) {
                lighting.set_sun_state(sky_sun.state());
                lighting.intensity = sky_sun.ambient();
                println!("soleil du ciel à {sky_hour} h : {:?}", sky_sun.state());
                lighting_for(&mut renderer, &lighting, &sun, &amb, light, bake_frame.unwrap_or(anim_time * 30.0));
            }
        }
        if let Some(r) = decor_shadow {
            renderer.decor_shadow_range = r;
        }
        if assets.is_empty() {
            let eye = eye_arg.unwrap_or(Vec3::new(70.0, 35.0, 70.0));
            let target_pt = target_arg.unwrap_or(Vec3::new(0.0, 5.0, 0.0));
            let view = ViewParams { eye, target: target_pt, fov_deg: fov };
            let (w, h) = size_arg.unwrap_or((1280, 720));
            let t0 = std::time::Instant::now();
            renderer.post_settings.focus = focus(&view);
            follow_flora(&mut ground_flora, &mut renderer, eye);
            let rgba = renderer.render_image(view.view_proj(w as f32 / h as f32), eye, &[], &Lines::default(), w, h, ss);
            println!("rendu en {:.2}s", t0.elapsed().as_secs_f32());
            crate::app::write_png(std::path::Path::new(out), w, h, &rgba)?;
            println!("écrit {out}");
            return Ok(());
        }
    }

    let mut scene = Scene::default();
    let mut first = None;
    for spec in &assets {
        // `Modele.adr@OS` : l'acteur entier suit cet os du premier (arme dans la main d'un MAX...)
        let (asset, bone) = match spec.split_once('@') {
            Some((a, b)) => (a, Some(b.to_string())),
            None => (spec.as_str(), None),
        };
        let model = Arc::new(gd.load_model(asset)?);
        for w in &model.warnings {
            eprintln!("{asset} : {w}");
        }
        for t in model.meshes.iter().flat_map(|m| m.render_textures()) {
            if renderer.has_texture(t) {
                continue;
            }
            match gd.load_texture_gpu(t) {
                Ok(img) => renderer.upload_texture(t, &img),
                Err(e) => eprintln!("texture {t} : {e:#}"),
            }
        }
        renderer.upload_model(asset, &model);
        let id = scene.add(asset, model);
        match first {
            None => first = Some(id),
            Some(p) => {
                scene.actor_mut(id).unwrap().attachment = Some(match bone {
                    Some(bone) => Attachment::Bone { parent: p, bone },
                    None => Attachment::BoneMerge { parent: p },
                })
            }
        }
    }
    let root = first.context("aucun asset")?;
    apply_appearance(&gd, &mut renderer, &mut scene, &camo, &decal)?;
    scene.evaluate(0.0);
    if let Some(secs) = dance {
        let a = scene.actor_mut(root).unwrap();
        let last = crate::dance::bake_horse_dance(a, 0, 30.0, 132.0, secs).context("squelette non humain")?;
        println!("danse écrite jusqu'à l'image {last}");
        scene.evaluate(bake_frame.unwrap_or(0.0));
    }
    if !layers.is_empty() {
        let is_fbx = |n: &str| n.to_ascii_lowercase().ends_with(".fbx");
        let net = match layers.iter().all(|(n, _, _)| is_fbx(n)) {
            true => None,
            false => Some(gd.load_network("ThirdPerson.mrn")?),
        };
        let mut last = 0;
        for (li, (name, additive, part)) in layers.iter().enumerate() {
            let clip = if is_fbx(name) {
                // animation FBX (Mixamo) transférée sur le squelette du personnage
                let anim = forgelight::fbx::FbxAnimation::parse(&std::fs::read(name).with_context(|| format!("lecture de {name}"))?)?;
                let sk = &scene.actor(root).unwrap().model.skeleton;
                crate::retarget::retarget(&anim, sk, name)?
            } else {
                let net = net.as_ref().unwrap();
                let info = net
                    .main_clips()
                    .into_iter()
                    .find(|c| c.name.eq_ignore_ascii_case(name))
                    .or_else(|| net.main_clips().into_iter().find(|c| c.name.to_ascii_lowercase().contains(&name.to_ascii_lowercase())))
                    .cloned()
                    .with_context(|| format!("clip introuvable : {name}"))?;
                crate::clips::DecodedClip::decode(net, &info)?
            };
            println!("couche {li} : {} ({:.2}s, {} images){}", clip.name, clip.duration(), clip.frames.len(), if *additive { " additive" } else { "" });
            let a = scene.actor_mut(root).unwrap();
            let mut root_tr = (li == 0 && !*additive).then_some(a.root_pose);
            let n = if li == 0 { loops } else { 1 };
            let mut end = 0;
            for l in 0..n {
                let opts = crate::clips::BakeOptions {
                    fps: 30.0,
                    speed: 1.0,
                    root_start: root_tr,
                    blend: crate::clips::BlendOptions {
                        mode: if *additive { crate::clips::BlendMode::Additive } else { crate::clips::BlendMode::Replace },
                        weight: 1.0,
                        mask: part.mask(&a.model.skeleton),
                    },
                    fade_in: if l == 0 && li > 0 { fade_in } else { 0 },
                    fade_out: 0,
                };
                let (e, r) = clip.bake(a, end, &opts);
                end = e;
                root_tr = r;
            }
            last = last.max(end);
        }
        println!("clés écrites jusqu'à l'image {last}");
        for probe in [0.0, 10.0, 20.0, 40.0, last as f32] {
            scene.evaluate(probe);
            let a = scene.actor(root).unwrap();
            let (_, r, t) = a.root_world.to_scale_rotation_translation();
            let (yaw, _, _) = r.to_euler(glam::EulerRot::YXZ);
            println!("  image {probe:>5} : racine ({:.2}, {:.2}, {:.2}) lacet {:.1}°", t.x, t.y, t.z, yaw.to_degrees());
        }
        scene.evaluate(bake_frame.unwrap_or(anim_time * 30.0));
    }
    for p in &poses {
        let (bone, v) = p.split_once('=').context("--pose OS=rx,ry,rz")?;
        let e: Vec<f32> = v.split(',').map(|s| s.parse().unwrap_or(0.0)).collect();
        let a = scene.actor_mut(root).unwrap();
        let b = a.bone_index(bone).with_context(|| format!("os inconnu {bone}"))?;
        let mut t = a.pose[b];
        let q = quat_from_euler_deg(Vec3::new(e[0], e.get(1).copied().unwrap_or(0.0), e.get(2).copied().unwrap_or(0.0)));
        t.rotation = (t.rotation * q).normalize();
        a.key_bone(b, 0, t, Default::default());
    }
    if layers.is_empty() && dance.is_none() {
        scene.evaluate(0.0);
    }

    if let Some((file, n)) = &video {
        // caméra qui suit le personnage, 720p
        let (w, h) = (1280u32, 720u32);
        use crate::video::{Codec, FrameRate, RateMode, Speed, VideoParams};
        let params = VideoParams {
            codec: Codec::H264,
            width: w,
            height: h,
            rate: FrameRate::new(30, 1),
            mode: RateMode::Crf,
            target_bps: 8_000_000,
            max_bps: 12_000_000,
            crf: 20,
            speed: Speed::Medium,
        };
        let mut enc = crate::export::Exporter::start(crate::export::Job::Video { path: file.into(), params })?;
        let t0 = std::time::Instant::now();
        for f in 0..=*n {
            scene.evaluate(f as f32);
            lighting_for(&mut renderer, &lighting, &sun, &amb, light, f as f32);
            renderer.wakes = crate::wake::build(&scene, f as f32, 30.0, &renderer.water_planes());
            let (lo, hi) = scene.actor(root).unwrap().bounds_world();
            let center = (lo + hi) * 0.5;
            let y = yaw.to_radians();
            let eye = center + Vec3::new(y.sin(), 0.25, y.cos()) * 3.5;
            let view = ViewParams { eye, target: center, fov_deg: 40.0 };
            let items: Vec<DrawItem> = scene
                .drawn_actors()
                .map(|a| DrawItem { id: a.id, asset: a.asset.clone(), skin: a.skin_matrices(), highlight: false, camo: a.camo.clone(), decal: a.decal.clone() })
                .collect();
            let mut lines = Lines::default();
            for i in -40..=40 {
                let g = i as f32 * 0.5;
                lines.line(Vec3::new(g, 0.0, -20.0), Vec3::new(g, 0.0, 20.0), [0.3, 0.3, 0.33, 0.6], false);
                lines.line(Vec3::new(-20.0, 0.0, g), Vec3::new(20.0, 0.0, g), [0.3, 0.3, 0.33, 0.6], false);
            }
            renderer.post_settings.focus = focus(&view);
            follow_flora(&mut ground_flora, &mut renderer, eye);
            let img = renderer.render_image(view.view_proj(w as f32 / h as f32), eye, &items, &lines, w, h, ss);
            enc.push_blocking(img)?;
        }
        enc.close_input();
        let path = enc.finish()?;
        println!("vidéo {} : {} images en {:.1}s", path.display(), n + 1, t0.elapsed().as_secs_f32());
        return Ok(());
    }
    // sillage dans l'eau du décor, à l'image rendue
    renderer.wakes = crate::wake::build(&scene, bake_frame.unwrap_or(anim_time * 30.0), 30.0, &renderer.water_planes());
    let (lo, hi) = scene.actor(root).unwrap().bounds_world();
    let center = (lo + hi) * 0.5 + Vec3::Y * aim;
    let size = (hi - lo).length().max(0.5);
    let y = yaw.to_radians();
    let eye = eye_arg.unwrap_or(center + Vec3::new(y.sin(), 0.15, y.cos()) * size * 1.3 / zoom.max(0.1));
    let view = ViewParams { eye, target: target_arg.unwrap_or(center), fov_deg: fov };
    let (w, h) = size_arg.unwrap_or((900, 1000));
    let items: Vec<DrawItem> = scene
        .drawn_actors()
        .map(|a| DrawItem { id: a.id, asset: a.asset.clone(), skin: a.skin_matrices(), highlight: false, camo: a.camo.clone(), decal: a.decal.clone() })
        .collect();
    let mut lines = Lines::default();
    for i in -6..=6 {
        let f = i as f32 * 0.5;
        lines.line(Vec3::new(f, 0.0, -3.0), Vec3::new(f, 0.0, 3.0), [0.3, 0.3, 0.33, 0.6], false);
        lines.line(Vec3::new(-3.0, 0.0, f), Vec3::new(3.0, 0.0, f), [0.3, 0.3, 0.33, 0.6], false);
    }
    renderer.post_settings.focus = focus(&view);
    follow_flora(&mut ground_flora, &mut renderer, eye);
    let t0 = std::time::Instant::now();
    let rgba = renderer.render_image(view.view_proj(w as f32 / h as f32), eye, &items, &lines, w, h, ss);
    println!("rendu {w}×{h} (suréchantillonné {ss}×) en {:.2}s", t0.elapsed().as_secs_f32());
    crate::app::write_png(std::path::Path::new(out), w, h, &rgba)?;
    println!("écrit {out}");
    Ok(())
}

/// Préréglage de soleil par son nom (ou son début).
fn preset_index(name: &str) -> Result<usize> {
    preset_by_name(name.trim()).with_context(|| {
        let names: Vec<_> = SUN_PRESETS.iter().map(|p| p.0).collect();
        format!("préréglage de soleil « {name} » inconnu ({})", names.join(", "))
    })
}

/// Soleil et ambiance d'une image : clés s'il y en a, sinon réglages fixes.
fn lighting_for(renderer: &mut Renderer, lighting: &Lighting, sun: &SunTrack, amb: &AmbTrack, light: Option<Vec3>, frame: f32) {
    let l = lighting.with_sun(sun.sample(frame).unwrap_or_else(|| lighting.sun_state()));
    // --light (direction brute) l'emporte sur l'azimut et la hauteur du soleil
    renderer.light_dir = light.unwrap_or_else(|| l.light_dir());
    renderer.sun_color = l.sun_radiance();
    let a = amb.sample(frame).unwrap_or_else(|| lighting.amb_state());
    renderer.env_rotation_deg = a.rotation;
    renderer.env_intensity = a.intensity;
    renderer.clear_color = a.background.map(|c| c as f64);
    // particules d'ambiance : temps de la timeline à 30 images/s
    renderer.particle_time = frame / 30.0;
}

/// « 1,2,3 » → n nombres
fn floats(s: &str, n: usize) -> Result<Vec<f32>> {
    let v: Vec<f32> = s.split(',').map(|x| x.trim().parse()).collect::<Result<_, _>>()?;
    anyhow::ensure!(v.len() == n, "{n} nombres séparés par des virgules attendus : « {s} »");
    Ok(v)
}

/// Donne le même camouflage et la même décalcomanie à tous les acteurs, et charge les textures
/// choisies si besoin.
fn apply_appearance(gd: &GameData, renderer: &mut Renderer, scene: &mut Scene, camo: &Camo, decal: &Decal) -> Result<()> {
    for choice in [&camo.choice, &decal.choice] {
        if let TextureChoice::Texture(t) = choice {
            if !renderer.has_texture(t) {
                renderer.upload_texture(t, &gd.load_texture_gpu(t).with_context(|| format!("texture {t}"))?);
            }
        }
    }
    for a in &mut scene.actors {
        a.camo = camo.clone();
        a.decal = decal.clone();
    }
    Ok(())
}
