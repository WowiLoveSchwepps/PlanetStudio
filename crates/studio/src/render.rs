//! Rendu 3D (wgpu) : maillages skinnés texturés + lignes (grille, squelette, gizmos).
//! Le rendu se fait dans une texture hors écran affichée par egui, ce qui permet
//! aussi l'export d'images.

use eframe::egui_wgpu;
use eframe::wgpu;
use eframe::wgpu::util::DeviceExt;
use crate::camo::Camo;
use crate::decal::Decal;
use crate::post::{Post, PostSettings, PostTargets, HDR_FORMAT};
use forgelight::{dds, slot};
use glam::{Mat4, Vec3};
use std::collections::HashMap;

pub const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// format des cartes de données (relief, spéculaire) : valeurs brutes, sans conversion sRGB
const DATA_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// anticrénelage par défaut (multiéchantillonnage)
const DEFAULT_SAMPLES: u32 = 4;

const SHADER: &str = r#"
struct Globals {
    view_proj: mat4x4<f32>,
    light_dir: vec4<f32>,
    cam_pos: vec4<f32>,
    // x : relief activé, y : émissif activé, z : intensité de l'éclairage studio
    flags: vec4<f32>,
    // ambiance : x active, y rotation (radians), z dernier mip utile, w intensité
    env: vec4<f32>,
    // éclairement diffus de l'ambiance (harmoniques sphériques, divisé par π)
    sh: array<vec4<f32>, 9>,
    // ombres du décor : projection du soleil (orthographique), couche 1 de la carte d'ombre
    light_vp: mat4x4<f32>,
    // x actives, y taille d'un texel en UV, z opacité de l'ombre des personnages au sol,
    // w décalage le long de la normale (m)
    shadow: vec4<f32>,
    // sol qui reçoit l'ombre : centre x, centre z, demi-côté, hauteur
    ground: vec4<f32>,
    // soleil : couleur × intensité (linéaire)
    sun: vec4<f32>,
    // qualité : x rayon du filtrage des ombres (texels), y luminosité, z biais de mip des
    // textures, w filtrage bicubique des gros plans
    quality: vec4<f32>,
    // brouillard : x actif, y densité (par m), z décroissance avec l'altitude (par m), w plancher (y du studio)
    fog: vec4<f32>,
    // x distance où il commence, y et z distances où le décor se fond dans le ciel ; w : le dôme
    // près de l'horizon, relativement au zénith (nuit d'Amerish ; 0 ou 1 : tel quel)
    fog2: vec4<f32>,
    // couleur propre du brouillard (rgb) et sa part face à la couleur de l'horizon (a)
    fog_color: vec4<f32>,
    // ombres des personnages : projection du soleil cadrée sur eux seuls, couche 0
    char_vp: mat4x4<f32>,
    // x actives, w décalage le long de la normale (m)
    char_shadow: vec4<f32>,
    // qualité (suite) : x anticrénelage des reflets, y distance du détail du terrain (m),
    // z eau (0 simple, 1 vagues animées, 2 vagues + reflet du ciel de la carte), w temps (s)
    quality2: vec4<f32>,
    // rayons du soleil : x densité de l'air (par m, 0 = coupés), y portée (m), z anisotropie de
    // la diffusion, w nombre de pas
    vol: vec4<f32>,
    // voile de l'air (`<Haze>` du jeu) : x force (0 = aucun ; part voilée au loin et dans le
    // ciel : 1 − exp(−force)), y distance caractéristique (m) ; z force du halo du soleil ;
    // w luminosité du dôme (`EvComp` du jeu à l'heure du ciel, 0 = sans ciel de carte)
    haze: vec4<f32>,
    // sillages des personnages dans l'eau (voir `wake.rs`) : x nombre de personnages
    wake_n: vec4<f32>,
    // par personnage : centre (x, z), rayon d'effet (m), hauteur de l'eau
    wake_head: array<vec4<f32>, 6>,
    // par personnage : premier point, points de la traînée, ronds, vitesse actuelle (m/s)
    wake_range: array<vec4<f32>, 6>,
    // traînée : x, z, âge (s), présence dans l'eau ; ronds : x, z, âge (s), force
    wake_pt: array<vec4<f32>, 256>,
    // lumières du décor (lampes, projecteurs de la carte) : x nombre
    light_n: vec4<f32>,
    // par lumière, 3 vec4 : position, portée (m) ; couleur × force (linéaire), cosinus du demi-angle
    // intérieur (−2 : ponctuelle) ; direction du faisceau, cosinus du demi-angle extérieur
    lights: array<vec4<f32>, 384>,
};
@group(0) @binding(0) var<uniform> g: Globals;
@group(0) @binding(1) var t_env: texture_2d<f32>;
@group(0) @binding(2) var s_env: sampler;
// couche 0 : personnages seuls (cadrage serré) ; couche 1 : décor (zone large)
@group(0) @binding(3) var t_shadow: texture_depth_2d_array;
@group(0) @binding(4) var s_shadow: sampler_comparison;
// dôme du ciel de la carte (même repère que le panorama d'ambiance)
@group(0) @binding(5) var t_sky: texture_2d<f32>;

fn sky_color(d: vec3<f32>) -> vec3<f32> {
    let uv = vec2<f32>(atan2(d.x, d.z) / 6.2831853 + 0.5, acos(clamp(d.y, -1.0, 1.0)) / 3.1415927);
    return textureSampleLevel(t_sky, s_env, uv, 0.0).rgb * g.quality.y * dome_exposure() * horizon_lift(d.y);
}

// lampes et projecteurs du décor (lumières placées de la carte) : diffus et reflet, atténués
// jusqu'au bout de leur portée, dans le cône des projecteurs. Pas d'ombre : une lampe éclaire
// aussi à travers un mur proche, mais leurs portées sont courtes (10 m en médiane).
fn decor_lamps(p: vec3<f32>, n: vec3<f32>, v: vec3<f32>, kd: vec3<f32>, f0: vec3<f32>, shin: f32, occ: f32) -> vec3<f32> {
    var acc = vec3<f32>(0.0);
    let cnt = u32(g.light_n.x);
    for (var k = 0u; k < cnt; k = k + 1u) {
        let a = g.lights[k * 3u];
        let d = a.xyz - p;
        let dist = length(d);
        if (dist >= a.w) { continue; }
        let b = g.lights[k * 3u + 1u];
        let s = g.lights[k * 3u + 2u];
        let l = d / max(dist, 1e-3);
        var att = 1.0 - dist / a.w;
        att = att * att;
        if (b.w > -1.5) {
            att = att * smoothstep(s.w, b.w, dot(-l, s.xyz));
        }
        let ndl = max(dot(n, l), 0.0);
        if (att * ndl <= 0.0) { continue; }
        let h = normalize(l + v);
        let fres = f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - max(dot(h, v), 0.0), 5.0);
        let spec = fres * ((shin + 2.0) / 8.0) * pow(max(dot(n, h), 0.0), shin) * occ;
        acc = acc + b.rgb * att * ndl * (kd + spec);
    }
    return acc;
}

// la nuit sur Amerish, le bas du ciel reste plus clair que le zénith (lueur sarcelle au-dessus
// des crêtes) : facteur `fog2.w` à l'horizon, 1 à partir d'environ 37° de hauteur
fn horizon_lift(y: f32) -> f32 {
    let l = select(1.0, g.fog2.w, g.fog2.w > 0.0);
    return mix(l, 1.0, smoothstep(0.0, 0.6, max(y, 0.0)));
}

// luminosité du dôme à l'heure du ciel (`EvComp` du jeu) ; 1 sans ciel de carte
fn dome_exposure() -> f32 {
    return select(1.0, g.haze.w, g.haze.w > 0.0);
}

// Brouillard en hauteur : densité constante sous le plancher, décroissance exponentielle
// au-dessus, intégrée le long du rayon. Au loin, le décor se fond dans le ciel, là où le
// terrain s'arrête.
fn apply_fog(c: vec3<f32>, wpos: vec3<f32>) -> vec3<f32> {
    if (g.fog.x < 0.5) { return c; }
    let ray = wpos - g.cam_pos.xyz;
    let dist = length(ray);
    let dir = ray / max(dist, 1e-4);
    let h0 = max(g.cam_pos.y - g.fog.w, 0.0);
    let h1 = max(wpos.y - g.fog.w, 0.0);
    let k = g.fog.z * (h1 - h0);
    var avg = exp(-g.fog.z * h0);
    if (abs(k) > 1e-3) { avg = (exp(-g.fog.z * h0) - exp(-g.fog.z * h1)) / k; }
    // début (`Range Near`) adouci : au lieu de rien jusqu'à cette distance puis tout d'un coup
    // (un bâtiment à 550 m net devant une montagne voilée à 650 m), le brouillard monte en
    // douceur dès la caméra (quadratique, presque nul près des personnages) et rejoint
    // `distance − début` à deux fois le début, puis le suit
    let s = g.fog2.x;
    var d_fog = max(dist - s, 0.0);
    if (dist < 2.0 * s) { d_fog = dist * dist / (4.0 * s); }
    let tau = g.fog.y * avg * d_fog;
    // teinte : l'horizon dans la direction du regard, mêlé à la couleur du jeu ramenée à la
    // luminosité de cet horizon (sombre la nuit, claire le jour)
    let flat_dir = vec3<f32>(dir.x, 0.0, dir.z) + vec3<f32>(1e-5, 0.0, 0.0);
    let hz = sky_color(normalize(flat_dir));
    let own = g.fog_color.rgb * (luma(hz) / max(luma(g.fog_color.rgb), 1e-4));
    let fc = mix(hz, own, g.fog_color.a);
    let f_fog = 1.0 - exp(-tau);
    var o = mix(c, fc, f_fog);
    // voile de l'air : vers l'horizon pâle du dôme, de plus en plus fort avec la distance jusqu'à
    // sa force, celle du ciel, si bien que le décor rejoint le ciel sans saut là où il s'y fond
    let f_haze = 1.0 - exp(-g.haze.x * (1.0 - exp(-dist / max(g.haze.y, 1.0))));
    o = mix(o, hz, f_haze);
    // l'air voilé face au soleil prend son halo, comme le ciel (contre-jour délavé)
    let glow = sun_glow(dir);
    o = o + glow * (1.0 - (1.0 - f_fog) * (1.0 - f_haze));
    let sky = mix(sky_color(dir), hz, 1.0 - exp(-g.haze.x)) + glow;
    return mix(o, sky, smoothstep(g.fog2.y, g.fog2.z, dist));
}

// rayons du soleil (« Fog Shadows » du jeu) : lumière du soleil diffusée par l'air le long du
// rayon caméra → surface, là seulement où le soleil arrive. Les points hors de la carte d'ombre
// du décor ne comptent pas (sinon tout le lointain s'éclairerait d'un voile uniforme).
fn sun_shafts(c: vec3<f32>, wpos: vec3<f32>, frag: vec2<f32>) -> vec3<f32> {
    if (g.vol.x <= 0.0 || g.shadow.x < 0.5) { return c; }
    let ray = wpos - g.cam_pos.xyz;
    let dist = length(ray);
    let dir = ray / max(dist, 1e-4);
    let len = min(dist, g.vol.y);
    let n = i32(g.vol.w);
    let dt = len / f32(n);
    // départ décalé d'un pixel à l'autre (bruit de gradient entrelacé) : pas de bandes
    let jit = fract(52.9829189 * fract(dot(frag, vec2<f32>(0.06711056, 0.00583715))));
    var lit = 0.0;
    for (var k = 0; k < n; k++) {
        let t = (f32(k) + jit) * dt;
        let p = g.cam_pos.xyz + dir * t;
        var v = shadow_point(g.light_vp, 1, p, true);
        if (g.char_shadow.x > 0.5) { v = v * shadow_point(g.char_vp, 0, p, false); }
        lit += v * exp(-g.vol.x * t);
    }
    // diffusion de Henyey-Greenstein, ramenée à 1 en moyenne : forte face au soleil
    let cs = dot(dir, normalize(-g.light_dir.xyz));
    let a = g.vol.z;
    let phase = (1.0 - a * a) / pow(max(1.0 + a * a - 2.0 * a * cs, 1e-4), 1.5);
    let scat = g.sun.rgb * g.quality.y * (g.vol.x * lit * dt * phase);
    return c * exp(-g.vol.x * len) + scat;
}

// Halo du soleil : la lumière diffusée vers l'avant par l'air (poussière, aérosols) éclaire le
// ciel autour du disque, en fondu : un cœur serré (≈ 2°, que le bloom élargit), une auréole
// (≈ 6°) et un halo large (≈ 25°). Teinte et force du soleil (orangé au coucher, faible et
// bleuté autour de la lune). `g.haze.z` : force (0 = aucun halo).
fn sun_glow(dir: vec3<f32>) -> vec3<f32> {
    if (g.haze.z <= 0.0) { return vec3<f32>(0.0); }
    let x = max(dot(dir, normalize(-g.light_dir.xyz)), 0.0);
    let k = 0.5 * pow(x, 3000.0) + 0.12 * pow(x, 300.0) + 0.1 * pow(x, 16.0);
    return g.sun.rgb * g.quality.y * (g.haze.z * k);
}

// soleil en un point de l'air (une seule lecture). `masked` : hors de la carte, 0 (le point ne
// compte pas) ; sinon 1 (rien n'y fait d'ombre)
fn shadow_point(vp: mat4x4<f32>, layer: i32, p: vec3<f32>, masked: bool) -> f32 {
    let lp = vp * vec4<f32>(p, 1.0);
    let uv = vec2<f32>(lp.x * 0.5 + 0.5, 0.5 - lp.y * 0.5);
    let edge = min(min(uv.x, 1.0 - uv.x), min(uv.y, 1.0 - uv.y));
    if (edge <= 0.0 || lp.z >= 1.0) { return select(1.0, 0.0, masked); }
    let v = textureSampleCompareLevel(t_shadow, s_shadow, uv, layer, lp.z);
    if (!masked) { return v; }
    return v * smoothstep(0.0, 0.08, edge);
}

// 1 = éclairé par le soleil, 0 = dans l'ombre ; filtrage 3×3 pour adoucir les bords
fn shadow_layer(vp: mat4x4<f32>, layer: i32, wpos: vec3<f32>, fade: bool) -> f32 {
    let lp = vp * vec4<f32>(wpos, 1.0);
    let uv = vec2<f32>(lp.x * 0.5 + 0.5, 0.5 - lp.y * 0.5);
    if (uv.x <= 0.0 || uv.x >= 1.0 || uv.y <= 0.0 || uv.y >= 1.0 || lp.z >= 1.0) { return 1.0; }
    let r = i32(g.quality.x);
    var s = 0.0;
    for (var y = -r; y <= r; y++) {
        for (var x = -r; x <= r; x++) {
            s += textureSampleCompareLevel(t_shadow, s_shadow, uv + vec2<f32>(f32(x), f32(y)) * g.shadow.y, layer, lp.z);
        }
    }
    let side = f32(2 * r + 1);
    if (!fade) { return s / (side * side); }
    // fondu sur les bords de la carte d'ombre : pas de limite nette entre ombre et lumière
    let edge = min(min(uv.x, 1.0 - uv.x), min(uv.y, 1.0 - uv.y));
    return mix(1.0, s / (side * side), smoothstep(0.0, 0.08, edge));
}

// ombre des personnages (la carte les contient tous : hors de la carte, rien ne fait d'ombre)
fn char_shadow_at(wpos: vec3<f32>, n: vec3<f32>) -> f32 {
    if (g.char_shadow.x < 0.5) { return 1.0; }
    return shadow_layer(g.char_vp, 0, wpos + n * g.char_shadow.w, false);
}

// ombre du décor (zone limitée autour des personnages, fondue sur ses bords)
fn decor_shadow_at(wpos: vec3<f32>, n: vec3<f32>) -> f32 {
    if (g.shadow.x < 0.5) { return 1.0; }
    return shadow_layer(g.light_vp, 1, wpos + n * g.shadow.w, true);
}

// même base que environment::sh_basis
fn env_irradiance(d: vec3<f32>) -> vec3<f32> {
    let x = d.x; let y = d.y; let z = d.z;
    var e = g.sh[0].xyz * 0.282095;
    e += g.sh[1].xyz * (0.488603 * y);
    e += g.sh[2].xyz * (0.488603 * z);
    e += g.sh[3].xyz * (0.488603 * x);
    e += g.sh[4].xyz * (1.092548 * x * y);
    e += g.sh[5].xyz * (1.092548 * y * z);
    e += g.sh[6].xyz * (0.315392 * (3.0 * z * z - 1.0));
    e += g.sh[7].xyz * (1.092548 * x * z);
    e += g.sh[8].xyz * (0.546274 * (x * x - y * y));
    return max(e, vec3<f32>(0.0));
}

// rotation du panorama autour de la verticale
fn env_dir(d: vec3<f32>) -> vec3<f32> {
    let c = cos(g.env.y);
    let s = sin(g.env.y);
    return vec3<f32>(c * d.x - s * d.z, d.y, s * d.x + c * d.z);
}

// panorama équirectangulaire : u = 0,5 vers +Z, v = 0 au zénith
fn env_radiance(d: vec3<f32>, lod: f32) -> vec3<f32> {
    let u = atan2(d.x, d.z) / 6.2831853 + 0.5;
    let v = acos(clamp(d.y, -1.0, 1.0)) / 3.1415927;
    return textureSampleLevel(t_env, s_env, vec2<f32>(u, v), lod).rgb;
}

// part spéculaire intégrée sur l'environnement (approximation de Karis)
fn env_brdf(f0: vec3<f32>, rough: f32, ndv: f32) -> vec3<f32> {
    let r = rough * vec4<f32>(-1.0, -0.0275, -0.572, 0.022) + vec4<f32>(1.0, 0.0425, 1.04, -0.04);
    let a004 = min(r.x * r.x, exp2(-9.28 * ndv)) * r.x + r.y;
    let ab = vec2<f32>(-1.04, 1.04) * a004 + r.zw;
    return f0 * ab.x + ab.y;
}

struct ActorU {
    tint: vec4<f32>,
    flags: vec4<f32>,
    // camouflage : x intensité (0 = aucun), y répétitions sur le dépliage UV1
    camo: vec4<f32>,
    // décalcomanie : x opacité (0 = aucune)
    decal: vec4<f32>,
};
@group(1) @binding(0) var<storage, read> bones: array<mat4x4<f32>>;
@group(1) @binding(1) var<uniform> actor: ActorU;

// matériau : couleur (sRGB), relief _N (linéaire, DXT5nm), _S (linéaire), motif de camouflage (sRGB)
@group(2) @binding(0) var t_diffuse: texture_2d<f32>;
@group(2) @binding(1) var t_normal: texture_2d<f32>;
@group(2) @binding(2) var t_spec: texture_2d<f32>;
@group(2) @binding(3) var s_tex: sampler;
@group(2) @binding(4) var t_camo: texture_2d<f32>;
struct MaterialU {
    // x : 1 si ce matériau reçoit un camouflage, y : 1 s'il reçoit une décalcomanie,
    // z : 1 s'il est découpé par l'alpha (feuillages), w : surface particulière (1 = terrain,
    // relief en repère du monde ; 2 = eau)
    flags: vec4<f32>,
    // terrain : côté d'une répétition du détail du sol (x) et de la roche (y), en m ; z et w :
    // 1 si ce détail est présent
    detail: vec4<f32>,
    // x : 1 si le détail répété du jeu est présent (choix `_DS` + cubemap), y : 1 s'il a une
    // couleur superposée, z : terrain, côtés où le détail s'estompe, w : 1 si `t_overlay` est
    // le masque de teinte de la flore du sol
    extra: vec4<f32>,
    // x : répétitions des motifs du détail sur UV1 (paramètre du matériau du jeu) ; y : 1 si le
    // terrain a sa carte de mélange des écosystèmes (`t_dsel`, couches dans `t_dcube` et `t_econ`)
    extra2: vec4<f32>,
    // terrain mélangé : écosystèmes des 4 tuiles du quart (5 chacune, la tuile (ligne x,
    // colonne z) en (ligne * 2 + colonne) * 5) : couche (−1 : aucune) et côté d'une répétition (m)
    eco_layer: array<vec4<f32>, 5>,
    eco_size: array<vec4<f32>, 5>,
};
@group(2) @binding(5) var<uniform> mat: MaterialU;
@group(2) @binding(6) var t_decal: texture_2d<f32>;
// détail répété du jeu : choix `_DS` (UV0), six motifs (UV1), couleur superposée `_OC` (UV1)
@group(2) @binding(7) var t_dsel: texture_2d<f32>;
@group(2) @binding(8) var t_dcube: texture_2d_array<f32>;
@group(2) @binding(9) var t_overlay: texture_2d<f32>;
// couleur et relief du maillage (UV0) : comme `s_tex`, mais sans boucler pour le terrain
@group(2) @binding(10) var s_main: sampler;
// terrain mélangé : relief des détails d'écosystème (une couche chacun, comme un `_N`)
@group(2) @binding(11) var t_econ: texture_2d_array<f32>;

struct VIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) joints: vec4<u32>,
    @location(4) weights: vec4<f32>,
    @location(5) tangent: vec4<f32>,
    @location(6) uv1: vec2<f32>,
    @location(7) uv2: vec2<f32>,
};

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) wpos: vec3<f32>,
    @location(3) tangent: vec4<f32>,
    @location(4) uv1: vec2<f32>,
    @location(5) uv2: vec2<f32>,
    // teinte de l'instance (flore du sol), 0 = aucune
    @location(6) tint: vec3<f32>,
};

// luminosité (linéaire) de _C pour laquelle le motif garde ses couleurs telles quelles
const CAMO_REF: f32 = 0.2;
// assombrissement des joints du détail répété (canal R de ses motifs)
const DETAIL_SEAMS: f32 = 0.35;
// couleur superposée : multiplication par 2 en sRGB, soit ≈ 2^2,2 en linéaire (gris = neutre)
const OVERLAY_GAIN: f32 = 4.59;
// luminosité (linéaire) d'une lumière de texture à pleine dose (canal B de _S = 1), avant
// l'intensité réglée dans la qualité graphique : 4 fois le seuil du bloom (les LED du jeu ont
// une dose de 0,5 à 0,7 : 2 à 3 en linéaire)
const EMISSIVE_GAIN: f32 = 4.0;

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// terrain mélangé : détail d'un écosystème (emplacement `k` du matériau), projeté sur les trois
// plans selon `w` (poids des plans yz, xz, xy). Gradients donnés : l'appel est conditionnel.
// `c` : couleur relative au terrain (RGB, 0,5 = inchangée) et hauteur de mélange (A) ; `n` :
// perturbation de la normale du monde.
struct EcoSample {
    c: vec4<f32>,
    n: vec3<f32>,
};
fn eco_sample(k: u32, p: vec3<f32>, w: vec3<f32>, dx: vec3<f32>, dy: vec3<f32>) -> EcoSample {
    var o = EcoSample(vec4<f32>(0.5), vec3<f32>(0.0));
    let layer = mat.eco_layer[k / 4u][k % 4u];
    if (layer < -0.5) { return o; }
    let li = i32(layer + 0.5);
    let s = 1.0 / max(mat.eco_size[k / 4u][k % 4u], 0.1);
    var c = vec4<f32>(0.0);
    var n = vec3<f32>(0.0);
    var tot = 0.0;
    if (w.y > 0.02) {
        let a = textureSampleGrad(t_dcube, s_tex, p.xz * s, li, dx.xz * s, dy.xz * s);
        let r = textureSampleGrad(t_econ, s_tex, p.xz * s, li, dx.xz * s, dy.xz * s);
        c += a * w.y;
        n += vec3<f32>(r.a * 2.0 - 1.0, 0.0, r.g * 2.0 - 1.0) * w.y;
        tot += w.y;
    }
    if (w.x > 0.02) {
        let a = textureSampleGrad(t_dcube, s_tex, p.zy * s, li, dx.zy * s, dy.zy * s);
        let r = textureSampleGrad(t_econ, s_tex, p.zy * s, li, dx.zy * s, dy.zy * s);
        c += a * w.x;
        n += vec3<f32>(0.0, r.g * 2.0 - 1.0, r.a * 2.0 - 1.0) * w.x;
        tot += w.x;
    }
    if (w.z > 0.02) {
        let a = textureSampleGrad(t_dcube, s_tex, p.xy * s, li, dx.xy * s, dy.xy * s);
        let r = textureSampleGrad(t_econ, s_tex, p.xy * s, li, dx.xy * s, dy.xy * s);
        c += a * w.z;
        n += vec3<f32>(r.a * 2.0 - 1.0, r.g * 2.0 - 1.0, 0.0) * w.z;
        tot += w.z;
    }
    if (tot > 0.0) {
        o.c = c / tot;
        o.n = n / tot;
    }
    return o;
}

@vertex
fn vs_main(v: VIn) -> VOut {
    let m = bones[v.joints.x] * v.weights.x
          + bones[v.joints.y] * v.weights.y
          + bones[v.joints.z] * v.weights.z
          + bones[v.joints.w] * v.weights.w;
    let wp = m * vec4<f32>(v.pos, 1.0);
    var o: VOut;
    o.clip = g.view_proj * wp;
    o.normal = (m * vec4<f32>(v.normal, 0.0)).xyz;
    o.uv = v.uv;
    o.wpos = wp.xyz;
    o.tangent = vec4<f32>((m * vec4<f32>(v.tangent.xyz, 0.0)).xyz, v.tangent.w);
    o.uv1 = v.uv1;
    o.uv2 = v.uv2;
    o.tint = vec3<f32>(0.0);
    return o;
}

// décor : modèles rigides instanciés, une matrice monde par instance (dans `bones`) ; sa ligne w
// porte la teinte de la flore du sol
@vertex
fn vs_static(v: VIn, @builtin(instance_index) inst: u32) -> VOut {
    let raw = bones[inst];
    let m = mat4x4<f32>(vec4<f32>(raw[0].xyz, 0.0), vec4<f32>(raw[1].xyz, 0.0), vec4<f32>(raw[2].xyz, 0.0), raw[3]);
    let wp = m * vec4<f32>(v.pos, 1.0);
    var o: VOut;
    o.clip = g.view_proj * wp;
    o.normal = (m * vec4<f32>(v.normal, 0.0)).xyz;
    o.uv = v.uv;
    o.wpos = wp.xyz;
    o.tangent = vec4<f32>((m * vec4<f32>(v.tangent.xyz, 0.0)).xyz, v.tangent.w);
    o.uv1 = v.uv1;
    o.uv2 = v.uv2;
    o.tint = vec3<f32>(raw[0].w, raw[1].w, raw[2].w);
    return o;
}

// filtrage bicubique Catmull-Rom en 9 lectures bilinéaires (Jorge Jimenez, Matt Pettineo),
// au niveau 0 : seulement là où la texture est agrandie
fn catmull_rom(t: texture_2d<f32>, s: sampler, uv: vec2<f32>, size: vec2<f32>) -> vec4<f32> {
    let sp = uv * size;
    let p1 = floor(sp - 0.5) + 0.5;
    let f = sp - p1;
    let w0 = f * (-0.5 + f * (1.0 - 0.5 * f));
    let w1 = 1.0 + f * f * (-2.5 + 1.5 * f);
    let w2 = f * (0.5 + f * (2.0 - 1.5 * f));
    let w3 = f * f * (-0.5 + 0.5 * f);
    let w12 = w1 + w2;
    let p0 = (p1 - 1.0) / size;
    let p3 = (p1 + 2.0) / size;
    let p12 = (p1 + w2 / w12) / size;
    var r = textureSampleLevel(t, s, vec2<f32>(p0.x, p0.y), 0.0) * w0.x * w0.y;
    r += textureSampleLevel(t, s, vec2<f32>(p12.x, p0.y), 0.0) * w12.x * w0.y;
    r += textureSampleLevel(t, s, vec2<f32>(p3.x, p0.y), 0.0) * w3.x * w0.y;
    r += textureSampleLevel(t, s, vec2<f32>(p0.x, p12.y), 0.0) * w0.x * w12.y;
    r += textureSampleLevel(t, s, vec2<f32>(p12.x, p12.y), 0.0) * w12.x * w12.y;
    r += textureSampleLevel(t, s, vec2<f32>(p3.x, p12.y), 0.0) * w3.x * w12.y;
    r += textureSampleLevel(t, s, vec2<f32>(p0.x, p3.y), 0.0) * w0.x * w3.y;
    r += textureSampleLevel(t, s, vec2<f32>(p12.x, p3.y), 0.0) * w12.x * w3.y;
    r += textureSampleLevel(t, s, vec2<f32>(p3.x, p3.y), 0.0) * w3.x * w3.y;
    // Catmull-Rom dépasse un peu de part et d'autre d'un contraste fort
    return clamp(r, vec4<f32>(0.0), vec4<f32>(1.0));
}

// eau : pente d'une houle (somme de vagues directionnelles), animée par le temps. Chaque vague
// s'efface quand sa longueur d'onde approche la taille d'un pixel au sol (`fp`, m) : sans cela,
// l'eau lointaine se couvre de moiré.
fn wave(p: vec2<f32>, d: vec2<f32>, freq: f32, amp: f32, speed: f32, t: f32, fp: f32) -> vec2<f32> {
    let k = 1.0 - smoothstep(0.08, 0.3, freq * fp / 6.2831853);
    return d * cos(dot(p, d) * freq + t * speed) * amp * k;
}
// eau des marais (matériau `SwampWater`) : côté d'une répétition de ses textures (m). Calés à
// l'œil sur des captures d'Hossin (algues larges d'un à deux mètres, bulles de quelques cm).
const ALGAE_TILE: f32 = 9.0;
const FOAM_TILE: f32 = 2.5;
const RIPPLE_TILE: f32 = 3.1;
const TWIRL_TILE: f32 = 7.3;

// couleur des algues : `Algae_OC` est sombre (sRGB 60 au plus) ; ×2,2 en linéaire (calé sur une
// capture d'Hossin : algues claires vers sRGB 112 / 119 / 60 en plein jour)
const ALGAE_GAIN: f32 = 2.2;

fn hash2(c: vec2<f32>) -> vec2<f32> {
    let q = vec2<f32>(dot(c, vec2<f32>(127.1, 311.7)), dot(c, vec2<f32>(269.5, 183.3)));
    return fract(sin(q) * 43758.5453);
}

// Ronds dans l'eau (gouttes, poissons : les `Water_Ripple` du jeu) : sur une grille de cases de
// 4 m, chaque case fait naître de temps en temps un rond en un point tiré au hasard. Le rond
// s'élargit à 0,7 m/s et s'éteint en 3 s ; l'onde est un paquet de quelques crêtes derrière
// son front. Pente radiale, filtrée comme la houle quand ses crêtes deviennent plus fines
// qu'un pixel. Les cases voisines comptent aussi (un rond déborde de sa case).
const RING_CELL: f32 = 4.0;
const RING_PERIOD: f32 = 5.0;
fn water_rings(p: vec2<f32>, t: f32, fp: f32) -> vec2<f32> {
    // (crêtes de 0,2 m : effacées en dessous de 4 pixels environ par crête)
    let k = 1.0 - smoothstep(0.025, 0.06, fp);
    if (k <= 0.0) { return vec2<f32>(0.0); }
    let cell = floor(p / RING_CELL);
    var s = vec2<f32>(0.0);
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let c = cell + vec2<f32>(f32(x), f32(y));
            let h = hash2(c);
            // une case sur deux seulement, et chacune à son rythme
            if (h.x > 0.55) { continue; }
            let period = RING_PERIOD * (0.7 + h.y);
            let phase = t / period + h.x * 7.31;
            let age = fract(phase) * period;
            // nouveau point à chaque rond
            let o = hash2(c + floor(phase) * 17.13);
            let d = p - (c + o) * RING_CELL;
            let r = length(d);
            let front = age * 0.7;
            let x0 = front - r;
            if (x0 < 0.0 || x0 > 0.6 || r < 1e-3) { continue; }
            // paquet de crêtes (longueur d'onde 0,2 m) derrière le front, qui s'éteint avec l'âge
            let env = smoothstep(0.0, 0.05, x0) * (1.0 - smoothstep(0.15, 0.6, x0));
            let a = 0.12 * env * exp(-age * 1.1) / (1.0 + r * 1.5);
            s += d / r * sin(x0 * 31.4) * a;
        }
    }
    return s * k;
}

// bruit de valeur lissé (0 à 1)
fn vnoise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = hash2(i).x;
    let b = hash2(i + vec2<f32>(1.0, 0.0)).x;
    let c = hash2(i + vec2<f32>(0.0, 1.0)).x;
    let d = hash2(i + vec2<f32>(1.0, 1.0)).x;
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

// Sillage des personnages qui marchent dans l'eau (points calculés par `wake.rs` à partir de la
// timeline) : `foam` eau brassée et blanchie (0 à 1), `clear` algues écartées (0 à 1), `slope`
// pente des ronds et des remous.
struct Wake {
    foam: f32,
    churn: f32,
    clear: f32,
    // algues repoussées en bourrelet sur les bords du passage
    edge: f32,
    slope: vec2<f32>,
};
// bulles d'écume : blanc verdâtre ; eau brassée : vase soulevée, laiteuse
const WAKE_FOAM: vec3<f32> = vec3<f32>(0.34, 0.38, 0.34);
const WAKE_MILKY: vec3<f32> = vec3<f32>(0.10, 0.12, 0.10);
fn water_wake(p: vec2<f32>, y: f32, t: f32, fp: f32) -> Wake {
    var w = Wake(0.0, 0.0, 0.0, 0.0, vec2<f32>(0.0));
    let n = min(i32(g.wake_n.x), 6);
    // crêtes des ronds (0,25 m) : effacées quand elles ne font plus que quelques pixels
    let kr = 1.0 - smoothstep(0.03, 0.08, fp);
    var churn = 0.0;
    // bords irréguliers (sinon, une bande aux bords tirés au cordeau)
    let jit = 1.0 + (vnoise(p * 1.9 + vec2<f32>(t * 0.15, 0.0)) - 0.5) * 0.7 + (vnoise(p * 5.3) - 0.5) * 0.3;
    for (var c = 0; c < n; c++) {
        let h = g.wake_head[c];
        // autre nappe d'eau, ou trop loin
        if (abs(y - h.w) > 0.4 || distance(p, h.xy) > h.z) { continue; }
        let r = g.wake_range[c];
        let s0 = i32(r.x);
        let nt = i32(r.y);
        // traînée : distance au trajet des chevilles, morceau par morceau
        for (var k = 0; k < nt - 1; k++) {
            let a = g.wake_pt[s0 + k];
            let b = g.wake_pt[s0 + k + 1];
            if (a.w <= 0.0 && b.w <= 0.0) { continue; }
            let ab = b.xy - a.xy;
            let l2 = dot(ab, ab);
            var u = 0.0;
            if (l2 > 1e-6) { u = clamp(dot(p - a.xy, ab) / l2, 0.0, 1.0); }
            let d = length(p - a.xy - ab * u) * jit;
            let age = mix(a.z, b.z, u);
            let s = mix(a.w, b.w, u);
            // vitesse du personnage sur ce morceau (m/s) : debout, l'eau se calme
            let mv = smoothstep(0.3, 3.0, sqrt(l2) / max(b.z - a.z, 1e-3));
            // eau brassée derrière les jambes : elle s'élargit et retombe en quelques secondes
            let fw = 0.45 + 0.35 * age;
            churn = max(churn, s * mix(0.2, 1.0, mv) * exp(-age / 1.6) * (1.0 - smoothstep(fw * 0.25, fw, d)));
            // passage ouvert dans les algues, qui se referme lentement
            let cw = 0.7 + 0.08 * age;
            let keep = s * exp(-age / 9.0);
            w.clear = max(w.clear, keep * (1.0 - smoothstep(cw * 0.5, cw, d)));
            w.edge = max(w.edge, keep * smoothstep(cw * 0.7, cw, d) * (1.0 - smoothstep(cw, cw + 0.4, d)));
        }
        // ronds partis des jambes : ils s'élargissent à 1 m/s ; paquet de crêtes derrière le front
        let nr = i32(r.z);
        for (var k = 0; k < nr; k++) {
            let e = g.wake_pt[s0 + nt + k];
            if (e.w <= 0.0) { continue; }
            let dv = p - e.xy;
            let rr = length(dv);
            let x0 = 0.25 + e.z - rr;
            if (x0 < 0.0 || x0 > 0.75 || rr < 1e-3) { continue; }
            let env = smoothstep(0.0, 0.06, x0) * (1.0 - smoothstep(0.2, 0.75, x0));
            let amp = 0.45 * e.w * env * exp(-e.z * 0.8) / (1.0 + rr * 0.5);
            w.slope += dv / rr * sin(x0 * 25.1) * amp * kr;
        }
    }
    if (churn > 0.0) {
        // remous : bruit animé, en grandes et petites bulles
        let q = p * 4.1 + vec2<f32>(t * 0.37, -t * 0.29);
        let n1 = vnoise(q) + 0.5 * vnoise(q * 2.7 + 13.1);
        let n2 = vnoise(q + vec2<f32>(5.3, 9.7)) + 0.5 * vnoise(q * 2.7 + 41.7);
        w.slope += (vec2<f32>(n1, n2) - 0.75) * 0.8 * churn;
        // écume en plaques : elle couvre presque tout juste derrière les jambes, puis se
        // clairsème en retombant (seules les plus grosses plaques restent)
        // (les détails fins s'effacent quand ils deviennent plus petits qu'un pixel : sinon, du
        // scintillement au loin)
        let k29 = 1.0 - smoothstep(0.015, 0.04, fp);
        let k23 = 1.0 - smoothstep(0.02, 0.05, fp);
        let k61 = 1.0 - smoothstep(0.008, 0.02, fp);
        let f = vnoise(p * 3.7 - vec2<f32>(t * 0.6, t * 0.2)) * 0.5 + vnoise(p * 11.3 + vec2<f32>(t * 0.9, 0.0)) * 0.3
            + mix(0.5, vnoise(p * 29.0 - vec2<f32>(0.0, t * 1.4)), k29) * 0.2;
        let th = 1.0 - churn * 0.65;
        // grain des bulles : grosses et petites, claires et sombres
        let grain = mix(0.5, vnoise(p * 23.0 + vec2<f32>(t * 1.7, -t * 1.3)), k23) * 0.5
            + mix(0.5, vnoise(p * 61.0 - vec2<f32>(t * 2.1)), k61) * 0.5;
        w.foam = clamp(smoothstep(th - 0.05, th + 0.2, f) * (0.45 + 0.55 * churn) * (0.3 + 1.0 * grain), 0.0, 1.0);
    }
    w.churn = churn;
    return w;
}

// Personnage mouillé : sous la ligne d'eau (plus haut quand il court : éclaboussures), la tenue
// est plus sombre et plus brillante. 0 à 1.
fn water_wet(wp: vec3<f32>) -> f32 {
    var wet = 0.0;
    let n = min(i32(g.wake_n.x), 6);
    for (var c = 0; c < n; c++) {
        let r = g.wake_range[c];
        let cur = g.wake_pt[i32(r.x)];
        if (cur.w <= 0.0 || distance(wp.xz, cur.xy) > 1.0) { continue; }
        let lvl = g.wake_head[c].w;
        let line = lvl + 0.08 + 0.32 * smoothstep(0.5, 5.0, r.w) + (vnoise(wp.xz * 9.0 + wp.y * 3.0) - 0.5) * 0.08;
        wet = max(wet, cur.w * (1.0 - smoothstep(line - 0.04, line + 0.06, wp.y)));
    }
    return wet;
}

// relief d'une texture DXT5nm de l'eau (X dans A, Y dans G), en pente
fn water_nm(t: texture_2d<f32>, uv: vec2<f32>, bias: f32) -> vec2<f32> {
    let s = textureSampleBias(t, s_tex, uv, bias);
    return vec2<f32>(s.a * 2.0 - 1.0, s.g * 2.0 - 1.0);
}

fn water_slope(p: vec2<f32>, t: f32, fp: f32) -> vec2<f32> {
    // amplitudes proches d'une octave à l'autre et directions variées : une eau calme, sans
    // bandes régulières
    var s = wave(p, vec2<f32>(0.80, 0.60), 0.7, 0.022, 0.9, t, fp);
    s += wave(p, vec2<f32>(-0.40, 0.92), 1.3, 0.022, 1.3, t, fp);
    s += wave(p, vec2<f32>(0.97, -0.24), 2.3, 0.020, 1.9, t, fp);
    s += wave(p, vec2<f32>(-0.70, -0.71), 3.9, 0.018, 2.5, t, fp);
    s += wave(p, vec2<f32>(0.20, 0.98), 6.7, 0.015, 3.4, t, fp);
    s += wave(p, vec2<f32>(-0.95, 0.31), 11.3, 0.012, 4.4, t, fp);
    return s;
}

@fragment
fn fs_main(i: VOut) -> @location(0) vec4<f32> {
    let v = normalize(g.cam_pos.xyz - i.wpos);
    // biais de mip (négatif : textures plus nettes, pour les images suréchantillonnées)
    let bias = g.quality.z;
    var dif = textureSampleBias(t_diffuse, s_main, i.uv, bias);
    var tn = textureSampleBias(t_normal, s_main, i.uv, bias);
    var ts = textureSampleBias(t_spec, s_tex, i.uv, bias);
    // feuillages : l'alpha des mips s'amenuise au loin (le feuillage se dégarnit) ; on le
    // renforce un peu selon le niveau de mip lu (plus, il fait ressortir les bords colorés des
    // textures). Largeur d'un pixel en alpha pour lisser la découpe.
    let tsz = vec2<f32>(textureDimensions(t_diffuse));
    let duvx = dpdx(i.uv * tsz);
    let duvy = dpdy(i.uv * tsz);
    // niveau de mip de la couleur (négatif : un texel couvre plusieurs pixels)
    let lod_d = 0.5 * log2(max(max(dot(duvx, duvx), dot(duvy, duvy)), 1e-12)) + bias;
    let mip = max(lod_d, 0.0);
    let alpha_w = max(fwidth(dif.a), 1e-4);
    // taille d'un pixel au sol (m), pour filtrer les vagues de l'eau
    let foot = max(length(dpdx(i.wpos.xz)), length(dpdy(i.wpos.xz)));
    // gros plans : un texel couvre plusieurs pixels ; le filtrage bilinéaire y dessine des
    // losanges et des marches (surtout sur le relief). Filtrage bicubique (Catmull-Rom) à la
    // place, en fondu entre 0 et −1 niveau de mip. L'alpha garde le bilinéaire (découpe des
    // feuillages inchangée).
    if (g.quality.w > 0.5) {
        let nsz = vec2<f32>(textureDimensions(t_normal));
        let ssz = vec2<f32>(textureDimensions(t_spec));
        let kd = 1.0 - smoothstep(-1.0, 0.0, lod_d);
        let kn = 1.0 - smoothstep(-1.0, 0.0, lod_d + log2(nsz.x / tsz.x));
        let ks = 1.0 - smoothstep(-1.0, 0.0, lod_d + log2(ssz.x / tsz.x));
        if (kd > 0.0) { dif = vec4<f32>(mix(dif.rgb, catmull_rom(t_diffuse, s_main, i.uv, tsz).rgb, kd), dif.a); }
        if (kn > 0.0) { tn = mix(tn, catmull_rom(t_normal, s_main, i.uv, nsz), kn); }
        if (ks > 0.0) { ts = mix(ts, catmull_rom(t_spec, s_tex, i.uv, ssz), ks); }
    }
    var base = dif.rgb * actor.tint.rgb;
    // couleur des lumières : celle de _C seule, avant camouflage, décalcomanie, détail répété et
    // couleur superposée (sinon la LED prend le grain du motif et s'assombrit avec lui)
    let emis_base = base;
    // flore du sol : couleur × teinte de la plante, là où le masque `_TM` est blanc (lu hors de
    // toute condition : la teinte varie d'une instance à l'autre)
    let tint_k = textureSampleBias(t_overlay, s_tex, i.uv, bias).r;
    if (i.tint.x + i.tint.y + i.tint.z > 0.0) {
        let k = select(1.0, tint_k, mat.extra.w > 0.5);
        base = base * mix(vec3<f32>(1.0), i.tint, k);
    }

    // camouflage : motif répété sur le dépliage UV1, masqué par le canal B de _N.
    // Le motif est modulé par la luminosité de _C : une tenue sombre garde un camouflage sombre,
    // et les plis et l'occlusion peints restent visibles.
    let camo_rgb = textureSampleBias(t_camo, s_tex, i.uv1 * actor.camo.y, bias).rgb;
    let camo_w = mat.flags.x * actor.camo.x * smoothstep(0.35, 0.65, tn.b);
    let shade = clamp(luma(base) / CAMO_REF, 0.0, 2.0);
    base = mix(base, camo_rgb * shade, camo_w);

    // décalcomanie : carré 0..1 du 3e jeu d'UV, posée par son alpha, par-dessus le camouflage
    let dec = textureSampleBias(t_decal, s_tex, i.uv2, bias);
    let inside = step(0.0, i.uv2.x) * step(i.uv2.x, 1.0) * step(0.0, i.uv2.y) * step(i.uv2.y, 1.0);
    base = mix(base, dec.rgb, dec.a * inside * mat.flags.y * actor.decal.x);

    // détail répété du jeu (bâtiments, pads, véhicules, tenues) : `_DS` code une direction
    // (RGB) qui désigne une face de la cubemap de motifs, et la dose (A). La face est un motif
    // répété sur UV1, autant de fois que le dit le matériau (40 sur les tenues et les véhicules) :
    // relief fin (G, A) et joints (R). Sans lui, une texture étirée sur des
    // dizaines de mètres reste floue de près.
    var detail_xy = vec2<f32>(0.0);
    // (l'eau range ses propres réglages dans `extra`)
    let water = mat.flags.w > 1.5;
    if (mat.extra.x > 0.5 && !water) {
        let sel = textureSampleBias(t_dsel, s_tex, i.uv, bias);
        let d = sel.rgb * 2.0 - 1.0;
        let a = abs(d);
        var layer = select(5, 4, d.z > 0.0);
        if (a.x >= a.y && a.x >= a.z) {
            layer = select(1, 0, d.x > 0.0);
        } else if (a.y >= a.z) {
            layer = select(3, 2, d.y > 0.0);
        }
        let dt = textureSampleBias(t_dcube, s_tex, i.uv1 * mat.extra2.x, layer, bias);
        let k = sel.a;
        detail_xy = vec2<f32>(dt.a * 2.0 - 1.0, dt.g * 2.0 - 1.0) * k;
        base = base * (1.0 - dt.r * DETAIL_SEAMS * k);
    }
    // couleur superposée (`_OC`, gris moyen = neutre, en sRGB) là où le canal B du relief l'autorise
    if (mat.extra.y > 0.5 && !water) {
        let oc = textureSampleBias(t_overlay, s_tex, i.uv1, bias).rgb;
        base = base * mix(vec3<f32>(1.0), oc * OVERLAY_GAIN, tn.b);
    }

    // normale géométrique, retournée pour les faces vues de dos
    let n0 = normalize(i.normal);
    var ng = n0;
    if (dot(ng, v) < 0.0) { ng = -ng; }
    var n = ng;
    // relief : X dans A, Y dans G, convention « Y vers le haut de l'image » (= binormale)
    if (abs(i.tangent.w) > 0.001 && g.flags.x > 0.5) {
        let t = normalize(i.tangent.xyz - n0 * dot(n0, i.tangent.xyz));
        let b = cross(n0, t) * sign(i.tangent.w);
        let x = tn.a * 2.0 - 1.0 + detail_xy.x;
        let y = tn.g * 2.0 - 1.0 + detail_xy.y;
        let z = sqrt(max(1.0 - x * x - y * y, 0.0));
        let p = normalize(t * x + b * y + n0 * z);
        // sur une face vue de dos, on garde la même perturbation de l'autre côté
        n = normalize(p - n0 * dot(p, n0) + ng * abs(dot(p, n0)));
    }
    // terrain : la carte donne directement la normale du monde (X dans A, Z dans G), plus fine
    // que le maillage
    let terrain = mat.flags.w > 0.5 && mat.flags.w < 1.5;
    var water_rough = 0.05;
    var water_refl = 1.0;
    if (water) {
        // eau : sombre (couleur `Tint` du matériau), presque tout est reflet
        base = mat.detail.rgb * 0.1;
        let p = i.wpos.xz;
        // sillage des personnages qui marchent dans l'eau
        var wk = Wake(0.0, 0.0, 0.0, 0.0, vec2<f32>(0.0));
        if (g.wake_n.x > 0.5) { wk = water_wake(p, i.wpos.y, g.quality2.w, foot); }
        var wslope = vec2<f32>(0.0);
        if (mat.extra.x > 0.5 || mat.extra.y > 0.5) {
            // reliefs du jeu, répétés en mètres et animés par la timeline (qualité haute) :
            // deux couches de rides qui glissent en sens contraires, et les tourbillons
            // (`water_twirl90`, tournés d'un quart de tour)
            let t = select(0.0, g.quality2.w, g.quality2.z > 0.5);
            var rip = water_nm(t_normal, p / RIPPLE_TILE + vec2<f32>(0.031, 0.012) * t, bias);
            rip += water_nm(t_normal, vec2<f32>(p.y, -p.x) / (RIPPLE_TILE * 1.53) + vec2<f32>(-0.019, 0.027) * t, bias);
            let tw = water_nm(t_spec, vec2<f32>(-p.y, p.x) / TWIRL_TILE + vec2<f32>(0.0, 0.011) * t, bias) * mat.extra.y;
            var slope = rip * (0.17 * mat.extra.x) + tw * 0.3;
            if (mat.extra.w > 0.5) {
                // nappe d'algues des marais : RGB leur couleur, A l'eau libre. Elle dérive
                // lentement, déformée par les tourbillons.
                let oc = textureSampleBias(t_overlay, s_tex, p / ALGAE_TILE + tw * 0.01 + vec2<f32>(0.0011, 0.0006) * t, bias);
                // (le passage d'un personnage écarte les algues, qui s'amassent sur ses bords)
                let open = max(smoothstep(0.45, 0.85, oc.a) * (1.0 - wk.edge * 0.8), wk.clear);
                // grain de bulles des algues (`water_foam_algae_C` : luminosité autour de 0,07,
                // bulles claires dans l'alpha)
                let foam = textureSampleBias(t_diffuse, s_main, p / FOAM_TILE, bias);
                let grain = mix(1.0, clamp(luma(foam.rgb) / 0.07, 0.0, 2.5), 0.8) + foam.a * 0.6;
                // teinte `Tint` du matériau (olive sans bleu) à mi-force : algues jaune-vert
                let hue = mat.detail.rgb / max(max(mat.detail.r, mat.detail.g), max(mat.detail.b, 1e-3));
                let algae = oc.rgb * ALGAE_GAIN * grain * mix(vec3<f32>(1.0), hue, 0.5);
                // l'eau libre garde un peu du marbré des algues en suspension
                base = mix(algae, base * (0.5 + 2.0 * luma(oc.rgb * OVERLAY_GAIN)), open);
                // les algues amortissent les rides : relief des bulles à la place
                let fb = water_nm(t_camo, p / FOAM_TILE, bias) * mat.extra.z;
                // ronds dans l'eau libre (gouttes, poissons), seulement si l'eau est animée
                if (g.quality2.z > 0.5) { slope += water_rings(p, t, foot) * open; }
                slope = mix(fb * 0.45, slope, open);
                water_rough = mix(0.6, 0.05, open);
                water_refl = 0.6;
            }
            wslope = slope;
        } else if (g.quality2.z > 0.5) {
            // houle animée
            wslope = water_slope(p, g.quality2.w, foot);
        } else {
            // légères rides fixes
            let rx = sin(p.x * 1.7 + p.y * 0.6) * 0.5 + sin(p.x * 4.1 - p.y * 3.3) * 0.25;
            let rz = sin(p.y * 1.9 - p.x * 0.8) * 0.5 + sin(p.y * 3.7 + p.x * 2.9) * 0.25;
            wslope = -vec2<f32>(rx, rz) * 0.04;
        }
        // sillage par-dessus : ronds et remous, eau blanchie (plus mate, reflet moindre)
        wslope += wk.slope;
        base = mix(base, WAKE_MILKY, wk.churn * 0.5);
        base = mix(base, WAKE_FOAM, wk.foam);
        water_rough = mix(water_rough, 0.35, wk.foam);
        water_refl = mix(water_refl, 0.35, wk.foam);
        n = normalize(ng + vec3<f32>(-wslope.x, 0.0, -wslope.y));
    }
    // personnage mouillé sous la ligne d'eau (le décor a `actor.flags.y` à 1)
    var wet = 0.0;
    if (!water && !terrain && actor.flags.y < 0.5 && g.wake_n.x > 0.5) {
        wet = water_wet(i.wpos);
        base = base * mix(1.0, 0.55, wet);
    }
    if (terrain && g.flags.x > 0.5) {
        let x = tn.a * 2.0 - 1.0;
        let z = tn.g * 2.0 - 1.0;
        n = normalize(vec3<f32>(x, sqrt(max(1.0 - x * x - z * z, 0.0)), z));
    }
    // terrain : détail des écosystèmes de près (texture du sol vue de dessus, roche projetée
    // sur les trois axes dans les pentes). Seule la luminosité du détail module la couleur.
    if (terrain) {
        // distance du détail réglée par la qualité du terrain (110 m : réglage d'origine)
        var fade = 1.0 - smoothstep(g.quality2.y * 0.18, g.quality2.y, length(g.cam_pos.xyz - i.wpos));
        // bords du quart (128 m) dont le voisin a un autre détail : estompé sur 16 m, pour passer
        // de l'un à l'autre sans marche. UV : x suit z du monde, y suit x.
        let edges = u32(mat.extra.z + 0.5);
        let e = 16.0 / 128.0;
        if ((edges & 1u) != 0u) { fade *= smoothstep(0.0, e, i.uv.y); }
        if ((edges & 2u) != 0u) { fade *= smoothstep(0.0, e, 1.0 - i.uv.y); }
        if ((edges & 4u) != 0u) { fade *= smoothstep(0.0, e, i.uv.x); }
        if ((edges & 8u) != 0u) { fade *= smoothstep(0.0, e, 1.0 - i.uv.x); }
        if (mat.extra2.y > 0.5) {
        // mélange au pixel près (carte de 1 m par pixel du jeu) : l'écosystème de fond de la
        // tuile, puis ceux de R, V, B, A peints l'un sur l'autre. La hauteur de mélange de chaque
        // détail (cailloux, touffes) le fait percer avant les creux du précédent : transitions
        // découpées comme dans le jeu, pas un fondu.
        let cell = clamp(floor(i.uv * 2.0), vec2<f32>(0.0), vec2<f32>(1.0));
        let slot = (u32(cell.y) * 2u + u32(cell.x)) * 5u;
        // (lu dans sa tuile seulement : les canaux d'une tuile à l'autre ne désignent pas les
        // mêmes écosystèmes)
        let tp = clamp(i.uv * 128.0, cell * 64.0 + 0.5, cell * 64.0 + 63.5) / 128.0;
        let bw = textureSampleLevel(t_dsel, s_main, tp, 0.0);
        var w = pow(abs(n), vec3<f32>(4.0));
        w = w / max(w.x + w.y + w.z, 1e-4);
        let gs = exp2(bias);
        let gx = dpdx(i.wpos) * gs;
        let gy = dpdy(i.wpos) * gs;
        let cv = vec4<f32>(1.0) - bw;
        let cover = cv.x * cv.y * cv.z * cv.w;
        var col = vec4<f32>(0.5);
        var dn = vec3<f32>(0.0);
        if (fade > 0.001) {
            if (cover > 0.003) {
                let s0 = eco_sample(slot, i.wpos, w, gx, gy);
                col = s0.c;
                dn = s0.n;
            }
            for (var k = 0u; k < 4u; k++) {
                let a = bw[k];
                if (a > 0.003) {
                    let sk = eco_sample(slot + k + 1u, i.wpos, w, gx, gy);
                    let ah = clamp(a + (sk.c.a - 0.5) * 3.0 * min(a, 1.0 - a), 0.0, 1.0);
                    col = mix(col, sk.c, ah);
                    dn = mix(dn, sk.n, ah);
                }
            }
        }
        base = base * mix(vec3<f32>(1.0), col.rgb * 2.0, fade * 0.9);
        if (g.flags.x > 0.5) {
            n = normalize(n + dn * fade * 0.8);
        }
        } else {
        let ds = max(mat.detail.xy, vec2<f32>(0.1));
        let gd = textureSampleBias(t_spec, s_tex, i.wpos.xz / ds.x, bias);
        let rx = textureSampleBias(t_camo, s_tex, i.wpos.zy / ds.y, bias);
        let ry = textureSampleBias(t_camo, s_tex, i.wpos.xz / ds.y, bias);
        let rz = textureSampleBias(t_camo, s_tex, i.wpos.xy / ds.y, bias);
        var w = pow(abs(n0), vec3<f32>(4.0));
        w = w / max(w.x + w.y + w.z, 1e-4);
        let gk = mat.detail.z;
        let rk = (1.0 - smoothstep(0.62, 0.85, n.y)) * mat.detail.w;
        let lum_g = mix(1.0, gd.r * 2.0, gk);
        let lum_r = (rx.r * w.x + ry.r * w.y + rz.r * w.z) * 2.0;
        base = base * mix(1.0, mix(lum_g, lum_r, rk), fade * 0.85);
        if (g.flags.x > 0.5) {
            let pg = vec3<f32>(gd.a * 2.0 - 1.0, 0.0, gd.g * 2.0 - 1.0) * gk;
            let pr = vec3<f32>(0.0, rx.g * 2.0 - 1.0, rx.a * 2.0 - 1.0) * w.x
                + vec3<f32>(ry.a * 2.0 - 1.0, 0.0, ry.g * 2.0 - 1.0) * w.y
                + vec3<f32>(rz.a * 2.0 - 1.0, rz.g * 2.0 - 1.0, 0.0) * w.z;
            n = normalize(n + mix(pg, pr, rk) * fade * 0.8);
        }
        }
    }

    // _S : R métal, G brillance (plus forte sur l'œil que sur la peau : rugosité = 1 − G),
    // B émissif, A occlusion spéculaire
    // les feuillages n'ont ni métal ni émissif (leur _S ne suit pas cette convention : sans
    // cela, le feuillage scintille)
    // le terrain non plus (pas de `_S` : sol mat)
    let leaf = mat.flags.z > 0.5 || terrain || water;
    let metal = select(ts.r, 0.0, leaf);
    let rough0 = select(clamp(1.0 - ts.g, 0.08, 1.0), select(select(0.85, 0.9, terrain), water_rough, water), leaf);
    // tissu ou armure mouillés : film d'eau brillant
    let rough = mix(rough0, max(rough0 * 0.45, 0.12), wet);
    let emis = select(ts.b, 0.0, leaf);
    // (le `_S` du terrain contient son détail, pas une occlusion)
    // (l'eau des marais reflète moins : eau trouble, film d'algues)
    let occ = select(select(ts.a, 1.0, terrain), water_refl, water);

    // anticrénelage des reflets : là où la normale varie vite d'un pixel à l'autre (relief fin,
    // courbures, vagues au loin), la surface est traitée comme plus rugueuse. Sans cela, les
    // reflets du soleil scintillent en points isolés (Kaplanyan et Tokuyoshi).
    let dndx = dpdx(n);
    let dndy = dpdy(n);
    let n_var = 0.25 * (dot(dndx, dndx) + dot(dndy, dndy));
    let kernel = min(2.0 * n_var, 0.18) * g.quality2.x;

    let l = normalize(-g.light_dir.xyz);
    let h = normalize(l + v);
    // ombre portée : décalage le long de la normale géométrique tournée vers le soleil
    var nl = n0;
    if (dot(nl, l) < 0.0) { nl = -nl; }
    let sun_c = char_shadow_at(i.wpos, nl);
    // (actor.flags.z : personnage que le décor n'ombre pas, éclairage de cinéma)
    let sun_d = select(decor_shadow_at(i.wpos, nl), 1.0, actor.flags.z > 0.5);
    let ndl = max(dot(n, l), 0.0) * sun_c * sun_d;
    let ndh = max(dot(n, h), 0.0);
    let a = rough * rough;
    let a2 = min(a * a + kernel, 1.0);
    // rugosité élargie, pour les reflets d'ambiance
    let rough_r = sqrt(sqrt(a2));
    let shin = clamp(2.0 / a2 - 2.0, 2.0, 4096.0);
    let f0 = mix(vec3<f32>(0.04), base, metal);
    let fres = f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - max(dot(h, v), 0.0), 5.0);
    let spec = fres * ((shin + 2.0) / 8.0) * pow(ndh, shin) * ndl * occ * g.sun.rgb;

    let kd = base * (1.0 - metal * 0.85);
    let lamps = decor_lamps(i.wpos, n, v, kd, f0, shin, occ);
    // eau de qualité haute avec le ciel de la carte : elle reflète ce ciel net (le panorama
    // d'ambiance n'a que 256×128 pixels) ; un rayon renvoyé vers le bas par une vague est
    // ramené juste au-dessus de l'horizon
    let water_sky = water && g.quality2.z > 1.5;
    var rw = reflect(-v, n);
    rw = normalize(vec3<f32>(rw.x, max(rw.y, 0.02), rw.z));
    let sky_refl = sky_color(rw) / max(g.quality.y, 1e-3);
    var c: vec3<f32>;
    if (g.env.x > 0.5) {
        // ambiance du jeu : diffus par harmoniques sphériques, reflets dans le panorama flouté
        let irr = env_irradiance(env_dir(n)) * g.env.w;
        let r = env_dir(reflect(-v, n));
        var pre = env_radiance(r, rough_r * g.env.z) * g.env.w;
        if (water_sky) { pre = sky_refl; }
        let spec_env = pre * env_brdf(f0, rough_r, max(dot(n, v), 0.0)) * occ;
        c = kd * (irr + ndl * g.sun.rgb) + spec + spec_env;
    } else {
        // éclairage fixe du studio, à l'échelle de l'intensité d'ambiance (flags.z, 1 = d'origine)
        let fill = max(dot(n, normalize(vec3<f32>(-0.5, 0.2, -0.6))), 0.0) * 0.22 * g.flags.z;
        let hemi = mix(0.22, 0.42, n.y * 0.5 + 0.5) * g.flags.z;
        // reflet d'ambiance approximatif : sans lui, le métal serait noir
        var env = f0 * hemi * occ * (1.0 - rough_r * 0.7) * 1.6;
        if (water_sky) { env = sky_refl * env_brdf(f0, rough_r, max(dot(n, v), 0.0)) * occ; }
        c = kd * (hemi + ndl * g.sun.rgb + fill) + spec + env;
    }
    c = c + lamps;
    // décor : l'ombre des personnages assombrit aussi l'ambiance, à l'opacité « Ombre au sol »
    // (comme le sol invisible du studio). Sans cela, elle ne retire que le soleil direct : presque
    // rien sur un sol sombre ou sous un soleil rasant. Seulement là où le soleil arrive (pas dans
    // l'ombre d'un bâtiment, pas sur une face tournée à l'opposé).
    let facing = smoothstep(0.0, 0.05, dot(ng, l));
    c = c * (1.0 - actor.flags.y * g.shadow.z * (1.0 - sun_c) * sun_d * facing);
    // lumières (néons, LED, voyants), portées au-delà de 1 pour que le bloom les auréole. Une
    // LED turquoise, rouge ou violette est peinte sombre dans _C (≈ 0,06 à 0,27 en linéaire) :
    // multipliée telle quelle, elle restait sous le seuil du bloom et paraissait mate. Sa teinte
    // est donc ramenée vers la pleine intensité, d'autant plus qu'elle est saturée ; une lumière
    // grise ou blanche garde la luminosité peinte (son dégradé fait le bord du trait). Le
    // plancher garde éteinte une zone presque noire.
    let e_max = max(emis_base.r, max(emis_base.g, emis_base.b));
    let e_min = min(emis_base.r, min(emis_base.g, emis_base.b));
    let e_sat = 1.0 - e_min / max(e_max, 1e-4);
    let e_col = emis_base * mix(1.0, 1.0 / max(e_max, 0.15), e_sat);
    c = c + e_col * emis * EMISSIVE_GAIN * g.flags.y;
    let rim = pow(1.0 - max(dot(n, v), 0.0), 3.0);
    c = c * g.quality.y;
    c = c + actor.flags.x * rim * 0.45 * vec3<f32>(1.0, 0.75, 0.2);
    // feuillages : découpe par l'alpha de la couleur (en fin de shader : les lectures de
    // texture restent en flot de contrôle uniforme). flags.w : 0 découpe nette d'origine ;
    // 1 alpha renforcé au loin ; 2 en plus, bord lissé sur un pixel par « alpha to coverage »
    // (l'alpha choisit la part des échantillons MSAA couverts)
    var out_a = 1.0;
    if (mat.flags.z > 0.5) {
        var fa = dif.a;
        if (g.flags.w > 0.5) { fa = fa * (1.0 + min(mip, 4.0) * 0.1); }
        if (g.flags.w > 1.5) {
            out_a = clamp((fa - 0.5) / alpha_w + 0.5, 0.0, 1.0);
            if (out_a <= 0.0) { discard; }
        } else if (fa < 0.5) {
            discard;
        }
    }
    c = sun_shafts(c, i.wpos, i.clip.xy);
    return vec4<f32>(apply_fog(c, i.wpos), out_a);
}

// champ de force (boucliers des bases, portes IFF) : film translucide et lumineux à la
// couleur du matériau (couleur de faction donnée par la palette de l'acteur, dans `mat.detail`),
// avec un réseau d'hexagones, de fines lignes et des bords plus vifs vus de biais
fn hex_edge(p: vec2<f32>) -> f32 {
    let s = vec2<f32>(1.0, 1.7320508);
    let a = p - floor(p / s) * s - s * 0.5;
    let q = p - s * 0.5;
    let b = q - floor(q / s) * s - s * 0.5;
    let gv = select(b, a, dot(a, a) < dot(b, b));
    let ag = abs(gv);
    // 0 au centre d'une cellule, 0,5 sur son bord
    return max(dot(ag, vec2<f32>(0.5, 0.8660254)), ag.x);
}
@fragment
fn fs_forcefield(i: VOut) -> @location(0) vec4<f32> {
    let v = normalize(g.cam_pos.xyz - i.wpos);
    let n = normalize(i.normal);
    let ndv = abs(dot(n, v));
    let col = mat.detail.rgb;
    // repère du plan de la surface, pour un motif à l'échelle du monde (cellules de 35 cm)
    let up = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), abs(n.y) > 0.9);
    let t = normalize(cross(up, n));
    let b = cross(n, t);
    let p = vec2<f32>(dot(i.wpos, t), dot(i.wpos, b)) / 0.35;
    // de loin, une cellule ne couvre que quelques pixels : le motif s'efface au lieu de scintiller
    let fw = fwidth(p);
    let fine = 1.0 - smoothstep(0.03, 0.12, max(fw.x, fw.y));
    let hex = smoothstep(0.44, 0.5, hex_edge(p)) * fine;
    // fines lignes parallèles au sol
    let scan = pow(abs(sin(dot(i.wpos, b) * 9.0)), 40.0) * fine;
    let fres = pow(1.0 - ndv, 3.0);
    // bord du maillage (UV 0..1 des panneaux)
    let e = min(min(i.uv.x, 1.0 - i.uv.x), min(i.uv.y, 1.0 - i.uv.y));
    let inside = step(0.0, i.uv.x) * step(i.uv.x, 1.0) * step(0.0, i.uv.y) * step(i.uv.y, 1.0);
    let rim = (1.0 - smoothstep(0.0, 0.02, e)) * inside;
    let alpha = clamp(0.10 + hex * 0.14 + scan * 0.06 + fres * 0.4 + rim * 0.35, 0.0, 0.85);
    let glow = 0.12 + hex * 0.5 + scan * 0.25 + fres * 0.9 + rim * 1.4;
    var c = col * (alpha * 0.3 + glow * 0.7) * g.quality.y;
    c = apply_fog(c, i.wpos);
    // sortie prémultipliée : la couleur s'ajoute, l'alpha voile ce qui est derrière
    return vec4<f32>(c, alpha);
}

// sol transparent qui ne montre que l'ombre des personnages
struct GOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) wpos: vec3<f32>,
};
@vertex
fn vs_ground(@builtin(vertex_index) vi: u32) -> GOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0),
    );
    let c = corners[vi];
    let wp = vec3<f32>(g.ground.x + c.x * g.ground.z, g.ground.w, g.ground.y + c.y * g.ground.z);
    var o: GOut;
    o.clip = g.view_proj * vec4<f32>(wp, 1.0);
    o.wpos = wp;
    return o;
}
@fragment
fn fs_ground(i: GOut) -> @location(0) vec4<f32> {
    let lit = char_shadow_at(i.wpos, vec3<f32>(0.0, 1.0, 0.0));
    return vec4<f32>(0.0, 0.0, 0.0, (1.0 - lit) * g.shadow.z);
}

struct LIn {
    @location(0) pos: vec3<f32>,
    @location(1) color: vec4<f32>,
};
struct LOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
};
@vertex
fn vs_line(v: LIn) -> LOut {
    var o: LOut;
    o.clip = g.view_proj * vec4<f32>(v.pos, 1.0);
    o.color = v.color;
    return o;
}
@fragment
fn fs_line(i: LOut) -> @location(0) vec4<f32> {
    return i.color;
}
"#;

/// Passe d'ombre : profondeur des personnages vue du soleil.
const SHADOW_SHADER: &str = r#"
struct ShadowU {
    light_vp: mat4x4<f32>,
};
@group(0) @binding(0) var<uniform> sg: ShadowU;
@group(1) @binding(0) var<storage, read> bones: array<mat4x4<f32>>;

struct VIn {
    @location(0) pos: vec3<f32>,
    @location(3) joints: vec4<u32>,
    @location(4) weights: vec4<f32>,
};

@vertex
fn vs_shadow(v: VIn) -> @builtin(position) vec4<f32> {
    let m = bones[v.joints.x] * v.weights.x
          + bones[v.joints.y] * v.weights.y
          + bones[v.joints.z] * v.weights.z
          + bones[v.joints.w] * v.weights.w;
    return sg.light_vp * (m * vec4<f32>(v.pos, 1.0));
}

@vertex
fn vs_shadow_static(v: VIn, @builtin(instance_index) inst: u32) -> @builtin(position) vec4<f32> {
    // (ligne w : teinte de la flore du sol, voir `vs_static`)
    let raw = bones[inst];
    let m = mat4x4<f32>(vec4<f32>(raw[0].xyz, 0.0), vec4<f32>(raw[1].xyz, 0.0), vec4<f32>(raw[2].xyz, 0.0), raw[3]);
    return sg.light_vp * (m * vec4<f32>(v.pos, 1.0));
}
"#;

/// Côté de la carte d'ombre par défaut (texels).
const DEFAULT_SHADOW_SIZE: u32 = 4096;

/// Ciel : dôme équirectangulaire de la carte, dessiné en fond sur tout l'écran.
const SKY_SHADER: &str = r#"
struct SkyU {
    inv_view_proj: mat4x4<f32>,
    view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    // x : luminosité, y : temps (s), z : 1 s'il y a des nuages, w : éclairage des nuages
    params: vec4<f32>,
    // nuages : poids des cumulus R, G, B et du stratus A
    cloud_w: vec4<f32>,
    // couleur des nuages (linéaire), contraste des cumulus
    cloud_color: vec4<f32>,
    // répétitions des cumulus et du stratus, défilement u, v (par seconde)
    cloud_tiling: vec4<f32>,
};
@group(0) @binding(0) var<uniform> u: SkyU;
@group(0) @binding(1) var t_sky: texture_2d<f32>;
@group(0) @binding(2) var s_sky: sampler;
@group(0) @binding(3) var t_clouds: texture_2d<f32>;
@group(0) @binding(4) var s_repeat: sampler;

// astre : direction (xyz) et largeur (w) ; case de texture ; couleur (rgb) ; rotation
// (radians, x), part du ciel qu'il cache (y), disque du soleil (z, 1) et son contraste (w)
struct SatU {
    dir: vec4<f32>,
    uv: vec4<f32>,
    color: vec4<f32>,
    roll: vec4<f32>,
};
@group(1) @binding(0) var<uniform> sat: SatU;
@group(1) @binding(1) var t_sat: texture_2d<f32>;

struct SOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

// un triangle qui couvre l'écran
@vertex
fn vs_sky(@builtin(vertex_index) i: u32) -> SOut {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u)) * 2.0 - 1.0;
    var o: SOut;
    o.clip = vec4<f32>(p, 0.5, 1.0);
    o.ndc = p;
    return o;
}

// B-spline cubique en 4 lectures bilinéaires (au niveau de mip `lod`) : sans arêtes
// comme `horizon_lift` du décor
fn horizon_lift(y: f32) -> f32 {
    let l = select(1.0, g.fog2.w, g.fog2.w > 0.0);
    return mix(l, 1.0, smoothstep(0.0, 0.6, max(y, 0.0)));
}

fn cloud_bicubic(uv: vec2<f32>, lod: f32) -> vec4<f32> {
    let l = i32(min(lod, f32(textureNumLevels(t_clouds) - 1u)));
    let size = vec2<f32>(textureDimensions(t_clouds, l));
    let st = uv * size - 0.5;
    let i = floor(st);
    let f = st - i;
    let f2 = f * f;
    let f3 = f2 * f;
    let w0 = (1.0 - 3.0 * f + 3.0 * f2 - f3) / 6.0;
    let w1 = (4.0 - 6.0 * f2 + 3.0 * f3) / 6.0;
    let w2 = (1.0 + 3.0 * f + 3.0 * f2 - 3.0 * f3) / 6.0;
    let w3 = f3 / 6.0;
    let g0 = w0 + w1;
    let g1 = w2 + w3;
    let h0 = (i - 0.5 + w1 / g0) / size;
    let h1 = (i + 1.5 + w3 / g1) / size;
    let lf = f32(l);
    let a = textureSampleLevel(t_clouds, s_repeat, vec2<f32>(h0.x, h0.y), lf);
    let b = textureSampleLevel(t_clouds, s_repeat, vec2<f32>(h1.x, h0.y), lf);
    let c = textureSampleLevel(t_clouds, s_repeat, vec2<f32>(h0.x, h1.y), lf);
    let d = textureSampleLevel(t_clouds, s_repeat, vec2<f32>(h1.x, h1.y), lf);
    return g0.y * (g0.x * a + g1.x * b) + g1.y * (g0.x * c + g1.x * d);
}

// même repère que le panorama d'ambiance : u = 0,5 vers +Z, v = 0 au zénith
@fragment
fn fs_sky(i: SOut) -> @location(0) vec4<f32> {
    let w = u.inv_view_proj * vec4<f32>(i.ndc, 0.5, 1.0);
    let d = normalize(w.xyz / w.w - u.eye.xyz);
    let uv = vec2<f32>(atan2(d.x, d.z) / 6.2831853 + 0.5, acos(clamp(d.y, -1.0, 1.0)) / 3.1415927);
    // niveau 0 imposé : pas de dérivées faussées par le raccord de atan2
    // luminosité du dôme à l'heure (`EvComp` du jeu), comme `dome_exposure` du décor
    let dome_ev = select(1.0, g.haze.w, g.haze.w > 0.0);
    var c = textureSampleLevel(t_sky, s_sky, uv, 0.0).rgb * dome_ev * horizon_lift(d.y);
    // nuages : la texture de densité projetée sur un plafond plat (plus serrée vers l'horizon),
    // qui défile avec le temps de la timeline
    let p = d.xz / (max(d.y, 0.0) + 0.12) * 0.12;
    let t = u.params.y;
    let uc = p * u.cloud_tiling.x + u.cloud_tiling.zw * t;
    let us = p * u.cloud_tiling.y + u.cloud_tiling.zw * (t * 0.5);
    // la texture est très agrandie : en bilinéaire, le contraste ferait apparaître la grille de
    // ses pixels (et ses blocs de compression) ; lecture bicubique, un niveau de mip au-dessus
    let sz = vec2<f32>(textureDimensions(t_clouds));
    let gc = max(length(dpdx(uc) * sz), length(dpdy(uc) * sz));
    let gs = max(length(dpdx(us) * sz), length(dpdy(us) * sz));
    let tc = cloud_bicubic(uc, max(floor(log2(max(gc, 1e-6))), 0.0) + 1.0);
    let ts = cloud_bicubic(us, max(floor(log2(max(gs, 1e-6))), 0.0) + 1.0);
    let dens = dot(pow(tc.rgb, vec3<f32>(u.cloud_color.w)), u.cloud_w.rgb) + ts.a * u.cloud_w.a;
    let a = (1.0 - exp(-dens * 8.0)) * smoothstep(0.0, 0.25, d.y) * u.params.z;
    // les nuages prennent la lumière du soleil (ou de la lune) : vifs le jour, sombres la nuit
    c = mix(c, u.cloud_color.rgb * u.params.w, clamp(a, 0.0, 1.0));
    // voile de l'air (`<Haze>` du jeu), à sa pleine force : le bleu du dôme et les nuages
    // pâlissent vers l'horizon dans cette direction, comme le décor lointain (`apply_fog`)
    if (g.fog.x > 0.5 && g.haze.x > 0.0) {
        let fd = normalize(vec3<f32>(d.x, 0.0, d.z) + vec3<f32>(1e-5, 0.0, 0.0));
        let hz = textureSampleLevel(t_sky, s_sky, vec2<f32>(atan2(fd.x, fd.z) / 6.2831853 + 0.5, 0.5), 0.0).rgb * dome_ev * horizon_lift(d.y);
        c = mix(c, hz, 1.0 - exp(-g.haze.x));
    }
    // halo du soleil (divisé par la luminosité, appliquée ensuite à tout le ciel)
    c = c + sun_glow(d) / max(u.params.x, 1e-4);
    // rayons du soleil jusqu'au bout de leur portée, comme sur le décor (sinon les trouées de
    // ciel entre les feuilles restent nettes au milieu de l'air éclairé)
    return vec4<f32>(sun_shafts(c * u.params.x, u.eye.xyz + d * 1.0e5, i.clip.xy), 1.0);
}

// Astres : un carré dans la direction de l'astre, à l'infini (w = 0), toujours derrière la scène.
struct AOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) dir: vec3<f32>,
};
@vertex
fn vs_sat(@builtin(vertex_index) vi: u32) -> AOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0),
    );
    let c = corners[vi];
    let d = normalize(sat.dir.xyz);
    var right = cross(vec3<f32>(0.0, 1.0, 0.0), d);
    if (length(right) < 1e-4) {
        right = vec3<f32>(1.0, 0.0, 0.0);
    }
    right = normalize(right);
    let up = cross(d, right);
    let cs = cos(sat.roll.x);
    let sn = sin(sat.roll.x);
    let ax = right * cs + up * sn;
    let ay = up * cs - right * sn;
    let v = d + (ax * c.x + ay * c.y) * (sat.dir.w * 0.5);
    let clip = u.view_proj * vec4<f32>(v, 0.0);
    var o: AOut;
    // profondeur fixée dans l'intervalle visible (le point est à l'infini)
    o.clip = vec4<f32>(clip.xy, clip.w * 0.9999, clip.w);
    o.uv = sat.uv.xy + vec2<f32>(c.x * 0.5 + 0.5, 0.5 - c.y * 0.5) * sat.uv.zw;
    o.dir = v;
    return o;
}
@fragment
fn fs_sat(i: AOut) -> @location(0) vec4<f32> {
    let tex = textureSample(t_sat, s_repeat, i.uv);
    // sous l'horizon, l'astre s'efface (le terrain lointain ne va pas jusqu'au bout du monde)
    let a = tex.a * smoothstep(-0.01, 0.03, normalize(i.dir).y);
    // disque du soleil et de la lune : sa texture est presque unie (211 à 222 en sRGB) alors que
    // les captures de nuit du jeu montrent des cratères bien marqués (×2 entre les taches et le
    // fond, en linéaire) ; le contraste est accentué autour de la valeur médiane (0,68)
    var t = tex.rgb;
    if (sat.roll.z > 0.5) {
        t = pow(max(t, vec3<f32>(0.0)) / 0.68, vec3<f32>(sat.roll.w)) * 0.68;
    }
    let rgb = t * sat.color.rgb * u.params.x * a;
    // la lumière du ciel est devant l'astre : il s'y ajoute (pâle le jour, jamais plus sombre
    // que le ciel) et ne cache le fond (étoiles, crépuscule) que dans la proportion `roll.y`
    return vec4<f32>(rgb, a * sat.roll.y);
}
"#;

/// Particules d'ambiance (voir `particles.rs`). Ce code suit le début de [`SHADER`] (globales,
/// brouillard, ombres, ambiance) : voir [`particle_shader_source`].
const PARTICLE_SHADER: &str = r#"
struct PartU {
    // demi-côtés de la boîte qui suit la caméra ; w : 1 si elle repose sur le sol
    half: vec4<f32>,
    // x hauteur du sol, y répartition en hauteur, z graine, w plancher des boîtes centrées sur la caméra
    place: vec4<f32>,
    // vitesse (m/s), w écart de vitesse d'une particule à l'autre
    vel: vec4<f32>,
    // balancement : amplitude (m), fréquence (Hz) ; rotation min et max (rad/s)
    motion: vec4<f32>,
    // côté min et max (m), flou de mouvement (s), images dans la bande
    size: vec4<f32>,
    // teinte (linéaire), opacité
    color: vec4<f32>,
    // temps (s), durée de vie (s), part du soleil, bascule
    misc: vec4<f32>,
    // taille de l'image (pixels)
    view: vec4<f32>,
};
@group(1) @binding(0) var<uniform> pu: PartU;
@group(1) @binding(1) var t_part: texture_2d<f32>;
@group(1) @binding(2) var s_part: sampler;

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

fn pcg(x: u32) -> u32 {
    let s = x * 747796405u + 2891336453u;
    let w = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (w >> 22u) ^ w;
}
fn rnd(s: ptr<function, u32>) -> f32 {
    *s = pcg(*s);
    return f32(*s >> 8u) / 16777216.0;
}

struct POut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) wpos: vec3<f32>,
    @location(2) alpha: f32,
};

// Une particule = un carré (6 sommets). Sa position ne dépend que de son numéro et du temps :
// elle naît à un endroit tiré au hasard, vit `life` secondes (fondus d'entrée et de sortie),
// puis renaît ailleurs. La boîte suit la caméra : on ramène la position dans la boîte modulo
// sa taille.
@vertex
fn vs_particle(@builtin(vertex_index) vi: u32, @builtin(instance_index) inst: u32) -> POut {
    var o: POut;
    // par défaut hors champ (triangle dégénéré, éliminé)
    o.clip = vec4<f32>(0.0, 0.0, 2.0, 1.0);
    o.uv = vec2<f32>(0.0);
    o.wpos = vec3<f32>(0.0);
    o.alpha = 0.0;

    let life = max(pu.misc.y, 0.1);
    var s0 = pcg(inst * 2654435761u ^ u32(pu.place.z));
    let tt = max(pu.misc.x, 0.0) / life + rnd(&s0);
    let cycle = u32(floor(tt));
    let age = fract(tt);
    var s = pcg(s0 ^ (cycle * 2246822519u));
    let r = vec3<f32>(rnd(&s), pow(rnd(&s), max(pu.place.y, 0.01)), rnd(&s));
    let spd = 1.0 + (rnd(&s) * 2.0 - 1.0) * pu.vel.w;
    let t = age * life;
    let span = pu.half.xyz * 2.0;
    let vel = pu.vel.xyz * spd;
    var p = r * span + vel * t;
    // balancement (feuilles qui se balancent en tombant, grains qui sautillent)
    let fa = pu.motion.x * (0.5 + rnd(&s));
    let fw = 6.2831853 * pu.motion.y * (0.7 + 0.6 * rnd(&s));
    let ph = rnd(&s) * 6.2831853;
    p += vec3<f32>(sin(fw * t + ph), 0.35 * sin(1.7 * fw * t + 2.0 * ph), cos(0.8 * fw * t + ph)) * fa;

    let cam = g.cam_pos.xyz;
    let ground = pu.half.w > 0.5;
    var center = cam;
    if (ground) {
        center.y = pu.place.x + pu.half.y;
    } else {
        center.y = max(cam.y, pu.place.w + pu.half.y);
    }
    let lo = center - pu.half.xyz;
    var q = lo + (p - lo) - span * floor((p - lo) / span);
    if (ground) {
        q.y = pu.place.x + p.y;
    }

    // fondus : bord de la boîte, naissance et fin de vie, tout près de la caméra
    var a = 1.0 - smoothstep(0.7, 1.0, length(q.xz - cam.xz) / pu.half.x);
    if (!ground) {
        let ey = min(q.y - lo.y, lo.y + span.y - q.y) / span.y;
        a *= smoothstep(0.0, 0.1, ey);
    }
    a *= smoothstep(0.0, 0.12, age) * (1.0 - smoothstep(0.85, 1.0, age));
    let d = length(q - cam);
    a *= smoothstep(0.15, 0.6, d);

    // orientation : face à la caméra, tournée sur elle-même, ou étirée dans le sens du mouvement
    let frame_r = rnd(&s);
    let size_r = rnd(&s);
    let rate = mix(pu.motion.z, pu.motion.w, rnd(&s)) * select(-1.0, 1.0, rnd(&s) > 0.5);
    let ang0 = rnd(&s) * 6.2831853;
    let f = (cam - q) / max(d, 1e-3);
    var right = cross(vec3<f32>(0.0, 1.0, 0.0), f);
    if (length(right) < 1e-3) {
        right = vec3<f32>(1.0, 0.0, 0.0);
    }
    right = normalize(right);
    let up = cross(f, right);
    var ax = right;
    var ay = up;
    var hx = mix(pu.size.x, pu.size.y, size_r) * 0.5;
    var hy = hx;
    if (pu.size.z > 0.0) {
        let vp = vel - f * dot(vel, f);
        let lv = length(vp);
        if (lv > 1e-3) {
            ay = vp / lv;
            ax = normalize(cross(ay, f));
            // longueur : la largeur plus le chemin parcouru pendant le temps de pose ; la même
            // quantité de grain s'étale sur toute la traînée, plus pâle quand elle s'allonge
            hy = hx + 0.5 * lv * pu.size.z;
            a *= max(hx / hy, 0.2);
        }
    } else {
        let ang = ang0 + rate * t;
        let cs = cos(ang);
        let sn = sin(ang);
        ax = right * cs + up * sn;
        ay = up * cs - right * sn;
        if (pu.misc.w > 0.5) {
            // la feuille bascule : vue tantôt de face, tantôt par la tranche
            hx = hx * (0.15 + 0.85 * abs(cos(rate * 0.6 * t + ph)));
        }
    }

    // une particule plus petite qu'un pixel scintillerait : on la grossit en baissant son
    // opacité d'autant (même quantité de lumière à l'écran)
    let c0 = g.view_proj * vec4<f32>(q, 1.0);
    if (c0.w <= 0.05) { return o; }
    let c1 = g.view_proj * vec4<f32>(q + ax * hx, 1.0);
    let px = length((c1.xy / max(c1.w, 1e-3) - c0.xy / c0.w) * pu.view.xy * 0.5);
    if (px < 0.7) {
        let k = 0.7 / max(px, 1e-3);
        hx *= k;
        hy *= k;
        a /= k * k;
    }
    if (a <= 0.002) { return o; }

    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0),
    );
    let c = corners[vi % 6u];
    let wp = q + ax * (c.x * hx) + ay * (c.y * hy);
    o.clip = g.view_proj * vec4<f32>(wp, 1.0);
    let frames = max(pu.size.w, 1.0);
    let frame = min(floor(frame_r * frames), frames - 1.0);
    o.uv = vec2<f32>((frame + c.x * 0.5 + 0.5) / frames, 0.5 - c.y * 0.5);
    o.wpos = q;
    o.alpha = min(a, 1.0) * pu.color.a;
    return o;
}

// Éclairage sans normale : ambiance (vers le haut et vers la caméra) et part du soleil, avec les
// ombres du décor et des personnages ; brouillard de la carte. Sortie prémultipliée.
@fragment
fn fs_particle(i: POut) -> @location(0) vec4<f32> {
    let tex = textureSample(t_part, s_part, i.uv);
    let a = clamp(tex.a * i.alpha, 0.0, 1.0);
    let up = vec3<f32>(0.0, 1.0, 0.0);
    let lit = char_shadow_at(i.wpos, up) * decor_shadow_at(i.wpos, up);
    let v = normalize(g.cam_pos.xyz - i.wpos);
    var amb: vec3<f32>;
    if (g.env.x > 0.5) {
        amb = (env_irradiance(env_dir(up)) + env_irradiance(env_dir(v))) * 0.5 * g.env.w;
    } else {
        amb = vec3<f32>(0.32 * g.flags.z);
    }
    var c = tex.rgb * pu.color.rgb * (amb + g.sun.rgb * pu.misc.z * lit) * g.quality.y;
    c = apply_fog(c, i.wpos);
    return vec4<f32>(c * a, a);
}
"#;

/// Source du ciel : la structure des globales et les rayons du soleil de [`SHADER`] (groupe 2 :
/// mêmes liaisons que le décor), puis [`SKY_SHADER`].
fn sky_shader_source() -> String {
    let from = |start: &str, end: &str| -> &'static str {
        let a = SHADER.find(start).expect("SHADER : début introuvable");
        let b = a + SHADER[a..].find(end).expect("SHADER : fin introuvable");
        &SHADER[a..b]
    };
    let globals = from("struct Globals {", "@group(0)");
    let shafts = from("// rayons du soleil (« Fog Shadows »", "// 1 = éclairé par le soleil");
    format!(
        "{globals}@group(2) @binding(0) var<uniform> g: Globals;
         @group(2) @binding(3) var t_shadow: texture_depth_2d_array;
         @group(2) @binding(4) var s_shadow: sampler_comparison;
{shafts}
{SKY_SHADER}"
    )
}

/// Source des particules : les déclarations communes de [`SHADER`] (globales, brouillard, ombres,
/// ambiance, jusqu'aux liaisons des acteurs exclues), puis [`PARTICLE_SHADER`].
fn particle_shader_source() -> String {
    let common = SHADER.split("struct ActorU").next().unwrap_or(SHADER);
    format!("{common}\n{PARTICLE_SHADER}")
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ParticleUniform {
    half: [f32; 4],
    place: [f32; 4],
    vel: [f32; 4],
    motion: [f32; 4],
    size: [f32; 4],
    color: [f32; 4],
    misc: [f32; 4],
    view: [f32; 4],
}

/// Couche de particules prête à dessiner.
struct GpuParticles {
    layer: crate::particles::Layer,
    buf: wgpu::Buffer,
    bind: wgpu::BindGroup,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SkyUniform {
    inv_view_proj: [[f32; 4]; 4],
    view_proj: [[f32; 4]; 4],
    eye: [f32; 4],
    params: [f32; 4],
    cloud_w: [f32; 4],
    cloud_color: [f32; 4],
    cloud_tiling: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SatUniform {
    dir: [f32; 4],
    uv: [f32; 4],
    color: [f32; 4],
    roll: [f32; 4],
}

/// Astre prêt à dessiner (voir [`crate::sky::Satellite`]).
struct GpuSatellite {
    sat: crate::sky::Satellite,
    buf: wgpu::Buffer,
    bind: wgpu::BindGroup,
}

struct ScenePipelines {
    mesh: wgpu::RenderPipeline,
    /// décor instancié (`vs_static`)
    decor: wgpu::RenderPipeline,
    /// champs de force (translucides, sans écriture de profondeur) : acteurs et décor
    ff_mesh: wgpu::RenderPipeline,
    ff_decor: wgpu::RenderPipeline,
    line_depth: wgpu::RenderPipeline,
    line_overlay: wgpu::RenderPipeline,
    ground: wgpu::RenderPipeline,
}
const SHADOW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Globals {
    view_proj: [[f32; 4]; 4],
    light_dir: [f32; 4],
    cam_pos: [f32; 4],
    flags: [f32; 4],
    env: [f32; 4],
    sh: [[f32; 4]; 9],
    light_vp: [[f32; 4]; 4],
    shadow: [f32; 4],
    ground: [f32; 4],
    sun: [f32; 4],
    quality: [f32; 4],
    fog: [f32; 4],
    fog2: [f32; 4],
    fog_color: [f32; 4],
    char_vp: [[f32; 4]; 4],
    char_shadow: [f32; 4],
    quality2: [f32; 4],
    vol: [f32; 4],
    haze: [f32; 4],
    wake_n: [f32; 4],
    wake_head: [[f32; 4]; crate::wake::MAX_CHARS],
    wake_range: [[f32; 4]; crate::wake::MAX_CHARS],
    wake_pt: [[f32; 4]; crate::wake::MAX_POINTS],
    light_n: [f32; 4],
    lights: [[f32; 4]; MAX_LIGHTS * 3],
}

/// Lumières du décor envoyées au shader : les plus proches de la caméra parmi celles qui
/// éclairent quelque chose dans le champ.
pub const MAX_LIGHTS: usize = 128;

/// Les lumières en limite de liste s'éteignent en fondu sur cette distance (m) : sans lui, une
/// lampe qui sortait de la liste quand la caméra avançait s'éteignait d'une image à l'autre (la
/// lumière des arches de la piste clignotait pendant un travelling).
const LIGHT_FADE: f32 = 20.0;

/// Force d'une lampe du décor d'intensité 1 et de couleur blanche (le soleil de midi vaut environ
/// 1,1). L'intensité est le second nombre après la portée dans le `.zone` : 0,12 à 0,25 pour les
/// petits voyants, 1 pour les lampes des arches de la piste des Tech Plants, 2,8 à 4 pour les
/// grands projecteurs. Calée pour que la piste s'éclaire sous les arches la nuit, comme dans le jeu.
const LIGHT_STRENGTH: f32 = 4.0;

/// Lumière placée du décor, dans le repère du studio.
#[derive(Clone, Copy, Debug)]
pub struct DecorLight {
    pub pos: Vec3,
    pub range: f32,
    /// couleur linéaire × intensité
    pub color: Vec3,
    /// projecteur : direction du faisceau et cosinus des demi-angles intérieur et extérieur
    pub spot: Option<(Vec3, f32, f32)>,
}

impl DecorLight {
    /// Lumière de la carte ramenée au repère du studio (`origin` : origine du studio dans le jeu).
    pub fn from_zone(l: &forgelight::zone::Light, origin: Vec3) -> Self {
        let color = Vec3::from(l.color.map(|c| crate::sky::srgb_to_linear(c as f32))) * l.inner_range.clamp(0.05, 8.0);
        let spot = l.spot.then(|| {
            let inner = (l.cone[0].abs() * 0.5).min(1.55);
            let outer = (l.cone[1].abs() * 0.5).clamp(inner + 0.02, 1.57);
            (l.direction(), inner.cos(), outer.cos())
        });
        Self { pos: l.position - origin, range: l.range.max(0.1), color, spot }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ActorUniform {
    tint: [f32; 4],
    flags: [f32; 4],
    camo: [f32; 4],
    decal: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LineVertex {
    pub pos: [f32; 3],
    pub color: [f32; 4],
}

/// Lignes à dessiner pour une image.
#[derive(Default)]
pub struct Lines {
    /// testées en profondeur (grille)
    pub depth: Vec<LineVertex>,
    /// toujours visibles (squelette, gizmos)
    pub overlay: Vec<LineVertex>,
}

impl Lines {
    pub fn line(&mut self, a: Vec3, b: Vec3, color: [f32; 4], overlay: bool) {
        let v = if overlay { &mut self.overlay } else { &mut self.depth };
        v.push(LineVertex { pos: a.to_array(), color });
        v.push(LineVertex { pos: b.to_array(), color });
    }
    pub fn circle(&mut self, center: Vec3, axis: Vec3, radius: f32, color: [f32; 4]) {
        let (u, w) = axis.normalize().any_orthonormal_pair();
        let n = 48;
        for i in 0..n {
            let a0 = i as f32 / n as f32 * std::f32::consts::TAU;
            let a1 = (i + 1) as f32 / n as f32 * std::f32::consts::TAU;
            let p0 = center + (u * a0.cos() + w * a0.sin()) * radius;
            let p1 = center + (u * a1.cos() + w * a1.sin()) * radius;
            self.line(p0, p1, color, true);
        }
    }
}

struct GpuMesh {
    vbuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    index_count: u32,
    /// textures (noms en minuscules) : couleur, relief, spéculaire
    diffuse: Option<String>,
    normal: Option<String>,
    specular: Option<String>,
    /// camouflage par défaut du matériau, et si le matériau en accepte un
    camo_default: Option<String>,
    accepts_camo: bool,
    /// décalcomanie par défaut du matériau (sans `white.dds`), et si le matériau en accepte une
    decal_default: Option<String>,
    accepts_decal: bool,
    /// clé du matériau dans le cache des bind groups (sans le camouflage)
    material: String,
    /// découpé par l'alpha de la couleur (feuillages) ; ne projette pas d'ombre
    alpha_test: bool,
    /// terrain : relief en repère du monde, sol mat
    terrain: bool,
    /// eau : surface sombre et lisse qui reflète le ciel ; ne projette pas d'ombre
    water: bool,
    /// champ de force : translucide et lumineux, dessiné après les surfaces opaques ; ne
    /// projette pas d'ombre
    forcefield: bool,
    /// couleur du champ de force (linéaire)
    ff_color: [f32; 3],
    /// terrain : textures de détail (minuscules) du sol et de la roche, avec le côté d'une
    /// répétition (m)
    detail: Vec<(String, f32)>,
    /// terrain : côtés du quart où le voisin a un autre détail
    detail_edges: u8,
    /// terrain : carte de mélange des écosystèmes, tableaux de leurs détails (couleur, relief)
    /// et écosystèmes des tuiles (couche, côté d'une répétition), voir `forgelight::Mesh`
    blend: Option<String>,
    blend_layers: Option<[String; 2]>,
    blend_ecos: Vec<(i32, f32)>,
    /// détail répété des matériaux du jeu (minuscules) : choix `_DS`, cubemap de relief, couleur
    /// superposée (voir `forgelight::slot`)
    detail_select: Option<String>,
    detail_cube: Option<String>,
    /// répétitions des motifs du détail sur UV1
    detail_repeat: f32,
    overlay: Option<String>,
    /// flore du sol : masque de teinte `_TM` (minuscules)
    tint_mask: Option<String>,
    /// eau des marais : rides, tourbillons, relief des bulles, nappe d'algues (minuscules), et
    /// couleur de l'eau (linéaire)
    water_bump1: Option<String>,
    water_bump2: Option<String>,
    water_foam_n: Option<String>,
    water_overlay: Option<String>,
    water_tint: [f32; 3],
}

/// Décor d'une carte : toutes les matrices d'instance dans un seul tampon, dessinées par lots
/// (un lot = un modèle et une plage d'instances).
struct GpuDecor {
    bind: wgpu::BindGroup,
    buf: wgpu::Buffer,
    _uniform: wgpu::Buffer,
    batches: Vec<DecorBatch>,
    /// boîte englobante des instances (monde)
    bounds: (Vec3, Vec3),
    /// matrices telles que chargées (avant décalage en hauteur)
    mats: Vec<[[f32; 4]; 4]>,
    /// décalage en hauteur appliqué aux matrices du tampon
    lift: f32,
}

struct DecorBatch {
    asset: String,
    first: u32,
    count: u32,
    /// la plage projette une ombre
    shadow: bool,
}

/// Une instance du décor à dessiner.
pub struct DecorInstance {
    pub asset: String,
    pub matrix: Mat4,
    pub cast_shadow: bool,
    /// teinte (linéaire) de la flore du sol, multipliée là où le masque `_TM` du matériau est
    /// blanc ; `None` : aucune. Envoyée au GPU dans la ligne w de la matrice (0, 0, 0 pour une
    /// matrice affine), que le vertex shader remet ensuite à (0, 0, 0, 1).
    pub tint: Option<[f32; 3]>,
}

impl GpuMesh {
    /// Toutes les textures (minuscules) que ce maillage lit.
    fn texture_refs(&self) -> impl Iterator<Item = &str> {
        [
            &self.diffuse,
            &self.normal,
            &self.specular,
            &self.camo_default,
            &self.decal_default,
            &self.blend,
            &self.detail_select,
            &self.detail_cube,
            &self.overlay,
            &self.tint_mask,
            &self.water_bump1,
            &self.water_bump2,
            &self.water_foam_n,
            &self.water_overlay,
        ]
        .into_iter()
        .flatten()
        .map(String::as_str)
        .chain(self.detail.iter().map(|d| d.0.as_str()))
        .chain(self.blend_layers.iter().flatten().map(String::as_str))
    }

    /// Texture de camouflage pour un acteur (minuscules), si ce maillage en reçoit une.
    fn camo_texture(&self, camo: &Camo) -> Option<String> {
        if !self.accepts_camo || !camo.is_active() {
            return None;
        }
        camo.texture_for(self.camo_default.as_deref()).map(|t| t.to_ascii_lowercase())
    }

    /// Texture de décalcomanie pour un acteur (minuscules), si ce maillage en reçoit une.
    fn decal_texture(&self, decal: &Decal) -> Option<String> {
        if !self.accepts_decal || !decal.is_active() {
            return None;
        }
        decal.texture_for(self.decal_default.as_deref()).map(|t| t.to_ascii_lowercase())
    }

    fn material_key(&self, camo: Option<&str>, decal: Option<&str>) -> String {
        format!("{}|{}|{}", self.material, camo.unwrap_or(""), decal.unwrap_or(""))
    }

    /// Clé du matériau et textures de camouflage et de décalcomanie pour un acteur.
    fn resolve(&self, item: &DrawItem) -> (String, Option<String>, Option<String>) {
        let camo = self.camo_texture(&item.camo);
        let decal = self.decal_texture(&item.decal);
        (self.material_key(camo.as_deref(), decal.as_deref()), camo, decal)
    }
}

/// Maillages que le jeu ne dessine jamais : volumes de collision ou d'occlusion laissés dans
/// les modèles du décor (comme les plaques sous les arbres mutants du Biolab), et géométrie
/// d'aide « Designer » (occluders) : sans texture, ou peinte de la couleur de remplissage
/// `medium_slate_blue.dds` comme les `Common_Structures_CourtyardBuilding_A_Occluder*`, qui
/// bouchaient les fenêtres des bâtiments de cour d'un panneau lavande uni.
fn is_hidden(m: &forgelight::Mesh) -> bool {
    matches!(m.material_definition.as_str(), "Invisible" | "OcclusionModel")
        || (m.material_definition == "Designer"
            && m.diffuse.as_deref().is_none_or(|t| t.to_ascii_lowercase().contains("slate_blue")))
}

/// Couleur d'un champ de force (linéaire), d'après le paramètre de couleur de son matériau.
/// Les palettes donnent des teintes sombres et de luminosités inégales (TR 0,5 / 0,04 / 0,04,
/// VS 0,2 / 0,12 / 0,35) : on garde la teinte et on ramène la composante la plus forte à 0,8.
/// Sans couleur, un bleu clair neutre.
fn forcefield_color(c: Option<[f32; 4]>) -> [f32; 3] {
    let Some([r, g, b, _]) = c else { return [0.45, 0.7, 1.0] };
    let m = r.max(g).max(b);
    if m < 1e-3 {
        return [0.45, 0.7, 1.0];
    }
    [r / m * 0.8, g / m * 0.8, b / m * 0.8]
}

/// Matériaux d'eau (ruisseaux, bassins) : leur couleur est un simple `grey.dds`.
fn is_water(material_definition: &str) -> bool {
    material_definition.to_ascii_lowercase().contains("water")
}

/// Texture d'un emplacement de l'eau (minuscules), sans les remplissages.
fn water_slot(m: &forgelight::Mesh, slot: u32) -> Option<String> {
    if !is_water(&m.material_definition) {
        return None;
    }
    m.water_slot(slot).map(|t| t.to_ascii_lowercase())
}

/// Couleur de l'eau (linéaire) d'après le paramètre `Tint` du matériau : olive sombre sur les
/// marais d'Hossin et d'Amerish (0,18 / 0,18 / 0). Sans lui, le vert-bleu sombre d'origine.
fn water_color(tint: Option<[f32; 4]>) -> [f32; 3] {
    match tint {
        Some([r, g, b, _]) if r + g + b > 1e-3 => [r, g, b],
        _ => [0.15, 0.3, 0.3],
    }
}

/// Matériaux découpés par l'alpha de la couleur (feuillages, herbes).
fn is_alpha_tested(material_definition: &str) -> bool {
    let m = material_definition.to_ascii_lowercase();
    // (`FloraModel` : herbes et plantes de la flore du sol)
    ["foliage", "grass", "leaves", "leaf", "alphatest", "cutout", "flora"].iter().any(|k| m.contains(k))
}

/// Exposant appliqué à la texture du disque du soleil et de la lune (voir `fs_sat`) : les taches
/// de `esamir_planets.dds` (0,65 à 0,80 en linéaire) passent de ×1,23 à ×3,5 environ : après
/// l'étalonnage de nuit, l'écart entre taches et fond rejoint celui des captures du jeu.
const MOON_CONTRAST: f32 = 6.0;

/// Les cartes de données (relief, spéculaire) ne sont pas en sRGB.
fn is_linear_texture(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    // (relief, spéculaire, choix et relief du détail : des données, pas des couleurs)
    // (les reliefs de l'eau, `water_ripple.dds` et `water_twirl90.dds`, n'ont pas le suffixe)
    n.ends_with("_n.dds")
        || n.ends_with("_s.dds")
        || n.ends_with("_ds.dds")
        || n.ends_with("_dn.dds")
        || n.starts_with("detail_cube")
        // (terrain : cartes de mélange et détails des écosystèmes, voir `decor::quad_blend`)
        || n.ends_with("_mel.dds")
        || n.starts_with("water_ripple")
        || n.starts_with("water_twirl")
}

pub struct GpuModel {
    meshes: Vec<GpuMesh>,
    /// positions de repos des os (pour cadrer l'ombre : os courant = skin × repos)
    bind_bones: Vec<Vec3>,
    bounds: (Vec3, Vec3),
}

struct GpuActor {
    bone_buf: wgpu::Buffer,
    bone_cap: usize,
    uniform: wgpu::Buffer,
    bind: wgpu::BindGroup,
}

/// Un acteur à dessiner.
pub struct DrawItem {
    pub id: u64,
    pub asset: String,
    pub skin: Vec<Mat4>,
    pub highlight: bool,
    pub camo: Camo,
    pub decal: Decal,
}

pub struct Target {
    pub width: u32,
    pub height: u32,
    samples: u32,
    /// scène HDR multiéchantillonnée (absente sans anticrénelage : on dessine dans `post.hdr_view`)
    msaa: Option<wgpu::TextureView>,
    depth: wgpu::TextureView,
    /// images intermédiaires du post-traitement (HDR résolue, occlusion, bloom)
    post: PostTargets,
    /// image finale sRGB (affichée par egui, relue pour les exports)
    pub resolve: wgpu::Texture,
    pub resolve_view: wgpu::TextureView,
}

impl Target {
    fn new(device: &wgpu::Device, width: u32, height: u32, samples: u32, post: &Post) -> Self {
        let size = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };
        let mk = |format, samples, usage, label| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size,
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        // relue par la résolution du post-traitement (moyenne pondérée des échantillons)
        let msaa = (samples > 1).then(|| {
            mk(HDR_FORMAT, samples, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING, "msaa")
                .create_view(&Default::default())
        });
        // la profondeur est relue par l'occlusion ambiante
        let depth = mk(
            DEPTH_FORMAT,
            samples,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            "depth",
        );
        let depth_view = depth.create_view(&Default::default());
        let post = PostTargets::new(device, post, width, height, &depth_view, msaa.as_ref());
        let resolve = mk(
            COLOR_FORMAT,
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
            "resolve",
        );
        Self {
            width,
            height,
            samples,
            msaa,
            depth: depth_view,
            post,
            resolve_view: resolve.create_view(&Default::default()),
            resolve,
        }
    }
}

pub struct Renderer {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    mesh_pipeline: wgpu::RenderPipeline,
    line_depth_pipeline: wgpu::RenderPipeline,
    line_overlay_pipeline: wgpu::RenderPipeline,
    globals_buf: wgpu::Buffer,
    globals_bind: wgpu::BindGroup,
    actor_layout: wgpu::BindGroupLayout,
    tex_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// même réglage, sans boucler : couleur et relief des quarts de terrain
    clamp_sampler: wgpu::Sampler,
    /// textures par défaut : couleur grise, relief plat, spéculaire neutre
    default_diffuse: wgpu::TextureView,
    default_normal: wgpu::TextureView,
    default_spec: wgpu::TextureView,
    default_camo: wgpu::TextureView,
    default_decal: wgpu::TextureView,
    /// choix du détail par défaut (sans effet : A = 0) et cubemap de détail par défaut (une
    /// couche plate)
    default_detail_select: wgpu::TextureView,
    default_detail_cube: wgpu::TextureView,
    textures: HashMap<String, wgpu::TextureView>,
    /// textures à plusieurs couches (cubemaps de détail), vues comme tableaux de textures
    layered: std::collections::HashSet<String>,
    /// bind groups des matériaux (couleur + relief + spéculaire), vidé à chaque nouvelle texture
    materials: HashMap<String, wgpu::BindGroup>,
    models: HashMap<String, GpuModel>,
    actors: HashMap<u64, GpuActor>,
    line_buf: wgpu::Buffer,
    line_cap: usize,
    pub viewport: Option<Target>,
    pub egui_tex: Option<eframe::egui::TextureId>,
    /// moniteur de la caméra de prise (incrusté dans la vue de travail)
    pub monitor: Option<Target>,
    pub monitor_tex: Option<eframe::egui::TextureId>,
    /// aperçu d'un asset survolé dans la liste (bulle au curseur)
    preview: Option<Target>,
    pub preview_tex: Option<eframe::egui::TextureId>,
    pub clear_color: [f64; 3],
    /// applique les cartes de relief (_N)
    pub normal_maps: bool,
    /// intensité des zones émissives (canal B de _S) ; 0 = éteintes
    pub emissive: f32,
    /// direction de la lumière principale (vers laquelle elle éclaire)
    pub light_dir: Vec3,
    /// couleur × intensité du soleil (linéaire)
    pub sun_color: Vec3,
    globals_layout: wgpu::BindGroupLayout,
    env_sampler: wgpu::Sampler,
    /// ambiance active : éclairement SH, facteur de niveau, nombre de mips
    env: Option<GpuEnv>,
    /// rotation du panorama d'ambiance (degrés)
    pub env_rotation_deg: f32,
    /// multiplicateur de l'ambiance
    pub env_intensity: f32,
    /// ombres portées du soleil
    pub shadows: bool,
    /// opacité de l'ombre sur le sol (0 = pas d'ombre au sol)
    pub ground_shadow: f32,
    /// le décor fait de l'ombre aux personnages (sinon, ils gardent le soleil sous les arbres)
    pub decor_shades_actors: bool,
    shadow_pipeline: wgpu::RenderPipeline,
    shadow_static_pipeline: wgpu::RenderPipeline,
    decor_pipeline: wgpu::RenderPipeline,
    /// champs de force translucides (acteurs, décor)
    ff_mesh_pipeline: wgpu::RenderPipeline,
    ff_decor_pipeline: wgpu::RenderPipeline,
    /// décor chargé d'une carte (instances regroupées par modèle)
    decor: Option<GpuDecor>,
    /// affiche le décor
    pub decor_visible: bool,
    /// lumières placées du décor (repère du studio, avant le relèvement du sol)
    decor_lights: Vec<DecorLight>,
    /// les lampes du décor éclairent la scène, et leur force (1 = réglage d'origine)
    pub decor_lights_on: bool,
    pub decor_lights_strength: f32,
    /// demi-côté (m) de la zone du décor couverte par la carte d'ombre, autour des personnages
    pub decor_shadow_range: f32,
    /// le décor a son terrain : le sol invisible qui reçoit l'ombre n'est plus dessiné
    pub decor_terrain: bool,
    ground_pipeline: wgpu::RenderPipeline,
    /// cartes d'ombre lues par les maillages (tableau de 2 couches)
    shadow_view: wgpu::TextureView,
    /// mêmes couches, une vue chacune pour y dessiner : personnages (0), décor (1)
    shadow_layers: [wgpu::TextureView; 2],
    shadow_sampler: wgpu::Sampler,
    /// côté des cartes d'ombre (texels)
    shadow_size: u32,
    /// rayon du filtrage des ombres en texels (0 = bords nets, 1 = 3×3, 2 = 5×5)
    pub shadow_filter: u32,
    /// multiplicateur final de la luminosité
    pub brightness: f32,
    /// anticrénelage : échantillons par pixel (1, 2, 4 ou 8)
    samples: u32,
    /// nombres d'échantillons que la carte graphique accepte (1 et 4 toujours)
    sample_counts: Vec<u32>,
    /// anticrénelage des reflets (rugosité élargie là où la normale varie vite)
    pub specular_aa: bool,
    /// feuillages de qualité haute : alpha renforcé au loin, bords lissés avec le MSAA
    pub foliage_aa: bool,
    /// distance (m) jusqu'où le détail des écosystèmes du terrain est visible
    pub terrain_detail: f32,
    /// eau de qualité haute : houle animée, reflet du ciel de la carte
    pub water_quality: bool,
    /// multiplicateur du nombre de particules d'ambiance
    pub particle_quality: f32,
    /// filtrage bicubique des textures agrandies (gros plans)
    pub texture_hq: bool,
    /// netteté des textures (0 à 1) : biais de mip négatif quand l'image est suréchantillonnée
    pub texture_sharpness: f32,
    /// suréchantillonnage de l'image en cours (export, ou résolution de la vue) : décide du biais
    pub render_ss: f32,
    /// cible réutilisée par les exports (`render_image`)
    export_target: Option<Target>,
    post: Post,
    /// occlusion ambiante et bloom
    pub post_settings: PostSettings,
    /// niveaux de mip ignorés sur les textures (0 = pleine résolution)
    texture_lod: f32,
    /// panorama d'ambiance courant (pour reconstruire les liaisons globales)
    env_view: wgpu::TextureView,
    shader: wgpu::ShaderModule,
    mesh_layout: wgpu::PipelineLayout,
    line_layout: wgpu::PipelineLayout,
    shadow_bufs: [wgpu::Buffer; 2],
    shadow_binds: [wgpu::BindGroup; 2],
    sky_shader: wgpu::ShaderModule,
    sky_layout: wgpu::PipelineLayout,
    sky_bind_layout: wgpu::BindGroupLayout,
    sky_pipeline: wgpu::RenderPipeline,
    /// groupe vide à l'index 1 du ciel (celui des astres, que le ciel n'utilise pas)
    sky_empty_bind: wgpu::BindGroup,
    sky_buf: wgpu::Buffer,
    sky_sampler: wgpu::Sampler,
    /// dôme du ciel de la carte (dessiné avec le décor)
    sky: Option<wgpu::BindGroup>,
    /// même dôme, lu par les maillages pour le brouillard
    sky_view: wgpu::TextureView,
    /// brouillard de la carte à l'heure du ciel
    fog: Option<crate::sky::Fog>,
    /// brouillard affiché, et multiplicateur de sa densité
    pub fog_enabled: bool,
    pub fog_density: f32,
    /// multiplicateur de la force du voile de l'air (1 = celle du jeu, 0 = aucun)
    pub haze_scale: f32,
    /// force du halo autour du soleil (0 = aucun)
    pub sun_glow: f32,
    /// luminosité du dôme du ciel à son heure (`EvComp` du jeu)
    dome_exposure: f32,
    /// assombrissement de l'air la nuit (`sky::Sky::night_air`), appliqué aux rayons du soleil
    night_air: f32,
    /// dôme près de l'horizon relativement au zénith (`sky::Sky::horizon_lift`)
    horizon_lift: f32,
    /// force des rayons du soleil (0 = coupés)
    pub sun_rays: f32,
    /// hauteur du jeu ramenée à y = 0 (le plancher du brouillard est en mètres du jeu)
    decor_ground: f32,
    /// hauteur du jeu ramenée à y = 0 dans les matrices du décor chargé
    decor_base_ground: f32,
    /// distance où s'arrête le terrain chargé (le décor doit s'être fondu dans le ciel avant)
    decor_reach: f32,
    /// particules d'ambiance : pipeline, liaisons, textures (gardées à part : le retrait du décor
    /// n'y touche pas)
    particle_shader: wgpu::ShaderModule,
    particle_layout: wgpu::PipelineLayout,
    particle_bind_layout: wgpu::BindGroupLayout,
    particle_pipeline: wgpu::RenderPipeline,
    particle_sampler: wgpu::Sampler,
    particle_textures: HashMap<String, wgpu::TextureView>,
    /// couches de l'effet `particle_kind`
    particles: Vec<GpuParticles>,
    particle_kind: Option<crate::particles::Kind>,
    particle_settings: crate::particles::Particles,
    /// temps de la timeline (s) : position des particules, défilement des nuages
    pub particle_time: f32,
    /// sillages des personnages dans l'eau du décor, à l'image courante (voir `wake.rs`)
    pub wakes: crate::wake::Wakes,
    /// astres du ciel de la carte (soleil, planètes, étoiles), du plus lointain au plus proche
    sat_bind_layout: wgpu::BindGroupLayout,
    sat_layout: wgpu::PipelineLayout,
    sat_pipeline: wgpu::RenderPipeline,
    sky_repeat_sampler: wgpu::Sampler,
    satellites: Vec<GpuSatellite>,
    /// couche de nuages du ciel et sa texture de densité
    clouds: Option<crate::sky::Clouds>,
    clouds_view: Option<wgpu::TextureView>,
}

struct GpuEnv {
    sh: [[f32; 4]; 9],
    scale: f32,
    mip_count: u32,
}

impl Renderer {
    pub fn new(rs: &egui_wgpu::RenderState) -> Self {
        let mut r = Self::from_device(rs.device.clone(), rs.queue.clone());
        r.detect_sample_counts(&rs.adapter);
        r
    }

    /// Nombres d'échantillons MSAA possibles : 1 et 4 sont garantis ; 2 et 8 dépendent de la
    /// carte (il faut alors la fonction `TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES` de l'appareil).
    pub fn detect_sample_counts(&mut self, adapter: &wgpu::Adapter) {
        let mut counts = vec![1, 4];
        if self.device.features().contains(wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES) {
            let hdr = adapter.get_texture_format_features(HDR_FORMAT).flags;
            let depth = adapter.get_texture_format_features(DEPTH_FORMAT).flags;
            for n in [2, 8] {
                if hdr.sample_count_supported(n) && depth.sample_count_supported(n) {
                    counts.push(n);
                }
            }
        }
        counts.sort_unstable();
        self.sample_counts = counts;
    }

    /// Nombres d'échantillons MSAA proposés.
    pub fn sample_counts(&self) -> &[u32] {
        &self.sample_counts
    }

    pub fn from_device(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("studio"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("globals"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // panorama d'ambiance
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // cartes d'ombre (personnages, décor)
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                    count: None,
                },
                // dôme du ciel (brouillard)
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let actor_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("actor"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let tex_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let tex_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("material"),
            entries: &[
                tex_entry(0),
                tex_entry(1),
                tex_entry(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // motif de camouflage
                tex_entry(4),
                // décalcomanie
                tex_entry(6),
                // détail répété : choix `_DS`, cubemap des motifs (tableau de six couches),
                // couleur superposée
                tex_entry(7),
                wgpu::BindGroupLayoutEntry {
                    binding: 8,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                tex_entry(9),
                // terrain mélangé : relief des détails d'écosystème (tableau)
                wgpu::BindGroupLayoutEntry {
                    binding: 11,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 10,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // paramètres du matériau
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let mesh_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mesh"),
            bind_group_layouts: &[&globals_layout, &actor_layout, &tex_layout],
            push_constant_ranges: &[],
        });
        let line_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lines"),
            bind_group_layouts: &[&globals_layout],
            push_constant_ranges: &[],
        });

        let ScenePipelines {
            mesh: mesh_pipeline,
            decor: decor_pipeline,
            ff_mesh: ff_mesh_pipeline,
            ff_decor: ff_decor_pipeline,
            line_depth: line_depth_pipeline,
            line_overlay: line_overlay_pipeline,
            ground: ground_pipeline,
        } = Self::scene_pipelines(&device, &shader, &mesh_layout, &line_layout, DEFAULT_SAMPLES);

        // passe d'ombre : profondeur seule, vue du soleil
        let shadow_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shadow"),
            source: wgpu::ShaderSource::Wgsl(SHADOW_SHADER.into()),
        });
        let shadow_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("shadow"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let shadow_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shadow"),
            bind_group_layouts: &[&shadow_layout, &actor_layout],
            push_constant_ranges: &[],
        });
        // mêmes sommets que le rendu, seuls position, os et poids sont lus
        let shadow_attrs = [
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x3, offset: 0, shader_location: 0 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Uint32x4, offset: 32, shader_location: 3 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 48, shader_location: 4 },
        ];
        let mk_shadow = |entry: &str| device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(entry),
            layout: Some(&shadow_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shadow_shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<forgelight::Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &shadow_attrs,
                }],
            },
            fragment: None,
            primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: SHADOW_FORMAT,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: Default::default(),
                // évite l'« acné » d'ombre sur les surfaces éclairées
                bias: wgpu::DepthBiasState { constant: 2, slope_scale: 2.0, clamp: 0.0 },
            }),
            multisample: Default::default(),
            multiview: None,
            cache: None,
        });
        let shadow_pipeline = mk_shadow("vs_shadow");
        let shadow_static_pipeline = mk_shadow("vs_shadow_static");
        let (shadow_view, shadow_layers) = Self::make_shadow_maps(&device, DEFAULT_SHADOW_SIZE);
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("shadow"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        // une projection par carte : personnages (0), décor (1)
        let shadow_bufs: [wgpu::Buffer; 2] = std::array::from_fn(|_| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("shadow"),
                size: 64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });
        let shadow_binds: [wgpu::BindGroup; 2] = std::array::from_fn(|i| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("shadow"),
                layout: &shadow_layout,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: shadow_bufs[i].as_entire_binding() }],
            })
        });

        let globals_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // le panorama boucle horizontalement et s'arrête aux pôles
        let env_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("env"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let env_view = Self::make_texture(&device, &queue, 1, 1, &[vec![0, 0, 0, 255]], COLOR_FORMAT);
        let sky_view = Self::make_texture(&device, &queue, 1, 1, &[vec![0, 0, 0, 255]], COLOR_FORMAT);
        let globals_bind = Self::make_globals_bind(
            &device,
            &globals_layout,
            &globals_buf,
            &env_view,
            &env_sampler,
            &shadow_view,
            &shadow_sampler,
            &sky_view,
        );
        let sampler = Self::material_sampler(&device, 0.0);
        let clamp_sampler = Self::material_sampler_with(&device, 0.0, wgpu::AddressMode::ClampToEdge);
        let line_cap = 1 << 16;
        let line_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lines"),
            size: (line_cap * std::mem::size_of::<LineVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // ciel des cartes
        let sky_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sky"),
            source: wgpu::ShaderSource::Wgsl(sky_shader_source().into()),
        });
        let sky_bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sky"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                tex_entry(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // nuages (texture de densité) et échantillonneur qui répète
                tex_entry(3),
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        // astres : paramètres et texture de chacun
        let sat_bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("astres"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                tex_entry(1),
            ],
        });
        let sat_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("astres"),
            bind_group_layouts: &[&sky_bind_layout, &sat_bind_layout],
            push_constant_ranges: &[],
        });
        let sat_pipeline = Self::sat_pipeline(&device, &sky_shader, &sat_layout, DEFAULT_SAMPLES);
        // ciel : groupe 2 = globales du décor (rayons du soleil) ; le 1 (celui des astres) ne
        // sert pas, mais wgpu veut un groupe lié à chaque index : un groupe vide
        let empty_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("vide"), entries: &[] });
        let sky_empty_bind =
            device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("vide"), layout: &empty_layout, entries: &[] });
        let sky_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sky"),
            bind_group_layouts: &[&sky_bind_layout, &empty_layout, &globals_layout],
            push_constant_ranges: &[],
        });
        let sky_pipeline = Self::sky_pipeline(&device, &sky_shader, &sky_layout, DEFAULT_SAMPLES);
        let sky_repeat_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("nuages"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let sky_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sky"),
            size: std::mem::size_of::<SkyUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // le dôme boucle autour de l'horizon et s'arrête aux pôles
        let sky_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("sky"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // particules d'ambiance
        let particle_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("particules"),
            source: wgpu::ShaderSource::Wgsl(particle_shader_source().into()),
        });
        let particle_bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("particules"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                tex_entry(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let particle_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("particules"),
            bind_group_layouts: &[&globals_layout, &particle_bind_layout],
            push_constant_ranges: &[],
        });
        let particle_pipeline = Self::particle_pipeline(&device, &particle_shader, &particle_layout, DEFAULT_SAMPLES);
        // les bandes d'images ne bouclent pas : pas de bavure d'une image sur l'autre au bord
        let particle_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("particules"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let post = Post::new(&device, DEFAULT_SAMPLES, COLOR_FORMAT);
        post.set_luts(&queue, None);
        let px = |rgba: [u8; 4], format| Self::make_texture(&device, &queue, 1, 1, &[rgba.to_vec()], format);
        let mut r = Self {
            default_diffuse: px([200, 200, 200, 255], COLOR_FORMAT),
            // relief plat (X = A, Y = G à 0,5)
            default_normal: px([128, 128, 255, 128], DATA_FORMAT),
            // diélectrique moyennement rugueux, sans émissif
            default_spec: px([0, 140, 0, 255], DATA_FORMAT),
            // gris moyen : sans effet, le camouflage étant coupé par le matériau
            default_camo: px([128, 128, 128, 255], COLOR_FORMAT),
            // transparente
            default_decal: px([0, 0, 0, 0], COLOR_FORMAT),
            default_detail_select: px([128, 128, 255, 0], DATA_FORMAT),
            default_detail_cube: Self::make_texture_layers(&device, &queue, 1, 1, 1, true, &[vec![0, 128, 0, 128]], DATA_FORMAT),
            device,
            queue,
            mesh_pipeline,
            line_depth_pipeline,
            line_overlay_pipeline,
            globals_buf,
            globals_bind,
            actor_layout,
            tex_layout,
            sampler,
            clamp_sampler,
            textures: HashMap::new(),
            layered: Default::default(),
            materials: HashMap::new(),
            models: HashMap::new(),
            actors: HashMap::new(),
            line_buf,
            line_cap,
            viewport: None,
            egui_tex: None,
            monitor: None,
            monitor_tex: None,
            preview: None,
            preview_tex: None,
            clear_color: [0.11, 0.12, 0.14],
            normal_maps: true,
            emissive: 1.0,
            light_dir: Vec3::new(-0.4, -0.8, -0.45),
            sun_color: Vec3::splat(0.95),
            globals_layout,
            env_sampler,
            env: None,
            env_rotation_deg: 0.0,
            env_intensity: 1.0,
            shadows: true,
            ground_shadow: 0.6,
            decor_shades_actors: true,
            shadow_pipeline,
            shadow_static_pipeline,
            decor_pipeline,
            ff_mesh_pipeline,
            ff_decor_pipeline,
            decor: None,
            decor_visible: true,
            decor_lights: Vec::new(),
            decor_lights_on: true,
            decor_lights_strength: 1.0,
            decor_shadow_range: 60.0,
            decor_terrain: false,
            ground_pipeline,
            shadow_view,
            shadow_layers,
            shadow_sampler,
            shadow_size: DEFAULT_SHADOW_SIZE,
            shadow_filter: 1,
            brightness: 1.0,
            samples: DEFAULT_SAMPLES,
            sample_counts: vec![1, 4],
            specular_aa: true,
            foliage_aa: true,
            terrain_detail: 110.0,
            water_quality: true,
            particle_quality: 1.0,
            texture_hq: true,
            texture_sharpness: 0.5,
            render_ss: 1.0,
            export_target: None,
            post,
            post_settings: PostSettings::default(),
            texture_lod: 0.0,
            env_view,
            shader,
            mesh_layout,
            line_layout,
            shadow_bufs,
            shadow_binds,
            sky_shader,
            sky_layout,
            sky_bind_layout,
            sky_pipeline,
            sky_empty_bind,
            sky_buf,
            sky_sampler,
            sky: None,
            sky_view,
            fog: None,
            fog_enabled: true,
            fog_density: 1.0,
            haze_scale: 1.0,
            sun_glow: 1.0,
            dome_exposure: 1.0,
            night_air: 1.0,
            horizon_lift: 1.0,
            sun_rays: 1.0,
            decor_ground: 0.0,
            decor_base_ground: 0.0,
            decor_reach: f32::MAX,
            particle_shader,
            particle_layout,
            particle_bind_layout,
            particle_pipeline,
            particle_sampler,
            particle_textures: HashMap::new(),
            particles: Vec::new(),
            particle_kind: None,
            particle_settings: Default::default(),
            particle_time: 0.0,
            wakes: Default::default(),
            sat_bind_layout,
            sat_layout,
            sat_pipeline,
            sky_repeat_sampler,
            satellites: Vec::new(),
            clouds: None,
            clouds_view: None,
        };
        r.textures.clear();
        r
    }

    /// Pipelines de la scène pour un niveau d'anticrénelage (maillages, lignes, sol).
    fn scene_pipelines(
        device: &wgpu::Device,
        shader: &wgpu::ShaderModule,
        mesh_layout: &wgpu::PipelineLayout,
        line_layout: &wgpu::PipelineLayout,
        samples: u32,
    ) -> ScenePipelines {
        let mesh_attrs = wgpu::vertex_attr_array![
            0 => Float32x3, 1 => Float32x3, 2 => Float32x2, 3 => Uint32x4, 4 => Float32x4, 5 => Float32x4,
            6 => Float32x2, 7 => Float32x2
        ];
        // `translucent` : champs de force, mélangés (couleur prémultipliée) sans écrire la profondeur
        let mk_mesh = |entry: &str, fs: &str, translucent: bool| device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(entry),
            layout: Some(mesh_layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<forgelight::Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &mesh_attrs,
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: Some(fs),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: HDR_FORMAT,
                    blend: Some(if translucent {
                        wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING
                    } else {
                        wgpu::BlendState::REPLACE
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: !translucent,
                depth_compare: wgpu::CompareFunction::Greater,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            // opaques : « alpha to coverage » (bords des feuillages lissés par le MSAA ; les autres
            // surfaces sortent un alpha de 1, donc couvrent tout le pixel)
            multisample: wgpu::MultisampleState {
                count: samples,
                mask: !0,
                alpha_to_coverage_enabled: samples > 1 && !translucent,
            },
            multiview: None,
            cache: None,
        });
        let mesh_pipeline = mk_mesh("vs_main", "fs_main", false);
        let decor_pipeline = mk_mesh("vs_static", "fs_main", false);
        let ff_mesh = mk_mesh("vs_main", "fs_forcefield", true);
        let ff_decor = mk_mesh("vs_static", "fs_forcefield", true);

        let line_attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4];
        let mk_line = |depth: bool| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("lines"),
                layout: Some(line_layout),
                vertex: wgpu::VertexState {
                    module: shader,
                    entry_point: Some("vs_line"),
                    compilation_options: Default::default(),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<LineVertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &line_attrs,
                    }],
                },
                fragment: Some(wgpu::FragmentState {
                    module: shader,
                    entry_point: Some("fs_line"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: HDR_FORMAT,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::LineList, ..Default::default() },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: false,
                    depth_compare: if depth { wgpu::CompareFunction::GreaterEqual } else { wgpu::CompareFunction::Always },
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: wgpu::MultisampleState { count: samples, ..Default::default() },
                multiview: None,
                cache: None,
            })
        };
        let line_depth_pipeline = mk_line(true);
        let line_overlay_pipeline = mk_line(false);

        // sol attrape-ombre : transparent, testé en profondeur sans l'écrire
        let ground_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ground"),
            layout: Some(line_layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: Some("vs_ground"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: Some("fs_ground"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: HDR_FORMAT,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::GreaterEqual,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState { count: samples, ..Default::default() },
            multiview: None,
            cache: None,
        });

        ScenePipelines { mesh: mesh_pipeline, decor: decor_pipeline, ff_mesh, ff_decor, line_depth: line_depth_pipeline, line_overlay: line_overlay_pipeline, ground: ground_pipeline }
    }

    /// Fond de ciel : sans test ni écriture de profondeur, dessiné avant la scène.
    fn sky_pipeline(
        device: &wgpu::Device,
        shader: &wgpu::ShaderModule,
        layout: &wgpu::PipelineLayout,
        samples: u32,
    ) -> wgpu::RenderPipeline {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sky"),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: Some("vs_sky"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: Some("fs_sky"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: HDR_FORMAT,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Always,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState { count: samples, ..Default::default() },
            multiview: None,
            cache: None,
        })
    }

    /// Particules : carrés mélangés (couleur prémultipliée), testés en profondeur sans l'écrire.
    fn particle_pipeline(
        device: &wgpu::Device,
        shader: &wgpu::ShaderModule,
        layout: &wgpu::PipelineLayout,
        samples: u32,
    ) -> wgpu::RenderPipeline {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("particules"),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: Some("vs_particle"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: Some("fs_particle"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: HDR_FORMAT,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Greater,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState { count: samples, ..Default::default() },
            multiview: None,
            cache: None,
        })
    }

    pub fn has_particle_texture(&self, name: &str) -> bool {
        self.particle_textures.contains_key(&name.to_ascii_lowercase())
    }

    /// Texture d'un effet de particules (voir [`crate::particles::prepare`]).
    pub fn upload_particle_texture(&mut self, name: &str, img: &dds::Image) {
        self.upload_texture(name, img);
        let key = name.to_ascii_lowercase();
        if let Some(v) = self.textures.get(&key) {
            self.particle_textures.insert(key, v.clone());
        }
        // les couches seront reconstruites avec cette texture
        self.particle_kind = None;
        self.particles.clear();
    }

    /// Effet de particules à dessiner (`None` : aucun) et ses réglages. Les textures doivent
    /// avoir été chargées ; une couche dont la texture manque n'est pas dessinée.
    pub fn set_particles(&mut self, kind: Option<crate::particles::Kind>, settings: &crate::particles::Particles) {
        self.particle_settings = settings.clone();
        if kind == self.particle_kind && !(kind.is_some() && self.particles.is_empty()) {
            return;
        }
        self.particle_kind = kind;
        self.particles = kind
            .map(|k| k.layers())
            .unwrap_or_default()
            .iter()
            .filter_map(|layer| {
                let view = self.particle_textures.get(&layer.texture.to_ascii_lowercase())?;
                let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("particules"),
                    size: std::mem::size_of::<ParticleUniform>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("particules"),
                    layout: &self.particle_bind_layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(view) },
                        wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.particle_sampler) },
                    ],
                });
                Some(GpuParticles { layer: *layer, buf, bind })
            })
            .collect();
    }

    /// Paramètres d'une couche pour cette image ; nombre de particules à dessiner.
    fn particle_uniform(&self, i: usize, layer: &crate::particles::Layer, width: u32, height: u32) -> (ParticleUniform, u32) {
        let s = &self.particle_settings;
        let [wx, wz] = s.wind_vector();
        let size = s.size.clamp(0.1, 10.0);
        let count = (layer.count as f32 * s.density.clamp(0.0, 10.0) * self.particle_quality.clamp(0.0, 4.0)).round() as u32;
        let terrain = self.decor_terrain && self.decor_active();
        let u = ParticleUniform {
            half: [layer.half[0], layer.half[1], layer.half[2], if layer.ground { 1.0 } else { 0.0 }],
            // sans terrain, le sol est la grille (y = 0) : rien ne tombe dessous ; avec le terrain,
            // le relief cache ce qui passe sous le sol
            place: [0.0, layer.height_bias, (i as f32 + 1.0) * 7919.0, if terrain { -1.0e6 } else { 0.0 }],
            vel: [wx * layer.wind, -layer.fall, wz * layer.wind, layer.speed_var],
            motion: [layer.flutter[0], layer.flutter[1], layer.spin[0].to_radians(), layer.spin[1].to_radians()],
            size: [layer.size[0] * size, layer.size[1] * size, layer.stretch, layer.frames.max(1) as f32],
            color: [layer.color[0], layer.color[1], layer.color[2], layer.alpha],
            misc: [self.particle_time.max(0.0), layer.life, layer.sun, if layer.tumble { 1.0 } else { 0.0 }],
            view: [width as f32, height as f32, 0.0, 0.0],
        };
        (u, count)
    }

    /// Ciel de la carte (`None` : fond uni, sans brouillard). Il n'est dessiné qu'avec le décor.
    pub fn set_sky(&mut self, sky: Option<&crate::sky::Sky>) {
        self.fog = sky.and_then(|s| s.fog.clone());
        self.dome_exposure = sky.map_or(1.0, |s| s.dome_exposure);
        self.night_air = sky.map_or(1.0, |s| s.night_air);
        self.horizon_lift = sky.map_or(1.0, |s| s.horizon_lift);
        // exposition et étalonnage du jeu à l'heure du ciel (servent si l'ambiance les demande)
        let grading = sky.and_then(|s| s.grading.as_ref());
        let luts = grading.and_then(|g| g.luts.as_ref());
        self.post.set_luts(&self.queue, luts.map(|(a, b, _)| (a.as_slice(), b.as_slice())));
        self.post_settings.white_point = grading.map_or(crate::post::DEFAULT_WHITE_POINT, |g| g.white_point);
        self.post_settings.luts = luts.is_some();
        self.post_settings.lut_mix = luts.map_or(0.0, |l| l.2);
        let img = sky.map(|s| &s.image);
        self.sky_view = match img {
            Some(img) => Self::make_texture(&self.device, &self.queue, img.width, img.height, &img.mips[..1], COLOR_FORMAT),
            None => Self::make_texture(&self.device, &self.queue, 1, 1, &[vec![0, 0, 0, 255]], COLOR_FORMAT),
        };
        self.rebuild_globals_bind();
        // textures des astres et des nuages
        let mut views: HashMap<String, wgpu::TextureView> = HashMap::new();
        for (name, img) in sky.map(|s| s.textures.as_slice()).unwrap_or_default() {
            self.upload_texture(name, img);
            let key = name.to_ascii_lowercase();
            if let Some(v) = self.textures.get(&key) {
                views.insert(key, v.clone());
            }
        }
        self.clouds = sky.and_then(|s| s.clouds.clone()).filter(|c| views.contains_key(&c.texture.to_ascii_lowercase()));
        self.clouds_view = self.clouds.as_ref().and_then(|c| views.get(&c.texture.to_ascii_lowercase()).cloned());
        let none = Self::make_texture(&self.device, &self.queue, 1, 1, &[vec![0, 0, 0, 0]], COLOR_FORMAT);
        self.sky = img.map(|_| {
            let view = &self.sky_view;
            let clouds = self.clouds_view.as_ref().unwrap_or(&none);
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("sky"),
                layout: &self.sky_bind_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: self.sky_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(view) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sky_sampler) },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(clouds) },
                    wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(&self.sky_repeat_sampler) },
                ],
            })
        });
        self.satellites = sky
            .map(|s| s.satellites.as_slice())
            .unwrap_or_default()
            .iter()
            .filter_map(|sat| {
                let view = views.get(&sat.texture.to_ascii_lowercase())?;
                let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("astre"),
                    size: std::mem::size_of::<SatUniform>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("astre"),
                    layout: &self.sat_bind_layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(view) },
                    ],
                });
                Some(GpuSatellite { sat: sat.clone(), buf, bind })
            })
            .collect();
    }

    /// Paramètres d'un astre pour cette image. Le disque du soleil suit la lumière du studio
    /// (heure du ciel, réglage à la main ou clés de soleil) : il est là où sont les ombres.
    fn sat_uniform(&self, s: &crate::sky::Satellite) -> SatUniform {
        let dir = if s.sun {
            -self.light_dir.normalize_or(Vec3::NEG_Y)
        } else {
            let (h, p) = (s.heading.to_radians(), s.pitch.to_radians());
            Vec3::new(p.cos() * h.sin(), p.sin(), p.cos() * h.cos())
        };
        // Une planète cache le ciel derrière elle seulement quand il fait sombre : de jour, la
        // lumière de l'atmosphère, devant elle, l'emporte (comme la lune en plein jour). Le disque
        // du soleil suit la même règle : le jour il s'ajoute au ciel, la nuit (c'est la lune) il
        // cache les nuages et le halo, sinon il n'était qu'un aplat clair sans ses taches. Les
        // étoiles s'ajoutent toujours.
        let sun_lum = self.sun_color.dot(Vec3::new(0.2126, 0.7152, 0.0722));
        let occlusion = if s.additive() { 0.0 } else { (1.0 - sun_lum * 1.6).clamp(0.0, 1.0) };
        SatUniform {
            dir: [dir.x, dir.y, dir.z, s.size],
            uv: s.uv,
            color: [s.color[0], s.color[1], s.color[2], 0.0],
            roll: [s.roll.to_radians(), occlusion, if s.sun { 1.0 } else { 0.0 }, MOON_CONTRAST],
        }
    }

    /// Astres : mélangés par-dessus le dôme, sans profondeur.
    fn sat_pipeline(
        device: &wgpu::Device,
        shader: &wgpu::ShaderModule,
        layout: &wgpu::PipelineLayout,
        samples: u32,
    ) -> wgpu::RenderPipeline {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("astres"),
            layout: Some(layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: Some("vs_sat"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: Some("fs_sat"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: HDR_FORMAT,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: false,
                depth_compare: wgpu::CompareFunction::Always,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState { count: samples, ..Default::default() },
            multiview: None,
            cache: None,
        })
    }

    /// Réglages du brouillard venant du décor (affichage, densité, repère).
    pub fn set_fog_settings(&mut self, s: &crate::decor::DecorSettings) {
        self.fog_enabled = s.fog;
        self.fog_density = s.fog_density.max(0.0);
        self.sun_rays = if s.sun_rays { s.sun_rays_strength.max(0.0) } else { 0.0 };
        self.decor_lights_on = s.lamps;
        self.decor_lights_strength = s.lamps_strength.max(0.0);
        self.set_decor_ground(s.ground.unwrap_or(0.0));
        self.decor_reach = if s.terrain { crate::decor::terrain_reach(s) } else { f32::MAX };
    }

    fn make_globals_bind(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        buf: &wgpu::Buffer,
        env: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
        shadow: &wgpu::TextureView,
        shadow_sampler: &wgpu::Sampler,
        sky: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("globals"),
            layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(env) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(sampler) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(shadow) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(shadow_sampler) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(sky) },
            ],
        })
    }

    fn rebuild_globals_bind(&mut self) {
        self.globals_bind = Self::make_globals_bind(
            &self.device,
            &self.globals_layout,
            &self.globals_buf,
            &self.env_view,
            &self.env_sampler,
            &self.shadow_view,
            &self.shadow_sampler,
            &self.sky_view,
        );
    }

    /// Anticrénelage : 1 (aucun), 2, 4 ou 8 échantillons par pixel, ramené au plus grand nombre
    /// accepté par la carte. Les cibles se recréent d'elles-mêmes.
    pub fn set_msaa(&mut self, samples: u32) {
        let samples = self.sample_counts.iter().copied().filter(|&n| n <= samples.max(1)).max().unwrap_or(1);
        if samples == self.samples {
            return;
        }
        let p = Self::scene_pipelines(&self.device, &self.shader, &self.mesh_layout, &self.line_layout, samples);
        self.mesh_pipeline = p.mesh;
        self.decor_pipeline = p.decor;
        self.ff_mesh_pipeline = p.ff_mesh;
        self.ff_decor_pipeline = p.ff_decor;
        self.line_depth_pipeline = p.line_depth;
        self.line_overlay_pipeline = p.line_overlay;
        self.ground_pipeline = p.ground;
        self.sky_pipeline = Self::sky_pipeline(&self.device, &self.sky_shader, &self.sky_layout, samples);
        self.sat_pipeline = Self::sat_pipeline(&self.device, &self.sky_shader, &self.sat_layout, samples);
        self.particle_pipeline = Self::particle_pipeline(&self.device, &self.particle_shader, &self.particle_layout, samples);
        self.post.set_samples(&self.device, samples);
        self.samples = samples;
    }

    /// Côté de la carte d'ombre (texels, borné à la limite du GPU).
    pub fn set_shadow_size(&mut self, size: u32) {
        let size = size.clamp(256, self.device.limits().max_texture_dimension_2d);
        if size == self.shadow_size {
            return;
        }
        (self.shadow_view, self.shadow_layers) = Self::make_shadow_maps(&self.device, size);
        self.shadow_size = size;
        self.rebuild_globals_bind();
    }

    /// Cartes d'ombre : une couche pour les personnages, une pour le décor.
    fn make_shadow_maps(device: &wgpu::Device, size: u32) -> (wgpu::TextureView, [wgpu::TextureView; 2]) {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("shadow"),
            size: wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 2 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: SHADOW_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let all = tex.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let layers = std::array::from_fn(|i| {
            tex.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2),
                base_array_layer: i as u32,
                array_layer_count: Some(1),
                ..Default::default()
            })
        });
        (all, layers)
    }

    /// Qualité des textures : niveaux de mip ignorés (0 = pleine résolution, 1 = moitié…).
    pub fn set_texture_lod(&mut self, lod: f32) {
        if lod == self.texture_lod {
            return;
        }
        self.sampler = Self::material_sampler(&self.device, lod);
        self.clamp_sampler = Self::material_sampler_with(&self.device, lod, wgpu::AddressMode::ClampToEdge);
        self.texture_lod = lod;
        // les matériaux référencent l'échantillonneur
        self.materials.clear();
    }

    fn material_sampler(device: &wgpu::Device, lod: f32) -> wgpu::Sampler {
        Self::material_sampler_with(device, lod, wgpu::AddressMode::Repeat)
    }

    fn material_sampler_with(device: &wgpu::Device, lod: f32, mode: wgpu::AddressMode) -> wgpu::Sampler {
        device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("diffuse"),
            address_mode_u: mode,
            address_mode_v: mode,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            anisotropy_clamp: 16,
            lod_min_clamp: lod,
            ..Default::default()
        })
    }


    /// Active une ambiance (panorama du jeu) ou revient à l'éclairage studio (`None`).
    pub fn set_environment(&mut self, env: Option<&crate::environment::EnvMap>) {
        let view = match env {
            Some(e) => Self::make_texture(&self.device, &self.queue, e.width, e.height, &e.mips, COLOR_FORMAT),
            None => Self::make_texture(&self.device, &self.queue, 1, 1, &[vec![0, 0, 0, 255]], COLOR_FORMAT),
        };
        self.env_view = view;
        self.rebuild_globals_bind();
        self.env = env.map(|e| GpuEnv {
            sh: e.sh.map(|c| [c[0], c[1], c[2], 0.0]),
            scale: e.scale,
            mip_count: e.mips.len() as u32,
        });
    }

    fn make_texture(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        mips: &[Vec<u8>],
        format: wgpu::TextureFormat,
    ) -> wgpu::TextureView {
        Self::make_texture_layers(device, queue, width, height, 1, false, mips, format)
    }

    /// Texture à `layers` couches (chaque niveau de `mips` les contient à la suite) ; `array` :
    /// vue en tableau de textures (cubemaps de détail).
    #[allow(clippy::too_many_arguments)]
    fn make_texture_layers(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        layers: u32,
        array: bool,
        mips: &[Vec<u8>],
        format: wgpu::TextureFormat,
    ) -> wgpu::TextureView {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("material"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: layers },
            mip_level_count: mips.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        // formats compressés : copie par blocs, sur la taille « physique » du niveau (arrondie au
        // bloc, pour les derniers mips de moins de 4 pixels)
        let (bw, bh) = format.block_dimensions();
        let block_bytes = format.block_copy_size(None).unwrap_or(4);
        let (mut w, mut h) = (width, height);
        for (level, data) in mips.iter().enumerate() {
            let (cols, rows) = (w.div_ceil(bw), h.div_ceil(bh));
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &tex,
                    mip_level: level as u32,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                data,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(cols * block_bytes), rows_per_image: Some(rows) },
                wgpu::Extent3d { width: cols * bw, height: rows * bh, depth_or_array_layers: layers },
            );
            w = (w / 2).max(1);
            h = (h / 2).max(1);
        }
        let dimension = if array { wgpu::TextureViewDimension::D2Array } else { wgpu::TextureViewDimension::D2 };
        tex.create_view(&wgpu::TextureViewDescriptor { dimension: Some(dimension), ..Default::default() })
    }

    /// Bind group d'un matériau ; une texture absente est remplacée par sa valeur par défaut.
    /// Le camouflage et la décalcomanie ne sont actifs que si leur texture est chargée.
    fn make_material(&self, mesh: &GpuMesh, camo: Option<&str>, decal: Option<&str>) -> wgpu::BindGroup {
        // (une texture à couches ne peut servir que de cubemap de détail)
        let flat = |n: &str| self.textures.get(n).filter(|_| !self.layered.contains(n));
        let get = |name: Option<&str>, default| name.and_then(flat).unwrap_or(default);
        let d = get(mesh.diffuse.as_deref(), &self.default_diffuse);
        let n_given = mesh.normal.as_deref().and_then(flat).is_some();
        let n = get(mesh.normal.as_deref(), &self.default_normal);
        // terrain : le détail du sol prend la place du `_S`, celui de la roche celle du camouflage
        let detail = |k: usize| mesh.detail.get(k).and_then(|d| self.textures.get(&d.0).map(|v| (v, d.1)));
        let (ground, rock) = (detail(0), detail(1));
        let s = ground.map(|g| g.0).unwrap_or_else(|| get(mesh.specular.as_deref(), &self.default_spec));
        let camo_view = camo.and_then(flat);
        let c = rock.map(|r| r.0).or(camo_view).unwrap_or(&self.default_camo);
        let decal_view = decal.and_then(flat);
        // détail répété : il faut le choix `_DS` et la cubemap (six motifs)
        let cube = mesh.detail_cube.as_deref().filter(|c| self.layered.contains(*c)).and_then(|c| self.textures.get(c));
        let select = mesh.detail_select.as_deref().and_then(flat);
        let (select, cube) = match (select, cube) {
            (Some(s), Some(c)) => (Some(s), Some(c)),
            _ => (None, None),
        };
        // terrain mélangé : carte de mélange (à la place du choix `_DS`) et tableaux des détails
        // d'écosystème (couleur à la place de la cubemap, relief en liaison 11)
        let layer = |n: &String| self.textures.get(n).filter(|_| self.layered.contains(n));
        let blend = match (mesh.terrain, mesh.blend.as_deref().and_then(flat), mesh.blend_layers.as_ref()) {
            (true, Some(b), Some([c, r])) => layer(c).zip(layer(r)).map(|(c, r)| (b, c, r)),
            _ => None,
        };
        let overlay = mesh.overlay.as_deref().and_then(flat);
        // la flore n'a pas de couleur superposée : son masque de teinte prend l'emplacement
        let tint_mask = mesh.tint_mask.as_deref().and_then(flat);
        let dv = decal_view.unwrap_or(&self.default_decal);
        let on = |v: Option<&wgpu::TextureView>| if v.is_some() { 1.0 } else { 0.0 };
        let flag = |b: bool| if b { 1.0 } else { 0.0 };
        let surface = if mesh.water { 2.0 } else { flag(mesh.terrain) };
        // champ de force : sa couleur à la place du détail du terrain
        let detail_params = if mesh.forcefield {
            [mesh.ff_color[0], mesh.ff_color[1], mesh.ff_color[2], 1.0]
        } else {
            [ground.map_or(1.0, |g| g.1), rock.map_or(1.0, |r| r.1), flag(ground.is_some()), flag(rock.is_some())]
        };
        let params: [f32; 16] = [
            on(camo_view),
            on(decal_view),
            flag(mesh.alpha_test),
            surface,
            detail_params[0],
            detail_params[1],
            detail_params[2],
            detail_params[3],
            on(cube),
            // la couleur superposée suit le masque du relief (canal B) : il faut la carte
            flag(overlay.is_some() && n_given),
            // terrain : côtés où le détail s'estompe (bits, voir `forgelight::Mesh::terrain_edges`)
            mesh.detail_edges as f32,
            // masque de teinte de la flore (dans l'emplacement de la couleur superposée)
            flag(overlay.is_none() && tint_mask.is_some()),
            mesh.detail_repeat,
            flag(blend.is_some()),
            0.0,
            0.0,
        ];
        let mut params: Vec<f32> = params.to_vec();
        // écosystèmes des tuiles : couches, puis côtés d'une répétition
        params.extend((0..20).map(|k| mesh.blend_ecos.get(k).map_or(-1.0, |e| e.0 as f32)));
        params.extend((0..20).map(|k| mesh.blend_ecos.get(k).map_or(1.0, |e| e.1)));
        // eau des marais : rides (à la place du relief), tourbillons (du `_S`), relief des bulles
        // (du camouflage) et nappe d'algues (de la couleur superposée) ; `detail` : couleur de
        // l'eau, `extra` : textures présentes. La couleur reste la mousse des algues.
        let (mut n, mut s, mut c) = (n, s, c);
        let mut ov = overlay.or(tint_mask).unwrap_or(&self.default_camo);
        if mesh.water {
            let bump1 = mesh.water_bump1.as_deref().and_then(flat);
            let bump2 = mesh.water_bump2.as_deref().and_then(flat);
            let foam_n = mesh.water_foam_n.as_deref().and_then(flat);
            let algae = mesh.water_overlay.as_deref().and_then(flat);
            let foam = mesh.diffuse.as_deref().and_then(flat).is_some();
            n = bump1.unwrap_or(&self.default_normal);
            s = bump2.unwrap_or(&self.default_normal);
            c = foam_n.unwrap_or(&self.default_normal);
            ov = algae.unwrap_or(&self.default_camo);
            let [r, g, b] = mesh.water_tint;
            params[4..12].copy_from_slice(&[r, g, b, 0.0, on(bump1), on(bump2), on(foam_n), flag(algae.is_some() && foam)]);
        }
        let buf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("material"),
            contents: bytemuck::cast_slice(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("material"),
            layout: &self.tex_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(d) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(n) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(s) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(c) },
                wgpu::BindGroupEntry { binding: 5, resource: buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::TextureView(dv) },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: wgpu::BindingResource::TextureView(
                        blend.map(|b| b.0).or(select).unwrap_or(&self.default_detail_select),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: wgpu::BindingResource::TextureView(blend.map(|b| b.1).or(cube).unwrap_or(&self.default_detail_cube)),
                },
                wgpu::BindGroupEntry {
                    binding: 11,
                    resource: wgpu::BindingResource::TextureView(blend.map(|b| b.2).unwrap_or(&self.default_detail_cube)),
                },
                wgpu::BindGroupEntry {
                    binding: 9,
                    resource: wgpu::BindingResource::TextureView(ov),
                },
                // couleur et relief propres au maillage : chaque quart de terrain a sa texture, qui
                // ne boucle pas (sinon son bord opposé déborde et dessine les carrés)
                wgpu::BindGroupEntry {
                    binding: 10,
                    resource: wgpu::BindingResource::Sampler(if mesh.terrain { &self.clamp_sampler } else { &self.sampler }),
                },
            ],
        })
    }

    /// Noms (en minuscules) des textures déjà envoyées au GPU.
    pub fn texture_names(&self) -> std::collections::HashSet<String> {
        self.textures.keys().cloned().collect()
    }

    pub fn has_texture(&self, name: &str) -> bool {
        self.textures.contains_key(&name.to_ascii_lowercase())
    }

    pub fn upload_texture(&mut self, name: &str, img: &dds::Image) {
        let linear = is_linear_texture(name);
        // blocs BC envoyés tels quels si le GPU sait les lire, sinon décompressés
        let format = match (img.encoding, self.device.features().contains(wgpu::Features::TEXTURE_COMPRESSION_BC)) {
            (dds::Encoding::Rgba8, _) | (_, false) => None,
            (dds::Encoding::Bc1, true) => Some(if linear { wgpu::TextureFormat::Bc1RgbaUnorm } else { wgpu::TextureFormat::Bc1RgbaUnormSrgb }),
            (dds::Encoding::Bc2, true) => Some(if linear { wgpu::TextureFormat::Bc2RgbaUnorm } else { wgpu::TextureFormat::Bc2RgbaUnormSrgb }),
            (dds::Encoding::Bc3, true) => Some(if linear { wgpu::TextureFormat::Bc3RgbaUnorm } else { wgpu::TextureFormat::Bc3RgbaUnormSrgb }),
            (dds::Encoding::Bc7, true) => Some(if linear { wgpu::TextureFormat::Bc7RgbaUnorm } else { wgpu::TextureFormat::Bc7RgbaUnormSrgb }),
        };
        let decoded;
        let (img, format) = match format {
            Some(f) => (img, f),
            None if img.encoding != dds::Encoding::Rgba8 => match img.to_rgba() {
                Ok(mut d) => {
                    // décompressée, elle prend 4 à 8 fois plus de place
                    crate::decor::limit_mips(&mut d, crate::decor::MAX_TEXTURE);
                    decoded = d;
                    (&decoded, if linear { DATA_FORMAT } else { COLOR_FORMAT })
                }
                Err(e) => {
                    eprintln!("texture {name} : {e:#}");
                    return;
                }
            },
            None => (img, if linear { DATA_FORMAT } else { COLOR_FORMAT }),
        };
        // niveaux de mip valides uniquement (taille cohérente)
        let mut mips = Vec::new();
        let (mut w, mut h) = (img.width, img.height);
        for m in &img.mips {
            if m.len() != img.encoding.level_size(w, h) * img.layers as usize {
                break;
            }
            mips.push(m.clone());
            if w == 1 && h == 1 {
                break;
            }
            w = (w / 2).max(1);
            h = (h / 2).max(1);
        }
        if mips.is_empty() {
            return;
        }
        let layered = img.layers > 1;
        let view = Self::make_texture_layers(&self.device, &self.queue, img.width, img.height, img.layers, layered, &mips, format);
        let key = name.to_ascii_lowercase();
        if layered {
            self.layered.insert(key.clone());
        } else {
            self.layered.remove(&key);
        }
        self.textures.insert(key, view);
        // les matériaux qui attendaient cette texture seront recréés
        self.materials.clear();
    }

    pub fn has_model(&self, asset: &str) -> bool {
        self.models.contains_key(asset)
    }

    pub fn upload_model(&mut self, asset: &str, model: &forgelight::Model) {
        let meshes = model
            .meshes
            .iter()
            .filter(|m| !m.indices.is_empty() && !m.vertices.is_empty() && !is_hidden(m))
            .map(|m| GpuMesh {
                vbuf: self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("vertices"),
                    contents: bytemuck::cast_slice(&m.vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                }),
                ibuf: self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("indices"),
                    contents: bytemuck::cast_slice(&m.indices),
                    usage: wgpu::BufferUsages::INDEX,
                }),
                index_count: m.indices.len() as u32,
                diffuse: m.diffuse.as_ref().map(|t| t.to_ascii_lowercase()),
                normal: m.normal_map.as_ref().map(|t| t.to_ascii_lowercase()),
                specular: m.specular.as_ref().map(|t| t.to_ascii_lowercase()),
                camo_default: m.camo.as_ref().map(|t| t.to_ascii_lowercase()),
                accepts_camo: m.accepts_camo(),
                decal_default: m
                    .decal
                    .as_ref()
                    .filter(|t| !crate::decal::is_blank(t))
                    .map(|t| t.to_ascii_lowercase()),
                accepts_decal: m.accepts_decal(),
                material: [&m.diffuse, &m.normal_map, &m.specular]
                    .map(|t| t.as_deref().unwrap_or("").to_ascii_lowercase())
                    .join("|")
                    + if is_alpha_tested(&m.material_definition) { "|a" } else { "" }
                    + if m.material_definition == "Terrain" { "|t" } else { "" }
                    + if is_water(&m.material_definition) { "|w" } else { "" }
                    + &if m.is_forcefield() { format!("|ff{:?}", forcefield_color(m.color)) } else { String::new() }
                    + &m.terrain_detail.iter().map(|d| format!("|{}", d.0.to_ascii_lowercase())).collect::<String>()
                    + &if m.terrain_edges != 0 { format!("|e{}", m.terrain_edges) } else { String::new() }
                    + &m.terrain_blend.as_ref().map_or(String::new(), |t| format!("|{}", t.to_ascii_lowercase()))
                    + &m.terrain_layers.as_ref().map_or(String::new(), |l| format!("|{}", l[0].to_ascii_lowercase()))
                    + &[slot::DETAIL_SELECT, slot::DETAIL, slot::OVERLAY]
                        .map(|s| m.slot_texture(s).map_or(String::new(), |t| format!("|{}", t.to_ascii_lowercase())))
                        .concat()
                    + &format!("|r{}", m.detail_repeat)
                    + &m.water_textures().map(|t| format!("|{}", t.to_ascii_lowercase())).collect::<String>(),
                alpha_test: is_alpha_tested(&m.material_definition),
                terrain: m.material_definition == "Terrain",
                water: is_water(&m.material_definition),
                forcefield: m.is_forcefield(),
                ff_color: forcefield_color(m.color),
                detail: m.terrain_detail.iter().map(|(t, s)| (t.to_ascii_lowercase(), *s)).collect(),
                detail_edges: m.terrain_edges,
                blend: m.terrain_blend.as_ref().map(|t| t.to_ascii_lowercase()),
                blend_layers: m.terrain_layers.as_ref().map(|l| l.clone().map(|t| t.to_ascii_lowercase())),
                blend_ecos: m.terrain_ecos.clone(),
                detail_select: m.slot_texture(slot::DETAIL_SELECT).map(|t| t.to_ascii_lowercase()),
                detail_cube: m.slot_texture(slot::DETAIL).map(|t| t.to_ascii_lowercase()),
                detail_repeat: m.detail_repeat,
                // gris moyen : sans effet
                overlay: m.slot_texture(slot::OVERLAY).filter(|t| !t.eq_ignore_ascii_case("grey.dds")).map(|t| t.to_ascii_lowercase()),
                tint_mask: m.slot_texture(slot::TINT_MASK).map(|t| t.to_ascii_lowercase()),
                water_bump1: water_slot(m, slot::WATER_BUMP1),
                water_bump2: water_slot(m, slot::WATER_BUMP2),
                water_foam_n: water_slot(m, slot::WATER_FOAM_NORMAL),
                water_overlay: water_slot(m, slot::WATER_OVERLAY),
                water_tint: water_color(m.water_tint),
            })
            .collect();
        let bind_bones = model.skeleton.bones.iter().map(|b| b.bind_world.w_axis.truncate()).collect();
        self.models
            .insert(asset.to_string(), GpuModel { meshes, bind_bones, bounds: (model.bounds_min, model.bounds_max) });
    }

    pub fn forget_actor(&mut self, id: u64) {
        self.actors.remove(&id);
    }

    /// Remplace le décor. Les modèles des instances doivent avoir été envoyés par
    /// [`Self::upload_model`] ; les instances sans modèle sont ignorées au dessin.
    /// `ground` : hauteur du jeu ramenée à y = 0 dans ces matrices (voir [`Self::set_decor_ground`]).
    /// Lumières placées du décor (lampes, projecteurs).
    pub fn set_decor_lights(&mut self, lights: &[DecorLight]) {
        self.decor_lights = lights.to_vec();
    }

    /// Les lumières du décor prêtes pour le shader : celles dont la portée touche le champ de la
    /// caméra, les plus proches d'abord (au plus [`MAX_LIGHTS`]). Une lumière compte d'après la
    /// distance de la caméra au bord de sa portée ; près de la limite de la liste, elle s'atténue
    /// en fondu ([`LIGHT_FADE`]) pour entrer et sortir sans saut quand la caméra bouge.
    fn light_uniform(&self, eye: Vec3, view_proj: Mat4) -> (usize, [[f32; 4]; MAX_LIGHTS * 3]) {
        let mut out = [[0.0; 4]; MAX_LIGHTS * 3];
        let k = LIGHT_STRENGTH * self.decor_lights_strength.max(0.0);
        if !self.decor_lights_on || !self.decor_active() || k <= 0.0 {
            return (0, out);
        }
        // décor relevé ou abaissé (« sol à ») : les lumières suivent
        let lift = Vec3::new(0.0, self.decor_base_ground - self.decor_ground, 0.0);
        // plans du champ (gauche, droite, bas, haut, devant la caméra), tirés de la projection
        let m = view_proj.transpose();
        let planes = [m.w_axis + m.x_axis, m.w_axis - m.x_axis, m.w_axis + m.y_axis, m.w_axis - m.y_axis, m.w_axis]
            .map(|p| p / p.truncate().length().max(1e-6));
        let visible = |c: Vec3, r: f32| planes.iter().all(|p| p.truncate().dot(c) + p.w > -r);
        let mut near: Vec<(f32, &DecorLight)> = self
            .decor_lights
            .iter()
            .filter(|l| visible(l.pos + lift, l.range))
            .map(|l| (((l.pos + lift) - eye).length() - l.range, l))
            .collect();
        let n = near.len().min(MAX_LIGHTS);
        if n == 0 {
            return (0, out);
        }
        // seuil : la première lumière laissée de côté (aucune : pas de fondu)
        let cutoff = if near.len() > MAX_LIGHTS {
            near.select_nth_unstable_by(MAX_LIGHTS, |a, b| a.0.total_cmp(&b.0));
            near[MAX_LIGHTS].0
        } else {
            f32::INFINITY
        };
        for (i, (prio, l)) in near[..n].iter().enumerate() {
            let fade = ((cutoff - prio) / LIGHT_FADE).clamp(0.0, 1.0);
            let p = l.pos + lift;
            let c = l.color * k * fade;
            let (dir, ci, co) = l.spot.unwrap_or((Vec3::NEG_Y, -2.0, -2.0));
            out[i * 3] = [p.x, p.y, p.z, l.range];
            out[i * 3 + 1] = [c.x, c.y, c.z, ci];
            out[i * 3 + 2] = [dir.x, dir.y, dir.z, co];
        }
        (n, out)
    }

    pub fn set_decor(&mut self, instances: &[DecorInstance], ground: f32) {
        self.decor_base_ground = ground;
        self.decor_ground = ground;
        if instances.is_empty() {
            self.decor = None;
            return;
        }
        let mut order: Vec<usize> = (0..instances.len()).collect();
        order.sort_by(|&a, &b| {
            (&instances[a].asset, instances[a].cast_shadow).cmp(&(&instances[b].asset, instances[b].cast_shadow))
        });
        let mut mats: Vec<[[f32; 4]; 4]> = Vec::with_capacity(order.len());
        let mut batches: Vec<DecorBatch> = Vec::new();
        let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for &i in &order {
            let it = &instances[i];
            let same = batches.last().is_some_and(|b| b.asset == it.asset && b.shadow == it.cast_shadow);
            if !same {
                batches.push(DecorBatch { asset: it.asset.clone(), first: mats.len() as u32, count: 0, shadow: it.cast_shadow });
            }
            batches.last_mut().unwrap().count += 1;
            let mut m = it.matrix.to_cols_array_2d();
            if let Some(t) = it.tint {
                // (une teinte noire vaudrait « aucune » : on la garde juste au-dessus de 0)
                for k in 0..3 {
                    m[k][3] = t[k].max(1e-4);
                }
            }
            mats.push(m);
            if let Some(m) = self.models.get(&it.asset) {
                let (blo, bhi) = m.bounds;
                for k in 0..8 {
                    let c = Vec3::new(
                        if k & 1 == 0 { blo.x } else { bhi.x },
                        if k & 2 == 0 { blo.y } else { bhi.y },
                        if k & 4 == 0 { blo.z } else { bhi.z },
                    );
                    let p = it.matrix.transform_point3(c);
                    lo = lo.min(p);
                    hi = hi.max(p);
                }
            }
        }
        let buf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("decor"),
            contents: bytemuck::cast_slice(&mats),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });
        // flags.y : le décor reçoit l'ombre des personnages à l'opacité « Ombre au sol »
        let u = ActorUniform { tint: [1.0; 4], flags: [0.0, 1.0, 0.0, 0.0], camo: [0.0; 4], decal: [0.0; 4] };
        let uniform = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("decor"),
            contents: bytemuck::bytes_of(&u),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("decor"),
            layout: &self.actor_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: uniform.as_entire_binding() },
            ],
        });
        if lo.x > hi.x {
            (lo, hi) = (Vec3::ZERO, Vec3::ZERO);
        }
        self.decor = Some(GpuDecor { bind, buf, _uniform: uniform, batches, bounds: (lo, hi), mats, lift: 0.0 });
    }

    /// Nappes d'eau du décor dessiné (instances des maillages d'eau), pour les sillages.
    pub fn water_planes(&self) -> Vec<crate::wake::WaterPlane> {
        let Some(d) = self.decor.as_ref().filter(|_| self.decor_active()) else { return Vec::new() };
        let mut out = Vec::new();
        for b in &d.batches {
            let Some(model) = self.models.get(&b.asset) else { continue };
            if !model.meshes.iter().any(|m| m.water) {
                continue;
            }
            let (lo, hi) = model.bounds;
            for k in b.first..b.first + b.count {
                let mut m = d.mats[k as usize];
                // (la 4e ligne des trois premières colonnes porte la teinte de l'instance)
                for c in m.iter_mut().take(3) {
                    c[3] = 0.0;
                }
                m[3][1] += d.lift;
                let matrix = Mat4::from_cols_array_2d(&m);
                out.push(crate::wake::WaterPlane { matrix, inverse: matrix.inverse(), lo, hi });
            }
        }
        out
    }

    /// Change la hauteur du jeu ramenée à y = 0 sans recharger le décor : les instances (terrain
    /// compris) sont décalées en hauteur, le brouillard suit.
    pub fn set_decor_ground(&mut self, ground: f32) {
        self.decor_ground = ground;
        let lift = self.decor_base_ground - ground;
        let Some(d) = self.decor.as_mut() else { return };
        if (lift - d.lift).abs() < 1e-6 {
            return;
        }
        let shifted: Vec<[[f32; 4]; 4]> = d
            .mats
            .iter()
            .map(|m| {
                let mut m = *m;
                m[3][1] += lift;
                m
            })
            .collect();
        self.queue.write_buffer(&d.buf, 0, bytemuck::cast_slice(&shifted));
        let dy = Vec3::Y * (lift - d.lift);
        d.bounds = (d.bounds.0 + dy, d.bounds.1 + dy);
        d.lift = lift;
    }

    pub fn clear_decor(&mut self) {
        self.decor = None;
        self.decor_terrain = false;
        self.set_sky(None);
        self.forget_prefix("terrain/");
    }

    /// Oublie les modèles et textures dont le nom commence par `prefix` (terrain d'un décor
    /// précédent).
    pub fn forget_prefix(&mut self, prefix: &str) {
        self.models.retain(|k, _| !k.starts_with(prefix));
        let n = self.textures.len();
        self.textures.retain(|k, _| !k.starts_with(prefix));
        self.layered.retain(|k| !k.starts_with(prefix));
        if n != self.textures.len() {
            self.materials.clear();
        }
    }

    /// Le décor est-il dessiné à cette image ?
    fn decor_active(&self) -> bool {
        self.decor_visible && self.decor.is_some()
    }

    fn update_actor(&mut self, item: &DrawItem) {
        let n = item.skin.len().max(1);
        let needs_new = self.actors.get(&item.id).is_none_or(|a| a.bone_cap < n);
        if needs_new {
            let bone_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("bones"),
                size: (n * 64) as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let uniform = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("actor"),
                size: std::mem::size_of::<ActorUniform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("actor"),
                layout: &self.actor_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: bone_buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: uniform.as_entire_binding() },
                ],
            });
            self.actors.insert(item.id, GpuActor { bone_buf, bone_cap: n, uniform, bind });
        }
        let ga = &self.actors[&item.id];
        let mats: Vec<[[f32; 4]; 4]> = if item.skin.is_empty() {
            vec![Mat4::IDENTITY.to_cols_array_2d()]
        } else {
            item.skin.iter().map(|m| m.to_cols_array_2d()).collect()
        };
        self.queue.write_buffer(&ga.bone_buf, 0, bytemuck::cast_slice(&mats));
        let camo = if item.camo.is_active() { [item.camo.strength.clamp(0.0, 1.0), item.camo.scale, 0.0, 0.0] } else { [0.0; 4] };
        let decal = if item.decal.is_active() { [item.decal.opacity.clamp(0.0, 1.0), 0.0, 0.0, 0.0] } else { [0.0; 4] };
        let u = ActorUniform {
            tint: [1.0; 4],
            // z : 1 si le décor ne lui fait pas d'ombre
            flags: [if item.highlight { 1.0 } else { 0.0 }, 0.0, if self.decor_shades_actors { 0.0 } else { 1.0 }, 0.0],
            camo,
            decal,
        };
        self.queue.write_buffer(&ga.uniform, 0, bytemuck::bytes_of(&u));
    }

    fn ensure_lines(&mut self, n: usize) {
        if n > self.line_cap {
            self.line_cap = n.next_power_of_two();
            self.line_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("lines"),
                size: (self.line_cap * std::mem::size_of::<LineVertex>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
    }

    /// Redimensionne la cible du viewport et l'enregistre auprès d'egui.
    pub fn ensure_viewport(&mut self, rs: &egui_wgpu::RenderState, width: u32, height: u32) {
        let (mut t, mut tex) = (self.viewport.take(), self.egui_tex);
        self.ensure_registered(rs, &mut t, &mut tex, width, height);
        (self.viewport, self.egui_tex) = (t, tex);
    }

    /// Même chose pour le moniteur de la caméra de prise.
    pub fn ensure_monitor(&mut self, rs: &egui_wgpu::RenderState, width: u32, height: u32) {
        let (mut t, mut tex) = (self.monitor.take(), self.monitor_tex);
        self.ensure_registered(rs, &mut t, &mut tex, width, height);
        (self.monitor, self.monitor_tex) = (t, tex);
    }

    /// Dessine un modèle seul (sans décor, ciel, particules ni profondeur de champ) dans la cible
    /// de l'aperçu, enregistrée auprès d'egui ([`Self::preview_tex`]).
    pub fn render_preview(&mut self, rs: &egui_wgpu::RenderState, size: u32, view_proj: Mat4, eye: Vec3, item: &DrawItem) {
        let (mut t, mut tex) = (self.preview.take(), self.preview_tex);
        self.ensure_registered(rs, &mut t, &mut tex, size, size);
        let saved_ps = self.post_settings;
        let saved = (self.decor_visible, self.render_ss, std::mem::take(&mut self.particles));
        self.decor_visible = false;
        self.render_ss = 1.0;
        self.post_settings.fx.dof = false;
        self.post_settings.fx.grain = 0.0;
        self.post_settings.fx.vignette = 0.0;
        self.post_settings.frame = None;
        self.post_settings.ao_guide = None;
        self.render(t.as_ref(), view_proj, eye, std::slice::from_ref(item), &Lines::default());
        self.post_settings = saved_ps;
        (self.decor_visible, self.render_ss, self.particles) = saved;
        (self.preview, self.preview_tex) = (t, tex);
    }

    /// Libère un modèle chargé pour l'aperçu, et celles de `textures` qu'aucun autre modèle
    /// en mémoire (scène, décor) n'utilise plus.
    pub fn forget_preview(&mut self, asset: Option<&str>, textures: &[String]) {
        if let Some(a) = asset {
            self.models.remove(a);
        }
        if textures.is_empty() {
            return;
        }
        let used: std::collections::HashSet<&str> = self.models.values().flat_map(|m| &m.meshes).flat_map(|m| m.texture_refs()).collect();
        let gone: Vec<String> = textures.iter().map(|t| t.to_ascii_lowercase()).filter(|t| !used.contains(t.as_str())).collect();
        if gone.is_empty() {
            return;
        }
        for t in &gone {
            self.textures.remove(t);
            self.layered.remove(t);
        }
        self.materials.clear();
    }

    fn ensure_registered(
        &self,
        rs: &egui_wgpu::RenderState,
        target: &mut Option<Target>,
        tex: &mut Option<eframe::egui::TextureId>,
        width: u32,
        height: u32,
    ) {
        let (width, height) = (width.max(8), height.max(8));
        if target.as_ref().is_some_and(|t| t.width == width && t.height == height && t.samples == self.samples) {
            return;
        }
        let t = Target::new(&self.device, width, height, self.samples, &self.post);
        let mut er = rs.renderer.write();
        match *tex {
            Some(id) => er.update_egui_texture_from_wgpu_texture(&self.device, &t.resolve_view, wgpu::FilterMode::Linear, id),
            None => *tex = Some(er.register_native_texture(&self.device, &t.resolve_view, wgpu::FilterMode::Linear)),
        }
        *target = Some(t);
    }

    /// Dessine la scène dans une cible.
    pub fn render(
        &mut self,
        target: Option<&Target>,
        view_proj: Mat4,
        eye: Vec3,
        items: &[DrawItem],
        lines: &Lines,
    ) {
        for it in items {
            self.update_actor(it);
            // matériaux manquants (nouveau modèle, nouveau camouflage, ou texture arrivée depuis)
            let Some(model) = self.models.get(&it.asset) else { continue };
            let created: Vec<(String, wgpu::BindGroup)> = model
                .meshes
                .iter()
                .filter_map(|m| {
                    let (key, camo, decal) = m.resolve(it);
                    (!self.materials.contains_key(&key)).then(|| {
                        let bind = self.make_material(m, camo.as_deref(), decal.as_deref());
                        (key, bind)
                    })
                })
                .collect();
            self.materials.extend(created);
        }
        if self.decor_active() {
            let mut created: Vec<(String, wgpu::BindGroup)> = Vec::new();
            for b in &self.decor.as_ref().unwrap().batches {
                let Some(model) = self.models.get(&b.asset) else { continue };
                for m in &model.meshes {
                    let key = m.material_key(None, None);
                    if !self.materials.contains_key(&key) && !created.iter().any(|(k, _)| *k == key) {
                        created.push((key, self.make_material(m, None, None)));
                    }
                }
            }
            self.materials.extend(created);
        }
        let flag = |b: bool| if b { 1.0 } else { 0.0 };
        let (char_shadow, shadow) = if self.shadows { self.fit_shadows(items) } else { (None, None) };
        for (i, s) in [&char_shadow, &shadow].into_iter().enumerate() {
            if let Some(s) = s {
                self.queue.write_buffer(&self.shadow_bufs[i], 0, bytemuck::cast_slice(&s.light_vp.to_cols_array()));
            }
        }
        let draw_sky = self.sky.is_some() && self.decor_active();
        let fog = self.fog.as_ref().filter(|_| draw_sky && self.fog_enabled);
        let g = Globals {
            view_proj: view_proj.to_cols_array_2d(),
            light_dir: self.light_dir.extend(0.0).to_array(),
            cam_pos: [eye.x, eye.y, eye.z, 1.0],
            // w : feuillages (0 découpe nette, 1 alpha renforcé au loin, 2 en plus bords lissés par le MSAA)
            flags: [
                flag(self.normal_maps),
                self.emissive.max(0.0),
                self.env_intensity.max(0.0),
                match (self.foliage_aa, self.samples > 1) {
                    (false, _) => 0.0,
                    (true, false) => 1.0,
                    (true, true) => 2.0,
                },
            ],
            // le flou maximal des reflets s'arrête 2 mips avant le dernier (4×2 texels) pour garder une direction
            env: match &self.env {
                Some(e) => [
                    1.0,
                    self.env_rotation_deg.to_radians(),
                    e.mip_count.saturating_sub(3) as f32,
                    e.scale * self.env_intensity,
                ],
                None => [0.0; 4],
            },
            sh: self.env.as_ref().map(|e| e.sh).unwrap_or([[0.0; 4]; 9]),
            light_vp: shadow.as_ref().map(|s| s.light_vp).unwrap_or(Mat4::IDENTITY).to_cols_array_2d(),
            // décalage normal : 1,5 texel, pour que la surface ne s'ombre pas elle-même
            shadow: [
                flag(shadow.is_some()),
                1.0 / self.shadow_size as f32,
                self.ground_shadow.clamp(0.0, 1.0),
                shadow.as_ref().map_or(0.0, |s| s.texel * 1.5),
            ],
            ground: match &char_shadow {
                Some(s) => [s.center.x, s.center.z, 100.0, 0.0],
                None => [0.0; 4],
            },
            sun: self.sun_color.extend(0.0).to_array(),
            quality: [
                self.shadow_filter as f32,
                self.brightness.max(0.0),
                // biais négatif seulement quand l'image est suréchantillonnée (sinon : scintillement)
                -self.texture_sharpness.clamp(0.0, 1.0) * self.render_ss.max(1.0).log2().min(1.0),
                flag(self.texture_hq),
            ],
            fog: match fog {
                Some(f) => [1.0, f.density * self.fog_density, f.gradient, f.floor - self.decor_ground],
                None => [0.0; 4],
            },
            fog2: match fog {
                Some(f) => {
                    // le décor doit avoir disparu avant le bord du terrain chargé
                    let far = f.fade[1].min(self.decor_reach - 100.0).max(200.0);
                    let near = f.fade[0].min(far - 300.0).max(0.0);
                    [f.start, near, far, self.horizon_lift]
                }
                None => [0.0, 0.0, 0.0, self.horizon_lift],
            },
            fog_color: match fog {
                Some(f) => [f.color[0], f.color[1], f.color[2], 0.5],
                None => [0.0; 4],
            },
            char_vp: char_shadow.as_ref().map(|s| s.light_vp).unwrap_or(Mat4::IDENTITY).to_cols_array_2d(),
            char_shadow: match &char_shadow {
                Some(s) => [1.0, 0.0, 0.0, s.texel * 1.5],
                None => [0.0; 4],
            },
            quality2: [
                flag(self.specular_aa),
                self.terrain_detail.max(20.0),
                match (self.water_quality, draw_sky) {
                    (false, _) => 0.0,
                    (true, false) => 1.0,
                    (true, true) => 2.0,
                },
                self.particle_time.max(0.0),
            ],
            // rayons du soleil : seulement avec le décor et ses ombres ; le rayon est suivi
            // jusqu'au bout de la zone des ombres du décor
            vol: match (&shadow, self.decor_active() && self.sun_rays > 0.0) {
                (Some(s), true) => [
                    SUN_RAYS_DENSITY * self.sun_rays * if draw_sky { self.night_air } else { 1.0 },
                    ((eye - s.center).length() + self.decor_shadow_range.max(1.0) * 1.5).min(500.0),
                    SUN_RAYS_ANISOTROPY,
                    // pas le long du rayon : moins quand l'image est suréchantillonnée (la réduction
                    // moyenne le bruit du départ décalé)
                    if self.render_ss >= 2.0 { 24.0 } else { 32.0 },
                ],
                _ => [0.0; 4],
            },
            // le curseur de densité du brouillard agit aussi sur le voile (comme une épaisseur d'air)
            haze: [
                fog.filter(|f| f.haze[0] > 0.0).map_or(0.0, |f| f.haze[0] * self.haze_scale.max(0.0) * self.fog_density.max(0.0)),
                fog.map_or(1000.0, |f| f.haze[1]),
                // halo du soleil : avec le ciel de la carte
                if draw_sky { self.sun_glow.max(0.0) } else { 0.0 },
                if draw_sky { self.dome_exposure.max(1e-3) } else { 0.0 },
            ],
            wake_n: [0.0; 4],
            wake_head: [[0.0; 4]; crate::wake::MAX_CHARS],
            wake_range: [[0.0; 4]; crate::wake::MAX_CHARS],
            wake_pt: [[0.0; 4]; crate::wake::MAX_POINTS],
            light_n: [0.0; 4],
            lights: [[0.0; 4]; MAX_LIGHTS * 3],
        };
        let mut g = g;
        let (n_lights, lights) = self.light_uniform(eye, view_proj);
        g.light_n[0] = n_lights as f32;
        g.lights = lights;
        // sillages : seulement quand le décor (et donc son eau) est dessiné
        if self.decor_active() {
            let w = &self.wakes;
            let n = w.heads.len().min(w.ranges.len()).min(crate::wake::MAX_CHARS);
            let np = w.points.len().min(crate::wake::MAX_POINTS);
            g.wake_n[0] = n as f32;
            g.wake_head[..n].copy_from_slice(&w.heads[..n]);
            g.wake_range[..n].copy_from_slice(&w.ranges[..n]);
            g.wake_pt[..np].copy_from_slice(&w.points[..np]);
        }
        self.queue.write_buffer(&self.globals_buf, 0, bytemuck::bytes_of(&g));
        if draw_sky {
            let c = self.clouds.as_ref();
            let su = SkyUniform {
                inv_view_proj: view_proj.inverse().to_cols_array_2d(),
                view_proj: view_proj.to_cols_array_2d(),
                eye: eye.extend(1.0).to_array(),
                // nuages éclairés par le soleil du studio (une aurore reste visible la nuit)
                params: [
                    self.brightness.max(0.0),
                    self.particle_time.max(0.0),
                    if c.is_some() { 1.0 } else { 0.0 },
                    (self.sun_color.dot(Vec3::new(0.2126, 0.7152, 0.0722)) * 1.7).clamp(0.4, 1.5),
                ],
                cloud_w: c.map_or([0.0; 4], |c| c.weights),
                cloud_color: c.map_or([0.0, 0.0, 0.0, 1.0], |c| [c.color[0], c.color[1], c.color[2], c.contrast]),
                cloud_tiling: c.map_or([1.0, 1.0, 0.0, 0.0], |c| [c.tiling[0], c.tiling[1], c.scroll[0], c.scroll[1]]),
            };
            self.queue.write_buffer(&self.sky_buf, 0, bytemuck::bytes_of(&su));
            for s in &self.satellites {
                self.queue.write_buffer(&s.buf, 0, bytemuck::bytes_of(&self.sat_uniform(&s.sat)));
            }
        }
        let n_lines = lines.depth.len() + lines.overlay.len();
        self.ensure_lines(n_lines.max(2));
        if n_lines > 0 {
            let mut all = Vec::with_capacity(n_lines);
            all.extend_from_slice(&lines.depth);
            all.extend_from_slice(&lines.overlay);
            self.queue.write_buffer(&self.line_buf, 0, bytemuck::cast_slice(&all));
        }

        let target = match target.or(self.viewport.as_ref()) {
            Some(t) => t,
            None => return,
        };
        let particle_counts: Vec<u32> = (0..self.particles.len())
            .map(|i| {
                let (u, n) = self.particle_uniform(i, &self.particles[i].layer, target.width, target.height);
                self.queue.write_buffer(&self.particles[i].buf, 0, bytemuck::bytes_of(&u));
                n
            })
            .collect();
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("scene") });
        // ombre : personnages seuls (couche 0), puis décor seul (couche 1)
        fn shadow_pass<'a>(enc: &'a mut wgpu::CommandEncoder, view: &wgpu::TextureView) -> wgpu::RenderPass<'a> {
            enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("shadow"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Store }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            })
        }
        if char_shadow.is_some() {
            let mut pass = shadow_pass(&mut enc, &self.shadow_layers[0]);
            pass.set_pipeline(&self.shadow_pipeline);
            pass.set_bind_group(0, &self.shadow_binds[0], &[]);
            for it in items {
                let (Some(model), Some(ga)) = (self.models.get(&it.asset), self.actors.get(&it.id)) else { continue };
                pass.set_bind_group(1, &ga.bind, &[]);
                for m in model.meshes.iter().filter(|m| !m.forcefield) {
                    pass.set_vertex_buffer(0, m.vbuf.slice(..));
                    pass.set_index_buffer(m.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..m.index_count, 0, 0..1);
                }
            }
        }
        if shadow.is_some() {
            let mut pass = shadow_pass(&mut enc, &self.shadow_layers[1]);
            pass.set_bind_group(0, &self.shadow_binds[1], &[]);
            if let Some(d) = self.decor.as_ref().filter(|_| self.decor_visible) {
                pass.set_pipeline(&self.shadow_static_pipeline);
                pass.set_bind_group(1, &d.bind, &[]);
                for b in d.batches.iter().filter(|b| b.shadow) {
                    let Some(model) = self.models.get(&b.asset) else { continue };
                    for m in model.meshes.iter().filter(|m| !m.alpha_test && !m.water && !m.forcefield) {
                        pass.set_vertex_buffer(0, m.vbuf.slice(..));
                        pass.set_index_buffer(m.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                        pass.draw_indexed(0..m.index_count, 0, b.first..b.first + b.count);
                    }
                }
            }
        }
        {
            let [r, g, b] = self.clear_color;
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    // avec anticrénelage, les échantillons sont gardés : le post-traitement les
                    // résout lui-même (moyenne pondérée selon la luminosité, voir post.rs)
                    view: target.msaa.as_ref().unwrap_or(&target.post.hdr_view),
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r, g, b, a: 1.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    // profondeur inversée : 0 au loin (voir `camera::ViewParams::proj`)
                    view: &target.depth,
                    // gardée pour l'occlusion ambiante
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(0.0), store: wgpu::StoreOp::Store }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if let Some(sky) = self.sky.as_ref().filter(|_| draw_sky) {
                pass.set_pipeline(&self.sky_pipeline);
                pass.set_bind_group(0, sky, &[]);
                pass.set_bind_group(1, &self.sky_empty_bind, &[]);
                pass.set_bind_group(2, &self.globals_bind, &[]);
                pass.draw(0..3, 0..1);
                // astres, du plus lointain au plus proche
                if !self.satellites.is_empty() {
                    pass.set_pipeline(&self.sat_pipeline);
                    for s in &self.satellites {
                        pass.set_bind_group(1, &s.bind, &[]);
                        pass.draw(0..6, 0..1);
                    }
                }
            }
            pass.set_bind_group(0, &self.globals_bind, &[]);
            pass.set_pipeline(&self.mesh_pipeline);
            for it in items {
                let (Some(model), Some(ga)) = (self.models.get(&it.asset), self.actors.get(&it.id)) else { continue };
                pass.set_bind_group(1, &ga.bind, &[]);
                for m in model.meshes.iter().filter(|m| !m.forcefield) {
                    let (key, _, _) = m.resolve(it);
                    let Some(mat) = self.materials.get(&key) else { continue };
                    pass.set_bind_group(2, mat, &[]);
                    pass.set_vertex_buffer(0, m.vbuf.slice(..));
                    pass.set_index_buffer(m.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..m.index_count, 0, 0..1);
                }
            }
            if let Some(d) = self.decor.as_ref().filter(|_| self.decor_visible) {
                pass.set_pipeline(&self.decor_pipeline);
                pass.set_bind_group(1, &d.bind, &[]);
                for b in &d.batches {
                    let Some(model) = self.models.get(&b.asset) else { continue };
                    for m in model.meshes.iter().filter(|m| !m.forcefield) {
                        let Some(mat) = self.materials.get(&m.material_key(None, None)) else { continue };
                        pass.set_bind_group(2, mat, &[]);
                        pass.set_vertex_buffer(0, m.vbuf.slice(..));
                        pass.set_index_buffer(m.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                        pass.draw_indexed(0..m.index_count, 0, b.first..b.first + b.count);
                    }
                }
            }
            if char_shadow.is_some() && self.ground_shadow > 0.0 && !(self.decor_terrain && self.decor_active()) {
                pass.set_pipeline(&self.ground_pipeline);
                pass.draw(0..6, 0..1);
            }
            // champs de force : après tout l'opaque, testés en profondeur sans l'écrire
            pass.set_pipeline(&self.ff_mesh_pipeline);
            for it in items {
                let (Some(model), Some(ga)) = (self.models.get(&it.asset), self.actors.get(&it.id)) else { continue };
                if !model.meshes.iter().any(|m| m.forcefield) {
                    continue;
                }
                pass.set_bind_group(1, &ga.bind, &[]);
                for m in model.meshes.iter().filter(|m| m.forcefield) {
                    let (key, _, _) = m.resolve(it);
                    let Some(mat) = self.materials.get(&key) else { continue };
                    pass.set_bind_group(2, mat, &[]);
                    pass.set_vertex_buffer(0, m.vbuf.slice(..));
                    pass.set_index_buffer(m.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..m.index_count, 0, 0..1);
                }
            }
            if let Some(d) = self.decor.as_ref().filter(|_| self.decor_visible) {
                pass.set_pipeline(&self.ff_decor_pipeline);
                pass.set_bind_group(1, &d.bind, &[]);
                for b in &d.batches {
                    let Some(model) = self.models.get(&b.asset) else { continue };
                    for m in model.meshes.iter().filter(|m| m.forcefield) {
                        let Some(mat) = self.materials.get(&m.material_key(None, None)) else { continue };
                        pass.set_bind_group(2, mat, &[]);
                        pass.set_vertex_buffer(0, m.vbuf.slice(..));
                        pass.set_index_buffer(m.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                        pass.draw_indexed(0..m.index_count, 0, b.first..b.first + b.count);
                    }
                }
            }
            // particules d'ambiance : après l'opaque et les champs de force
            if !self.particles.is_empty() {
                pass.set_pipeline(&self.particle_pipeline);
                for (p, &n) in self.particles.iter().zip(&particle_counts) {
                    if n > 0 {
                        pass.set_bind_group(1, &p.bind, &[]);
                        pass.draw(0..6, 0..n);
                    }
                }
            }
            if n_lines > 0 {
                pass.set_vertex_buffer(0, self.line_buf.slice(..));
                if !lines.depth.is_empty() {
                    pass.set_pipeline(&self.line_depth_pipeline);
                    pass.draw(0..lines.depth.len() as u32, 0..1);
                }
                if !lines.overlay.is_empty() {
                    pass.set_pipeline(&self.line_overlay_pipeline);
                    let s = lines.depth.len() as u32;
                    pass.draw(s..s + lines.overlay.len() as u32, 0..1);
                }
            }
        }
        // occlusion ambiante, profondeur de champ, bloom, étalonnage, puis image finale ; le grain
        // change à chaque image de la timeline
        let mut ps = self.post_settings;
        ps.seed = (self.particle_time.max(0.0) * 1000.0).floor();
        target.post.run(&self.post, &self.queue, &mut enc, &target.resolve_view, view_proj, eye, &ps);
        self.queue.submit(Some(enc.finish()));
    }

    /// Cadre les cartes d'ombre. Personnages : au plus près de leurs os courants (skin × position
    /// de repos), ou de leur boîte englobante pour les modèles rigides, avec une marge pour
    /// l'épaisseur des maillages. Décor : une zone large autour d'eux.
    fn fit_shadows(&self, items: &[DrawItem]) -> (Option<ShadowFit>, Option<ShadowFit>) {
        let mut pts: Vec<Vec3> = Vec::new();
        for it in items {
            let Some(model) = self.models.get(&it.asset) else { continue };
            if !model.bind_bones.is_empty() && model.bind_bones.len() == it.skin.len() {
                pts.extend(it.skin.iter().zip(&model.bind_bones).map(|(m, p)| m.transform_point3(*p)));
            } else if let Some(m) = it.skin.first() {
                let (lo, hi) = model.bounds;
                for i in 0..8 {
                    let c = Vec3::new(
                        if i & 1 == 0 { lo.x } else { hi.x },
                        if i & 2 == 0 { lo.y } else { hi.y },
                        if i & 4 == 0 { lo.z } else { hi.z },
                    );
                    pts.push(m.transform_point3(c));
                }
            }
        }
        let chars = shadow_fit(self.light_dir, &pts, self.shadow_size, 0.0);
        // décor : carré autour des personnages (ou de l'origine), et plan proche reculé pour que les
        // bâtiments entre le soleil et la scène projettent leur ombre
        let Some(d) = self.decor.as_ref().filter(|_| self.decor_visible) else { return (chars, None) };
        let c = if pts.is_empty() { Vec3::ZERO } else { pts.iter().copied().sum::<Vec3>() / pts.len() as f32 };
        let r = self.decor_shadow_range.max(1.0);
        let (lo, hi) = d.bounds;
        let (x0, x1) = ((c.x - r).max(lo.x), (c.x + r).min(hi.x));
        let (z0, z1) = ((c.z - r).max(lo.z), (c.z + r).min(hi.z));
        if x0 < x1 && z0 < z1 {
            for k in 0..8 {
                pts.push(Vec3::new(
                    if k & 1 == 0 { x0 } else { x1 },
                    if k & 2 == 0 { c.y - 10.0 } else { c.y + 30.0 },
                    if k & 4 == 0 { z0 } else { z1 },
                ));
            }
        }
        (chars, shadow_fit(self.light_dir, &pts, self.shadow_size, 600.0))
    }

    pub fn create_target(&self, width: u32, height: u32) -> Target {
        Target::new(&self.device, width, height, self.samples, &self.post)
    }

    /// Image d'export (photo, image de vidéo) en RGBA8 sRGB, `width` × `height` : rendue
    /// `supersample` fois plus grande, réduite, puis rendue plus nette si le projet le demande.
    ///
    /// Au-delà de `EXPORT_TILE` pixels de côté, l'image suréchantillonnée est rendue par tuiles :
    /// chacune voit une part du cadre (projection décentrée) avec une marge tout autour, pour
    /// que l'occlusion, le bloom et la profondeur de champ aient leurs voisins ; seul son cœur
    /// est gardé. La taille n'est donc plus limitée par la carte graphique (photos 8K et plus,
    /// 4K suréchantillonnées 3×), et la mémoire reste celle d'une tuile. L'occlusion, dont la
    /// portée dépasse la marge sur un gros plan, lit hors de la tuile la profondeur de l'image
    /// entière rendue d'abord en réduit.
    #[allow(clippy::too_many_arguments)]
    pub fn render_image(
        &mut self,
        view_proj: Mat4,
        eye: Vec3,
        items: &[DrawItem],
        lines: &Lines,
        width: u32,
        height: u32,
        supersample: u32,
    ) -> Vec<u8> {
        let ss = supersample.clamp(1, 4);
        let (fw, fh) = (width * ss, height * ss);
        let tiled = fw > EXPORT_TILE || fh > EXPORT_TILE;
        let margin = if tiled { EXPORT_MARGIN } else { 0 };
        // cœur des tuiles : multiple du suréchantillonnage et de 64 (les niveaux du bloom, jusqu'à
        // 1/64, tombent alors sur la même grille d'une tuile à l'autre), au plus EXPORT_TILE avec
        // ses marges
        let align = if ss == 3 { 192 } else { 64 };
        let core = |full: u32| -> (u32, u32) {
            if !tiled {
                return (full, 1);
            }
            let room = (EXPORT_TILE - 2 * margin) / align * align;
            let n = full.div_ceil(room);
            (full.div_ceil(n).div_ceil(align) * align, n)
        };
        let ((cw, nx), (ch, ny)) = (core(fw), core(fh));
        let (tw, th) = (cw + 2 * margin, ch + 2 * margin);
        let target = match self.export_target.take() {
            Some(t) if t.width == tw && t.height == th && t.samples == self.samples => t,
            _ => self.create_target(tw, th),
        };
        let saved = (self.post_settings, self.render_ss);
        self.render_ss = ss as f32;
        // netteté après réduction (sur le processeur) quand l'image est suréchantillonnée
        self.post_settings.gpu_sharpen = ss == 1;
        self.post_settings.grain_cell = ss as f32;
        self.post_settings.max_coc = if tiled { margin as f32 } else { 256.0 };
        // occlusion d'une photo par tuiles : son rayon (0,3 m sur un personnage à 2 m) couvre bien
        // plus que la marge. On rend d'abord l'image entière réduite (au plus EXPORT_TILE de côté,
        // sans post-traitement) ; chaque tuile lit sa profondeur pour les points hors d'elle.
        let guide = (tiled && self.post_settings.ao > 0.0).then(|| {
            let k = fw.max(fh).div_ceil(EXPORT_TILE).max(1);
            let g = self.create_target(fw.div_ceil(k), fh.div_ceil(k));
            let keep = self.post_settings;
            self.post_settings.ao = 0.0;
            self.post_settings.bloom = 0.0;
            self.post_settings.fx.dof = false;
            self.render(Some(&g), view_proj, eye, items, lines);
            self.post_settings = keep;
            self.post_settings.ao_guide = Some((view_proj, [g.width, g.height]));
            g
        });
        let mut target = target;
        target.post.set_ao_guide(&self.device, &self.post, &target.depth, guide.as_ref().map(|g| &g.depth));
        let mut out = vec![0u8; (width * height * 4) as usize];
        for ty in 0..ny {
            for tx in 0..nx {
                // tuile : cœur [x0, x0 + cw) de l'image suréchantillonnée, marge comprise
                let (x0, y0) = (tx * cw, ty * ch);
                let (rx, ry) = (x0 as f32 - margin as f32, y0 as f32 - margin as f32);
                self.post_settings.frame = Some([rx, ry, fw as f32, fh as f32]);
                let vp = tile_projection(fw as f32, fh as f32, rx, ry, tw as f32, th as f32) * view_proj;
                self.render(Some(&target), vp, eye, items, lines);
                let tile = self.read_target(&target);
                // cœur de la tuile dans l'image (la dernière rangée et la dernière colonne débordent)
                let (kw, kh) = (cw.min(fw - x0), ch.min(fh - y0));
                let mut core_px = vec![0u8; (kw * kh * 4) as usize];
                for y in 0..kh {
                    let src = (((y + margin) * tw + margin) * 4) as usize;
                    let dst = (y * kw * 4) as usize;
                    core_px[dst..dst + (kw * 4) as usize].copy_from_slice(&tile[src..src + (kw * 4) as usize]);
                }
                let small = downsample(&core_px, kw, kh, ss);
                let (ow, oh, ox, oy) = (kw / ss, kh / ss, x0 / ss, y0 / ss);
                for y in 0..oh {
                    let src = (y * ow * 4) as usize;
                    let dst = (((oy + y) * width + ox) * 4) as usize;
                    out[dst..dst + (ow * 4) as usize].copy_from_slice(&small[src..src + (ow * 4) as usize]);
                }
            }
        }
        let sharpen = self.post_settings.fx.sharpen;
        (self.post_settings, self.render_ss) = saved;
        if guide.is_some() {
            target.post.set_ao_guide(&self.device, &self.post, &target.depth, None);
        }
        self.export_target = Some(target);
        if ss > 1 {
            crate::post::sharpen(&mut out, width, height, sharpen);
        }
        out
    }

    /// Libère la cible des exports (après une photo, ou à la fin d'une vidéo).
    pub fn release_export_target(&mut self) {
        self.export_target = None;
    }

    /// Relit une cible en RGBA8 (pour l'export PNG).
    pub fn read_target(&self, t: &Target) -> Vec<u8> {
        let row = 4 * t.width;
        let padded = row.div_ceil(256) * 256;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (padded * t.height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("readback") });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &t.resolve,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(t.height) },
            },
            wgpu::Extent3d { width: t.width, height: t.height, depth_or_array_layers: 1 },
        );
        self.queue.submit(Some(enc.finish()));
        let slice = buf.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.device.poll(wgpu::Maintain::Wait);
        let data = slice.get_mapped_range();
        let mut out = Vec::with_capacity((row * t.height) as usize);
        for y in 0..t.height {
            let s = (y * padded) as usize;
            out.extend_from_slice(&data[s..s + row as usize]);
        }
        drop(data);
        buf.unmap();
        out
    }
}

/// Densité de l'air pour les rayons du soleil, à la force 1 (par m). Avec une diffusion très
/// tournée vers l'avant ([`SUN_RAYS_ANISOTROPY`]), face au soleil, 60 m d'air éclairé ajoutent
/// environ la moitié de la lumière du soleil ; de côté ou dos au soleil, presque rien (sinon tout
/// terrain dégagé se couvre d'un voile laiteux).
const SUN_RAYS_DENSITY: f32 = 0.0006;
/// Anisotropie de Henyey-Greenstein des rayons du soleil (0 : même diffusion partout).
const SUN_RAYS_ANISOTROPY: f32 = 0.7;

/// Côté maximal d'une cible d'export avant de passer aux tuiles (pixels).
const EXPORT_TILE: u32 = 4096;
/// Marge autour du cœur d'une tuile (pixels de rendu) : portée de l'occlusion, du bloom et de
/// la profondeur de champ.
const EXPORT_MARGIN: u32 = 256;

/// Matrice qui ramène le rectangle (`x`, `y`, `w`, `h`) d'une image de `full_w` × `full_h`
/// pixels (origine en haut à gauche) au cadre entier : à composer avant la projection.
fn tile_projection(full_w: f32, full_h: f32, x: f32, y: f32, w: f32, h: f32) -> Mat4 {
    let (sx, sy) = (full_w / w, full_h / h);
    // centre du rectangle en coordonnées normalisées du cadre (y vers le haut)
    let cx = (x + w * 0.5) / full_w * 2.0 - 1.0;
    let cy = 1.0 - (y + h * 0.5) / full_h * 2.0;
    Mat4::from_cols(
        glam::Vec4::new(sx, 0.0, 0.0, 0.0),
        glam::Vec4::new(0.0, sy, 0.0, 0.0),
        glam::Vec4::new(0.0, 0.0, 1.0, 0.0),
        glam::Vec4::new(-sx * cx, -sy * cy, 0.0, 1.0),
    )
}

/// Projection du soleil cadrée sur une scène.
struct ShadowFit {
    light_vp: Mat4,
    /// taille d'un texel de la carte d'ombre (m)
    texel: f32,
    center: Vec3,
}

/// Projection orthographique vue du soleil qui contient tous les points (avec une marge de
/// 40 cm pour l'épaisseur des corps autour des os). Le plan lointain va bien au-delà, pour que le
/// sol et tout ce qui est derrière reçoivent l'ombre.
fn shadow_fit(light_dir: Vec3, pts: &[Vec3], size: u32, extra_near: f32) -> Option<ShadowFit> {
    if pts.is_empty() {
        return None;
    }
    const MARGIN: f32 = 0.4;
    let d = light_dir.try_normalize()?;
    let center = pts.iter().copied().sum::<Vec3>() / pts.len() as f32;
    let up = if d.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
    let view = Mat4::look_at_rh(center - d * 100.0, center, up);
    let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for p in pts {
        let q = view.transform_point3(*p);
        lo = lo.min(q);
        hi = hi.max(q);
    }
    // carré, pour des texels isotropes
    let half = ((hi.x - lo.x).max(hi.y - lo.y) * 0.5 + MARGIN).max(0.5);
    let (cx, cy) = ((lo.x + hi.x) * 0.5, (lo.y + hi.y) * 0.5);
    // vue RH : on regarde vers -z ; les points sont à -z entre -hi.z et -lo.z
    // `extra_near` recule le plan proche (le décor devant le soleil projette son ombre) ; en
    // orthographique, un plan proche négatif reste valable
    let near = if extra_near > 0.0 { -hi.z - MARGIN - extra_near } else { (-hi.z - MARGIN).max(0.01) };
    let far = -lo.z + 200.0;
    let proj = Mat4::orthographic_rh(cx - half, cx + half, cy - half, cy + half, near, far);
    Some(ShadowFit { light_vp: proj * view, texel: 2.0 * half / size as f32, center })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// La réduction moyenne en lumière linéaire : noir + blanc donne le gris moyen sRGB (188).
    #[test]
    fn tile_projection_maps_rectangle_to_frame() {
        // le quart en haut à droite d'une image 200 × 100
        let m = tile_projection(200.0, 100.0, 100.0, 0.0, 100.0, 50.0);
        // coin haut droit du cadre : reste le coin haut droit de la tuile
        let p = m * glam::Vec4::new(1.0, 1.0, 0.5, 1.0);
        assert!((p.x - 1.0).abs() < 1e-5 && (p.y - 1.0).abs() < 1e-5);
        // centre du cadre : coin bas gauche de la tuile
        let p = m * glam::Vec4::new(0.0, 0.0, 0.5, 1.0);
        assert!((p.x + 1.0).abs() < 1e-5 && (p.y + 1.0).abs() < 1e-5);
        // la profondeur et w ne changent pas (en coordonnées homogènes)
        let p = m * glam::Vec4::new(0.0, 0.0, 0.3, 2.0);
        assert!((p.z - 0.3).abs() < 1e-6 && (p.w - 2.0).abs() < 1e-6 && (p.x + 2.0).abs() < 1e-5);
    }

    #[test]
    fn downsample_is_linear() {
        let img = [0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 255];
        let out = downsample(&img, 2, 2, 2);
        assert_eq!(out.len(), 4);
        assert!((out[0] as i32 - 188).abs() <= 1, "{}", out[0]);
        assert_eq!(downsample(&img, 2, 2, 1), img.to_vec());
    }

    /// Tous les points cadrés tombent dans la carte, en profondeur 0..1.
    #[test]
    fn shadow_fit_contains_points() {
        let pts = [Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.3, 1.8, 0.2), Vec3::new(-2.0, 1.0, 5.0)];
        let f = shadow_fit(Vec3::new(-0.4, -0.8, -0.45), &pts, 4096, 0.0).unwrap();
        for p in pts {
            let c = f.light_vp.project_point3(p);
            assert!(c.x.abs() < 1.0 && c.y.abs() < 1.0 && c.z > 0.0 && c.z < 1.0, "{p} -> {c}");
        }
        // un point du sol plus loin du soleil reste devant le plan lointain
        let c = f.light_vp.project_point3(Vec3::new(1.0, 0.0, 1.0));
        assert!(c.z > 0.0 && c.z < 1.0);
        assert!(shadow_fit(Vec3::ZERO, &pts, 4096, 0.0).is_none());
    }
}

/// Réduit une image RGBA8 sRGB d'un facteur entier (moyenne en lumière linéaire), pour le
/// suréchantillonnage des exports.
pub fn downsample(rgba: &[u8], width: u32, height: u32, factor: u32) -> Vec<u8> {
    if factor <= 1 {
        return rgba.to_vec();
    }
    let lut: Vec<f32> = (0..256)
        .map(|i| {
            let c = i as f32 / 255.0;
            if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        })
        .collect();
    let enc = |c: f32| {
        let c = c.clamp(0.0, 1.0);
        let s = if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
        (s * 255.0).round() as u8
    };
    let (w, h, f) = (width as usize, height as usize, factor as usize);
    let (ow, oh) = (w / f, h / f);
    let n = (f * f) as f32;
    let mut out = vec![0u8; ow * oh * 4];
    for y in 0..oh {
        for x in 0..ow {
            let mut acc = [0.0f32; 4];
            for sy in 0..f {
                for sx in 0..f {
                    let i = ((y * f + sy) * w + x * f + sx) * 4;
                    for k in 0..3 {
                        acc[k] += lut[rgba[i + k] as usize];
                    }
                    acc[3] += rgba[i + 3] as f32;
                }
            }
            let o = (y * ow + x) * 4;
            for k in 0..3 {
                out[o + k] = enc(acc[k] / n);
            }
            out[o + 3] = (acc[3] / n).round() as u8;
        }
    }
    out
}
