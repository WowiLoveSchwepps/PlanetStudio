//! Post-traitement : occlusion ambiante (SSAO), profondeur de champ, bloom, étalonnage, FXAA
//! et netteté.
//!
//! La scène est dessinée dans une image HDR (`Rgba16Float`) : les zones lumineuses peuvent
//! dépasser 1, ce que le bloom exploite. Ensuite :
//! 1. occlusion ambiante, à demi-résolution, à partir de la seule profondeur : chaque point
//!    regarde 12 points de son hémisphère ; ceux qui sont cachés par la géométrie l'assombrissent ;
//! 2. profondeur de champ (au choix) : flou selon la distance au plan de mise au point ;
//! 3. bloom : extraction de ce qui dépasse un seuil, réductions successives (filtre « dual
//!    Kawase »), puis remontée en additionnant les niveaux ;
//! 4. assemblage : couleur × occlusion (adoucie) + bloom, puis étalonnage (exposition, balance
//!    des blancs, rendu des tons, contraste, saturation, vignettage, grain), vers l'image sRGB ;
//! 5. FXAA puis netteté (CAS), au choix, sur l'image finale.
//!
//! Avec le MSAA, les échantillons sont d'abord résolus par une moyenne pondérée par l'inverse
//! de leur luminosité : une moyenne simple en HDR, écrêtée ensuite à 1, laisse un bord brillant
//! (ciel, émissif, reflet du soleil) aussi crénelé que sans anticrénelage.
//!
//! Une photo très grande est rendue par tuiles (voir `Renderer::render_image`) : le vignettage et
//! le grain se calculent alors sur l'image entière (`PostSettings::frame`).

use eframe::wgpu;
use eframe::wgpu::util::DeviceExt;
use glam::{Mat4, Vec3};
use serde::{Deserialize, Serialize};

pub const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const AO_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// au plus 6 niveaux de bloom (de 1/2 à 1/64)
const BLOOM_LEVELS: usize = 6;
/// échantillons de la profondeur de champ par pixel
const DOF_SAMPLES: f32 = 48.0;
/// taille d'une table d'étalonnage du jeu : 16 tranches de 16 × 16 côte à côte
const LUT_W: u32 = 256;
const LUT_H: u32 = 16;
/// point blanc du jeu en plein jour (Indar à 13 h), sans ciel de carte
pub const DEFAULT_WHITE_POINT: f32 = 11.3;

/// Table d'étalonnage neutre (même disposition que les `colorkey` du jeu).
pub fn identity_lut() -> Vec<u8> {
    let mut out = Vec::with_capacity((LUT_W * LUT_H * 4) as usize);
    let v = |i: u32| (i * 255 / 15) as u8;
    for g in 0..LUT_H {
        for x in 0..LUT_W {
            out.extend_from_slice(&[v(x % 16), v(g), v(x / 16), 255]);
        }
    }
    out
}

const FULLSCREEN: &str = r#"
@vertex
fn vs_full(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}
"#;

const POST_UNIFORM: &str = r#"
struct PostU {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    eye: vec4<f32>,
    // x force de l'occlusion, y rayon (m), z force du bloom, w seuil du bloom
    params: vec4<f32>,
    // x, y : taille pleine résolution ; z, w : taille de l'occlusion
    size: vec4<f32>,
    // image entière : origine de cette cible (pixels de rendu), taille de l'image entière
    frame: vec4<f32>,
    // x exposition (facteur), y contraste, z saturation, w rendu des tons (0 écrêtage, 1 ACES, 2 AgX)
    grade: vec4<f32>,
    // rgb balance des blancs, w vignettage
    grade2: vec4<f32>,
    // x grain, y taille d'un grain (pixels de rendu), z graine aléatoire, w netteté (CAS)
    grade3: vec4<f32>,
    // x profondeur de champ active, y distance de mise au point (m), z flou de l'arrière-plan
    // lointain (pixels), w nombre d'échantillons
    dof: vec4<f32>,
    // image entière réduite (photo par tuiles) : sa projection et son inverse ; guide : x active,
    // y, z sa taille. L'occlusion y cherche les points qui tombent hors de la tuile.
    full_vp: mat4x4<f32>,
    full_inv: mat4x4<f32>,
    guide: vec4<f32>,
    // étalonnage du jeu : x actif, y part de la seconde table, z point blanc, w tables présentes
    game: vec4<f32>,
};
@group(0) @binding(0) var<uniform> pu: PostU;
"#;

/// Occlusion ambiante ; `DEPTH_TYPE` est remplacé selon l'anticrénelage.
const SSAO_SHADER: &str = r#"
@group(0) @binding(1) var t_depth: DEPTH_TYPE;
// profondeur de l'image entière réduite (photo par tuiles), sinon la même que `t_depth`
@group(0) @binding(2) var t_guide: DEPTH_TYPE;

fn world_at(pix: vec2<i32>) -> vec4<f32> {
    let p = clamp(pix, vec2<i32>(0), vec2<i32>(pu.size.xy) - 1);
    let d = textureLoad(t_depth, p, 0);
    let uv = (vec2<f32>(p) + 0.5) / pu.size.xy;
    let w = pu.inv_view_proj * vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, d, 1.0);
    return vec4<f32>(w.xyz / w.w, d);
}

// point de l'image entière réduite, `uv` dans le cadre entier
fn guide_at(uv: vec2<f32>) -> vec4<f32> {
    let size = pu.guide.yz;
    let p = clamp(vec2<i32>(uv * size), vec2<i32>(0), vec2<i32>(size) - 1);
    let d = textureLoad(t_guide, p, 0);
    let c = (vec2<f32>(p) + 0.5) / size;
    let w = pu.full_inv * vec4<f32>(c.x * 2.0 - 1.0, 1.0 - c.y * 2.0, d, 1.0);
    return vec4<f32>(w.xyz / w.w, d);
}

@fragment
fn fs_ssao(@builtin(position) fc: vec4<f32>) -> @location(0) vec4<f32> {
    let pix = vec2<i32>(fc.xy * (pu.size.xy / pu.size.zw));
    let c = world_at(pix);
    // (profondeur inversée : 0 = fond, rien de dessiné)
    if (c.w <= 0.0) { return vec4<f32>(1.0); }
    let p = c.xyz;
    // normale à partir des voisins, du côté le plus proche en profondeur (évite les silhouettes)
    let step = max(i32(pu.size.x / pu.size.z), 1);
    let r = world_at(pix + vec2<i32>(step, 0));
    let l = world_at(pix - vec2<i32>(step, 0));
    let d = world_at(pix + vec2<i32>(0, step));
    let u = world_at(pix - vec2<i32>(0, step));
    var dx = r.xyz - p;
    if (abs(l.w - c.w) < abs(r.w - c.w)) { dx = p - l.xyz; }
    var dy = d.xyz - p;
    if (abs(u.w - c.w) < abs(d.w - c.w)) { dy = p - u.xyz; }
    var n = normalize(cross(dy, dx));
    let to_eye = pu.eye.xyz - p;
    if (dot(n, to_eye) < 0.0) { n = -n; }
    // repère autour de la normale, tourné d'un angle pseudo-aléatoire par pixel
    var t1 = normalize(cross(n, vec3<f32>(0.0, 1.0, 0.0)));
    if (abs(n.y) > 0.99) { t1 = normalize(cross(n, vec3<f32>(1.0, 0.0, 0.0))); }
    let t2 = cross(n, t1);
    // bruit calé sur l'image entière (le même d'une tuile à l'autre)
    let gfc = fc.xy + pu.frame.xy * (pu.size.zw / pu.size.xy);
    let noise = fract(52.9829189 * fract(dot(gfc, vec2<f32>(0.06711056, 0.00583715))));
    let radius = pu.params.y;
    let eye_dist = length(to_eye);
    var occ = 0.0;
    for (var i = 0; i < 12; i++) {
        let t = (f32(i) + 0.5) / 12.0;
        let a = f32(i) * 2.399963 + noise * 6.2831853;
        // hémisphère plutôt rasant : les creux se détectent surtout sur les côtés
        let h = 0.1 + 0.9 * fract(f32(i) * 0.618034 + noise);
        let dir = normalize(t1 * cos(a) + t2 * sin(a) + n * h);
        let s = p + dir * radius * mix(0.12, 1.0, t * t);
        let clip = pu.view_proj * vec4<f32>(s, 1.0);
        if (clip.w <= 0.0) { continue; }
        let suv = vec2<f32>(clip.x / clip.w * 0.5 + 0.5, 0.5 - clip.y / clip.w * 0.5);
        var q: vec4<f32>;
        if (any(suv < vec2<f32>(0.0)) || any(suv > vec2<f32>(1.0))) {
            // hors de la tuile : l'image entière réduite (sans elle, le bord d'une tuile perd
            // les occulteurs de sa voisine et s'éclaircit d'un coup)
            if (pu.guide.x < 0.5) { continue; }
            let fc = pu.full_vp * vec4<f32>(s, 1.0);
            let fuv = vec2<f32>(fc.x / fc.w * 0.5 + 0.5, 0.5 - fc.y / fc.w * 0.5);
            if (any(fuv < vec2<f32>(0.0)) || any(fuv > vec2<f32>(1.0))) { continue; }
            q = guide_at(fuv);
        } else {
            q = world_at(vec2<i32>(suv * pu.size.xy));
        }
        if (q.w <= 0.0) { continue; }
        // le point de l'hémisphère est caché par la géométrie, à portée du rayon
        if (length(pu.eye.xyz - q.xyz) < length(pu.eye.xyz - s) - 0.02 * eye_dist * 0.1) {
            occ += smoothstep(0.0, 1.0, radius / max(distance(p, q.xyz), 1e-4));
        }
    }
    // la moitié des points cachés suffit pour un creux bien marqué
    let ao = clamp(1.0 - occ / 6.0, 0.0, 1.0);
    return vec4<f32>(ao, ao, ao, 1.0);
}
"#;

/// Occlusion adoucie : 4 lectures filtrées autour du point (16 texels d'occlusion).
const AO_READ: &str = r#"
fn ao_at(uv: vec2<f32>) -> f32 {
    let o = 0.75 / pu.size.zw;
    var ao = textureSampleLevel(t_ao, s_lin, uv + vec2<f32>(-o.x, -o.y), 0.0).r;
    ao += textureSampleLevel(t_ao, s_lin, uv + vec2<f32>(o.x, -o.y), 0.0).r;
    ao += textureSampleLevel(t_ao, s_lin, uv + vec2<f32>(-o.x, o.y), 0.0).r;
    ao += textureSampleLevel(t_ao, s_lin, uv + vec2<f32>(o.x, o.y), 0.0).r;
    return mix(1.0, ao * 0.25, pu.params.x);
}

// couleur occultée : une source de lumière (néon, LED, reflet vif, au-delà de 1) n'est pas
// assombrie par les creux qui l'entourent (une LED au fond d'une visière restait terne)
fn occluded(c: vec3<f32>, uv: vec2<f32>) -> vec3<f32> {
    let k = smoothstep(1.0, 2.5, max(c.r, max(c.g, c.b)));
    return c * mix(ao_at(uv), 1.0, k);
}
"#;

/// Profondeur de champ : rayon du cercle de confusion d'une lentille mince, proportionnel à
/// |1 − mise au point / distance| (0 sur le plan net, le maximum réglé à l'infini, plus devant).
/// Chaque pixel moyenne des points d'une spirale tournée au hasard dans son propre cercle ; un
/// point plus net que la distance qui le sépare du pixel n'y participe pas (un personnage net ne
/// bave pas sur le fond flou). L'occlusion est appliquée ici, avant le flou.
/// `DEPTH_TYPE` est remplacé selon l'anticrénelage.
const DOF_SHADER: &str = r#"
@group(0) @binding(1) var t_hdr: texture_2d<f32>;
@group(0) @binding(2) var t_ao: texture_2d<f32>;
@group(0) @binding(3) var t_depth: DEPTH_TYPE;
@group(0) @binding(4) var s_lin: sampler;

fn clamp_pix(p: vec2<i32>) -> vec2<i32> {
    return clamp(p, vec2<i32>(0), vec2<i32>(pu.size.xy) - 1);
}

fn coc_of(p: vec2<i32>, d: f32) -> f32 {
    var dist = 1e6;
    if (d > 0.0) {
        let uv = (vec2<f32>(p) + 0.5) / pu.size.xy;
        let w = pu.inv_view_proj * vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, d, 1.0);
        dist = max(length(w.xyz / w.w - pu.eye.xyz), 1e-3);
    }
    return min(pu.dof.z * abs(1.0 - pu.dof.y / dist), pu.dof.z * 1.5);
}

// cercle le plus net parmi les échantillons du pixel (`DEPTH_SAMPLES` : ceux du MSAA). Un pixel
// du contour mêle le personnage net et le fond : il garde sa couleur anticrénelée, et le fond
// flou ne lui emprunte rien. Avec le seul premier échantillon, chaque pixel du bord était
// classé tout net ou tout flou : contour en escalier.
fn coc_at(p: vec2<i32>) -> f32 {
    var c = 1e6;
    for (var s = 0; s < DEPTH_SAMPLES; s++) {
        c = min(c, coc_of(p, textureLoad(t_depth, p, s)));
    }
    return c;
}

fn color_at(p: vec2<i32>) -> vec3<f32> {
    let uv = (vec2<f32>(p) + 0.5) / pu.size.xy;
    return occluded(textureLoad(t_hdr, p, 0).rgb, uv);
}

// couleur lue entre les pixels (filtrage bilinéaire) : des points arrondis au pixel près
// dessinaient un damier dans les zones floues et les reflets
fn color_lin(pos: vec2<f32>) -> vec3<f32> {
    let uv = clamp(pos, vec2<f32>(0.5), pu.size.xy - 0.5) / pu.size.xy;
    return occluded(textureSampleLevel(t_hdr, s_lin, uv, 0.0).rgb, uv);
}

@fragment
fn fs_dof(@builtin(position) fc: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<i32>(fc.xy);
    let c0 = coc_at(p);
    let center = color_at(p);
    // (alpha : rayon du cercle, lu par le lissage)
    if (c0 < 0.5) { return vec4<f32>(center, c0); }
    let n = i32(pu.dof.w);
    let rot = fract(52.9829189 * fract(dot(fc.xy, vec2<f32>(0.06711056, 0.00583715)))) * 6.2831853;
    var sum = center;
    var wsum = 1.0;
    for (var i = 0; i < n; i++) {
        let t = (f32(i) + 0.5) / f32(n);
        let r = sqrt(t) * c0;
        let a = f32(i) * 2.399963 + rot;
        let off = vec2<f32>(cos(a), sin(a)) * r;
        let q = clamp_pix(p + vec2<i32>(round(off)));
        let w = clamp(coc_at(q) - r + 1.0, 0.0, 1.0);
        sum += color_lin(fc.xy + off) * w;
        wsum += w;
    }
    return vec4<f32>(sum / wsum, c0);
}

// Lissage du flou : les points de la spirale sont espacés d'environ √(π / n) × rayon (8 pixels
// pour un rayon de 30). Une petite lumière du fond n'est touchée que par certains d'entre eux,
// différents d'un pixel à l'autre : grain en damier. Chaque pixel flou est moyenné avec ses
// voisins aussi flous, dans un disque d'un demi-écart. Un voisin net (le personnage) n'y entre
// pas : le contour reste intact. `t_hdr` est ici l'image floutée, son alpha le rayon du cercle.
@fragment
fn fs_dof_smooth(@builtin(position) fc: vec4<f32>) -> @location(0) vec4<f32> {
    let c = textureLoad(t_hdr, vec2<i32>(fc.xy), 0);
    let c0 = c.a;
    if (c0 < 2.0) { return vec4<f32>(c.rgb, 1.0); }
    let k = clamp(c0 * sqrt(3.14159265 / pu.dof.w) * 0.5, 1.0, 6.0);
    var sum = c.rgb;
    var wsum = 1.0;
    for (var i = 0; i < 16; i++) {
        let r = sqrt((f32(i) + 0.5) / 16.0) * k;
        let a = f32(i) * 2.399963;
        let pos = clamp(fc.xy + vec2<f32>(cos(a), sin(a)) * r, vec2<f32>(0.5), pu.size.xy - 0.5);
        let s = textureSampleLevel(t_hdr, s_lin, pos / pu.size.xy, 0.0);
        let w = smoothstep(0.5, 0.8, s.a / c0);
        sum += s.rgb * w;
        wsum += w;
    }
    return vec4<f32>(sum / wsum, 1.0);
}
"#;

const BLOOM_SHADER: &str = r#"
struct LevelU {
    // xy : texel de la source ; z : seuil ; w : genou
    src: vec4<f32>,
    // xy : texel de la destination
    dst: vec4<f32>,
};
@group(0) @binding(0) var<uniform> lv: LevelU;
@group(0) @binding(1) var t_src: texture_2d<f32>;
@group(0) @binding(2) var s_lin: sampler;

fn down(uv: vec2<f32>) -> vec3<f32> {
    let o = lv.src.xy;
    var c = textureSample(t_src, s_lin, uv).rgb * 4.0;
    c += textureSample(t_src, s_lin, uv + vec2<f32>(-o.x, -o.y)).rgb;
    c += textureSample(t_src, s_lin, uv + vec2<f32>(o.x, -o.y)).rgb;
    c += textureSample(t_src, s_lin, uv + vec2<f32>(-o.x, o.y)).rgb;
    c += textureSample(t_src, s_lin, uv + vec2<f32>(o.x, o.y)).rgb;
    return c / 8.0;
}

// extraction de ce qui dépasse le seuil (genou progressif), puis première réduction
@fragment
fn fs_prefilter(@builtin(position) fc: vec4<f32>) -> @location(0) vec4<f32> {
    // plafond bas : le disque du soleil (très au-delà de 1) ne doit pas noyer l'image dans un halo blanc
    let c = min(down(fc.xy * lv.dst.xy), vec3<f32>(12.0));
    let b = max(c.r, max(c.g, c.b));
    let thr = lv.src.z;
    let knee = lv.src.w;
    var soft = clamp(b - thr + knee, 0.0, 2.0 * knee);
    soft = soft * soft / (4.0 * knee + 1e-4);
    let w = max(soft, b - thr) / max(b, 1e-4);
    return vec4<f32>(c * max(w, 0.0), 1.0);
}

@fragment
fn fs_down(@builtin(position) fc: vec4<f32>) -> @location(0) vec4<f32> {
    return vec4<f32>(down(fc.xy * lv.dst.xy), 1.0);
}

// remontée (additionnée au niveau de destination par le mélange)
@fragment
fn fs_up(@builtin(position) fc: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = fc.xy * lv.dst.xy;
    let o = lv.src.xy;
    var c = textureSample(t_src, s_lin, uv + vec2<f32>(-o.x * 2.0, 0.0)).rgb;
    c += textureSample(t_src, s_lin, uv + vec2<f32>(o.x * 2.0, 0.0)).rgb;
    c += textureSample(t_src, s_lin, uv + vec2<f32>(0.0, -o.y * 2.0)).rgb;
    c += textureSample(t_src, s_lin, uv + vec2<f32>(0.0, o.y * 2.0)).rgb;
    c += textureSample(t_src, s_lin, uv + vec2<f32>(-o.x, -o.y)).rgb * 2.0;
    c += textureSample(t_src, s_lin, uv + vec2<f32>(o.x, -o.y)).rgb * 2.0;
    c += textureSample(t_src, s_lin, uv + vec2<f32>(-o.x, o.y)).rgb * 2.0;
    c += textureSample(t_src, s_lin, uv + vec2<f32>(o.x, o.y)).rgb * 2.0;
    return vec4<f32>(c / 12.0, 1.0);
}
"#;

/// Résolution du MSAA : échantillons pondérés par 1 / (1 + luminosité) (Karis).
const RESOLVE_SHADER: &str = r#"
@group(0) @binding(0) var t_ms: texture_multisampled_2d<f32>;

@fragment
fn fs_resolve(@builtin(position) fc: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<i32>(fc.xy);
    let n = i32(textureNumSamples(t_ms));
    var sum = vec3<f32>(0.0);
    var wsum = 0.0;
    for (var s = 0; s < n; s++) {
        let c = textureLoad(t_ms, p, s).rgb;
        let w = 1.0 / (1.0 + max(c.r, max(c.g, c.b)));
        sum += c * w;
        wsum += w;
    }
    return vec4<f32>(sum / max(wsum, 1e-6), 1.0);
}
"#;

/// Image finale sRGB lue par le FXAA et la netteté.
const LDR_INPUT: &str = r#"
@group(0) @binding(1) var t_ldr: texture_2d<f32>;
@group(0) @binding(2) var s_lin: sampler;
"#;

/// FXAA (d'après Timothy Lottes) sur l'image finale : cherche la direction du contour à partir
/// de la luminosité des voisins, puis mélange le long de ce contour. Les zones peu contrastées
/// (textures) ne sont pas touchées.
const FXAA_SHADER: &str = r#"
const SPAN_MAX: f32 = 8.0;
const REDUCE_MUL: f32 = 0.125;
const REDUCE_MIN: f32 = 0.0078125;

// luminosité perceptive (l'image lue est linéaire)
fn lum(c: vec3<f32>) -> f32 {
    return sqrt(dot(c, vec3<f32>(0.299, 0.587, 0.114)));
}

@fragment
fn fs_fxaa(@builtin(position) fc: vec4<f32>) -> @location(0) vec4<f32> {
    let inv = 1.0 / pu.size.xy;
    let uv = fc.xy * inv;
    let m = textureSampleLevel(t_ldr, s_lin, uv, 0.0).rgb;
    let nw = lum(textureSampleLevel(t_ldr, s_lin, uv + vec2<f32>(-1.0, -1.0) * inv, 0.0).rgb);
    let ne = lum(textureSampleLevel(t_ldr, s_lin, uv + vec2<f32>(1.0, -1.0) * inv, 0.0).rgb);
    let sw = lum(textureSampleLevel(t_ldr, s_lin, uv + vec2<f32>(-1.0, 1.0) * inv, 0.0).rgb);
    let se = lum(textureSampleLevel(t_ldr, s_lin, uv + vec2<f32>(1.0, 1.0) * inv, 0.0).rgb);
    let lm = lum(m);
    let lo = min(lm, min(min(nw, ne), min(sw, se)));
    let hi = max(lm, max(max(nw, ne), max(sw, se)));
    // contraste trop faible : pas un contour
    if (hi - lo < max(0.0312, hi * 0.125)) { return vec4<f32>(m, 1.0); }
    var dir = vec2<f32>(-((nw + ne) - (sw + se)), (nw + sw) - (ne + se));
    let reduce = max((nw + ne + sw + se) * (0.25 * REDUCE_MUL), REDUCE_MIN);
    let rcp_min = 1.0 / (min(abs(dir.x), abs(dir.y)) + reduce);
    dir = clamp(dir * rcp_min, vec2<f32>(-SPAN_MAX), vec2<f32>(SPAN_MAX)) * inv;
    let a = 0.5 * (textureSampleLevel(t_ldr, s_lin, uv + dir * (1.0 / 3.0 - 0.5), 0.0).rgb
        + textureSampleLevel(t_ldr, s_lin, uv + dir * (2.0 / 3.0 - 0.5), 0.0).rgb);
    let b = a * 0.5 + 0.25 * (textureSampleLevel(t_ldr, s_lin, uv - dir * 0.5, 0.0).rgb
        + textureSampleLevel(t_ldr, s_lin, uv + dir * 0.5, 0.0).rgb);
    let lb = lum(b);
    if (lb < lo || lb > hi) { return vec4<f32>(a, 1.0); }
    return vec4<f32>(b, 1.0);
}
"#;

/// Netteté adaptative au contraste (d'après AMD FidelityFX CAS) : renforce l'écart d'un pixel à
/// ses 4 voisins, moins là où le contraste local est déjà fort (pas de halo sur les contours).
/// Même calcul que `sharpen` (côté processeur, pour les exports suréchantillonnés).
const CAS_SHADER: &str = r#"
fn ld(p: vec2<i32>) -> vec3<f32> {
    return textureLoad(t_ldr, clamp(p, vec2<i32>(0), vec2<i32>(pu.size.xy) - 1), 0).rgb;
}

@fragment
fn fs_cas(@builtin(position) fc: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<i32>(fc.xy);
    let b = ld(p + vec2<i32>(0, -1));
    let d = ld(p + vec2<i32>(-1, 0));
    let e = ld(p);
    let f = ld(p + vec2<i32>(1, 0));
    let h = ld(p + vec2<i32>(0, 1));
    let mn = min(min(min(d, e), min(f, b)), h);
    let mx = max(max(max(d, e), max(f, b)), h);
    let amp = sqrt(clamp(min(mn, 1.0 - mx) / max(mx, vec3<f32>(1e-4)), vec3<f32>(0.0), vec3<f32>(1.0)));
    let w = amp * (-1.0 / mix(8.0, 5.0, clamp(pu.grade3.w, 0.0, 1.0)));
    let o = (b * w + d * w + f * w + h * w + e) / (1.0 + 4.0 * w);
    return vec4<f32>(clamp(o, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
"#;

const COMPOSITE_SHADER: &str = r#"
@group(0) @binding(1) var t_hdr: texture_2d<f32>;
@group(0) @binding(2) var t_bloom: texture_2d<f32>;
@group(0) @binding(3) var t_ao: texture_2d<f32>;
@group(0) @binding(4) var s_lin: sampler;
// tables d'étalonnage du jeu (`colorkey`) : deux tables de 256 × 16 l'une sous l'autre
@group(0) @binding(5) var t_lut: texture_2d<f32>;

fn to_srgb(c: vec3<f32>) -> vec3<f32> {
    let c1 = clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
    return select(1.055 * pow(c1, vec3<f32>(1.0 / 2.4)) - 0.055, c1 * 12.92, c1 <= vec3<f32>(0.0031308));
}
fn from_srgb(c: vec3<f32>) -> vec3<f32> {
    return select(pow((c + 0.055) / 1.055, vec3<f32>(2.4)), c / 12.92, c <= vec3<f32>(0.04045));
}

// une table (`row` : 0 ou 1) lue en trilinéaire : bilinéaire dans les tranches de bleu qui
// encadrent la valeur, puis mélange des deux
fn lut_read(s: vec3<f32>, row: f32) -> vec3<f32> {
    let v = clamp(s, vec3<f32>(0.0), vec3<f32>(1.0)) * 15.0;
    let b0 = floor(v.b);
    let b1 = min(b0 + 1.0, 15.0);
    let y = (v.g + 0.5 + row * 16.0) / 32.0;
    let c0 = textureSampleLevel(t_lut, s_lin, vec2<f32>((b0 * 16.0 + v.r + 0.5) / 256.0, y), 0.0).rgb;
    let c1 = textureSampleLevel(t_lut, s_lin, vec2<f32>((b1 * 16.0 + v.r + 0.5) / 256.0, y), 0.0).rgb;
    return mix(c0, c1, v.b - b0);
}

// épaule douce : identité jusqu'à 0,6, puis approche exponentielle du blanc (dérivée continue).
// Les ombres et les tons moyens ne bougent pas : seules les lumières fortes sont adoucies.
fn soft_shoulder(c: vec3<f32>) -> vec3<f32> {
    let t = 0.6;
    let x = max(c - t, vec3<f32>(0.0));
    return min(c, vec3<f32>(t)) + (1.0 - t) * (1.0 - exp(-x / (1.0 - t)));
}

// ACES approché (Narkowicz) : contrasté, assombrit nettement les tons très sombres (une scène
// de nuit demande alors +0,5 à +1 IL)
fn aces(c: vec3<f32>) -> vec3<f32> {
    return clamp((c * (2.51 * c + 0.03)) / (c * (2.43 * c + 0.59) + 0.14), vec3<f32>(0.0), vec3<f32>(1.0));
}

// AgX (version minimale de Benjamin Wrensch) : courbe filmique qui désature doucement les
// très hautes lumières au lieu de les faire virer
fn agx(c: vec3<f32>) -> vec3<f32> {
    let inset = mat3x3<f32>(
        0.842479062253094, 0.0423282422610123, 0.0423756549057051,
        0.0784335999999992, 0.878468636469772, 0.0784336,
        0.0792237451477643, 0.0791661274605434, 0.879142973793104);
    let outset = mat3x3<f32>(
        1.19687900512017, -0.0528968517574562, -0.0529716355144438,
        -0.0980208811401368, 1.15190312990417, -0.0980434501171241,
        -0.0990297440797205, -0.0989611768448433, 1.15107367264116);
    let min_ev = -12.47393;
    let max_ev = 4.026069;
    var v = inset * max(c, vec3<f32>(1e-10));
    v = clamp(log2(v), vec3<f32>(min_ev), vec3<f32>(max_ev));
    v = (v - min_ev) / (max_ev - min_ev);
    let v2 = v * v;
    let v4 = v2 * v2;
    v = 15.5 * v4 * v2 - 40.14 * v4 * v + 31.96 * v4 - 6.868 * v2 * v + 0.4298 * v2 + 0.1191 * v - 0.00232;
    v = outset * v;
    return pow(clamp(v, vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(2.2));
}

fn pcg(v: u32) -> u32 {
    let s = v * 747796405u + 2891336453u;
    let w = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (w >> 22u) ^ w;
}
fn rnd(x: u32, y: u32, seed: u32) -> f32 {
    return f32(pcg(x ^ pcg(y ^ pcg(seed)))) / 4294967295.0;
}

@fragment
fn fs_composite(@builtin(position) fc: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = fc.xy / pu.size.xy;
    var c = textureLoad(t_hdr, vec2<i32>(fc.xy), 0).rgb;
    // l'occlusion est déjà appliquée par la profondeur de champ
    if (pu.dof.x < 0.5) { c = occluded(c, uv); }
    c = c + textureSampleLevel(t_bloom, s_lin, uv, 0.0).rgb * pu.params.z;

    // étalonnage : exposition et balance des blancs en lumière linéaire, puis rendu des tons
    c = max(c * pu.grade.x * pu.grade2.rgb, vec3<f32>(0.0));
    if (pu.game.x > 0.5) {
        // comme le jeu : Reinhard étendu, dont le point blanc (la lumière rendue blanche) suit
        // l'heure de la carte, puis la table d'étalonnage de l'heure, sur l'image encodée sRGB
        let w2 = pu.game.z * pu.game.z;
        c = min(c * (1.0 + c / w2) / (1.0 + c), vec3<f32>(1.0));
        if (pu.game.w > 0.5) {
            let s = to_srgb(c);
            c = from_srgb(mix(lut_read(s, 0.0), lut_read(s, 1.0), pu.game.y));
        }
    } else {
        let tm = i32(pu.grade.w + 0.5);
        if (tm == 1) { c = aces(c); } else if (tm == 2) { c = agx(c); } else if (tm == 3) { c = soft_shoulder(c); }
    }
    // contraste autour du gris moyen (0,18), puis saturation
    c = 0.18 * pow(c / 0.18, vec3<f32>(pu.grade.y));
    let l = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
    c = max(mix(vec3<f32>(l), c, pu.grade.z), vec3<f32>(0.0));

    // vignettage, sur l'image entière (une tuile n'en est qu'une partie)
    let full = fc.xy + pu.frame.xy;
    let aspect = pu.frame.z / max(pu.frame.w, 1.0);
    let d = (full / pu.frame.zw - 0.5) * vec2<f32>(aspect, 1.0);
    let r = length(d) / length(vec2<f32>(aspect, 1.0) * 0.5);
    c = c * (1.0 - pu.grade2.w * 0.85 * smoothstep(0.3, 1.15, r));

    // grain : un tirage par pixel de l'image exportée, plus présent dans les tons moyens
    if (pu.grade3.x > 0.0) {
        let cell = vec2<u32>(max(floor(full / max(pu.grade3.y, 1.0)), vec2<f32>(0.0)));
        let seed = u32(pu.grade3.z);
        let n = rnd(cell.x, cell.y, seed) + rnd(cell.x, cell.y, seed + 7919u) - 1.0;
        c = max(c * (1.0 + n * pu.grade3.x * 0.35) + n * pu.grade3.x * 0.004, vec3<f32>(0.0));
    }
    return vec4<f32>(c, 1.0);
}
"#;

/// Rendu des tons (« tone mapping ») : comment les lumières au-delà du blanc sont ramenées à
/// l'écran.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToneMap {
    /// écrêtage à 1 (rendu d'origine du studio)
    #[default]
    Clip,
    /// ombres et tons moyens intacts, hautes lumières ramenées en douceur vers le blanc
    Soft,
    /// courbe filmique contrastée
    Aces,
    /// courbe filmique douce, hautes lumières désaturées
    Agx,
}

impl ToneMap {
    pub const ALL: [Self; 4] = [Self::Clip, Self::Soft, Self::Aces, Self::Agx];

    pub fn label(self) -> &'static str {
        match self {
            Self::Clip => "Aucun (écrêtage)",
            Self::Soft => "Filmique doux (hautes lumières)",
            Self::Aces => "Filmique ACES (contrasté)",
            Self::Agx => "Filmique AgX (doux)",
        }
    }

    fn index(self) -> f32 {
        match self {
            Self::Clip => 0.0,
            Self::Aces => 1.0,
            Self::Agx => 2.0,
            Self::Soft => 3.0,
        }
    }

    pub fn from_key(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "aucun" | "clip" | "none" => Some(Self::Clip),
            "doux" | "soft" => Some(Self::Soft),
            "aces" => Some(Self::Aces),
            "agx" => Some(Self::Agx),
            _ => None,
        }
    }
}

/// Post-traitement artistique d'un projet : étalonnage, profondeur de champ, netteté. Les
/// valeurs par défaut laissent l'image telle quelle.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PostFx {
    /// exposition (diaphragmes : +1 = deux fois plus clair)
    pub exposure: f32,
    pub tonemap: ToneMap,
    /// rendu des tons et étalonnage du jeu, selon l'heure du ciel de la carte (point blanc de
    /// `<Exposure>`, tables `<ColorGrading>`) : remplace `tonemap`
    pub game_grading: bool,
    /// contraste (1 = neutre)
    pub contrast: f32,
    /// saturation (1 = neutre, 0 = noir et blanc)
    pub saturation: f32,
    /// température (−1 froid, bleu ; +1 chaud, orangé)
    pub temperature: f32,
    /// teinte (−1 vert, +1 magenta)
    pub tint: f32,
    /// assombrissement des bords (0 à 1)
    pub vignette: f32,
    /// grain de pellicule (0 à 1)
    pub grain: f32,
    /// netteté (0 à 1)
    pub sharpen: f32,
    /// profondeur de champ
    pub dof: bool,
    /// mise au point sur la cible de la caméra (sinon : `focus`)
    pub focus_auto: bool,
    /// distance de mise au point (m)
    pub focus: f32,
    /// flou de l'arrière-plan lointain, en % de la hauteur de l'image
    pub blur: f32,
}

impl Default for PostFx {
    fn default() -> Self {
        Self {
            exposure: 0.0,
            tonemap: ToneMap::Clip,
            game_grading: false,
            contrast: 1.0,
            saturation: 1.0,
            temperature: 0.0,
            tint: 0.0,
            vignette: 0.0,
            grain: 0.0,
            sharpen: 0.0,
            dof: false,
            focus_auto: true,
            focus: 5.0,
            blur: 1.0,
        }
    }
}

impl PostFx {
    /// Rendu « cinéma » : hautes lumières adoucies, un peu de contraste, vignettage, grain et
    /// netteté. Les ombres restent lisibles (scènes de nuit).
    pub fn cinema() -> Self {
        Self {
            tonemap: ToneMap::Soft,
            contrast: 1.06,
            saturation: 1.1,
            vignette: 0.3,
            grain: 0.12,
            sharpen: 0.45,
            ..Self::default()
        }
    }

    /// Rendu « jeu » : calé sur des captures d'Indar du serveur live (désert, canyons, savane au
    /// coucher), à heure et lieu comparables, avec le ciel, le brouillard et le voile de la carte.
    /// Le jeu pâlit ses hautes lumières sans les écrêter (sable du désert), et son image tire vers
    /// le rose-lavande : sol rouge-orangé, ombres violacées, ciel pervenche. Le rendu des tons et
    /// l'étalonnage sont ceux du jeu à l'heure du ciel (`game_grading`) ; le Reinhard du jeu
    /// assombrit et désature les tons moyens, d'où l'exposition et la saturation relevées. Pas de
    /// vignettage ni de grain, absents du jeu.
    pub fn game() -> Self {
        Self {
            exposure: 0.5,
            tonemap: ToneMap::Soft,
            game_grading: true,
            contrast: 1.08,
            saturation: 1.2,
            temperature: -0.2,
            tint: 0.9,
            ..Self::default()
        }
    }

    /// Multiplicateurs de la balance des blancs (luminosité conservée).
    fn white_balance(&self) -> [f32; 3] {
        let t = self.temperature.clamp(-1.0, 1.0) * 0.25;
        let g = self.tint.clamp(-1.0, 1.0) * 0.15;
        let c = [1.0 + t, 1.0 - g, 1.0 - t];
        let l = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        [c[0] / l, c[1] / l, c[2] / l]
    }
}

/// Réglages du post-traitement pour une image.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PostSettings {
    /// force de l'occlusion ambiante (0 = désactivée)
    pub ao: f32,
    /// rayon de recherche de l'occlusion (m)
    pub ao_radius: f32,
    /// force du bloom (0 = désactivé)
    pub bloom: f32,
    /// luminosité à partir de laquelle une zone brille
    pub bloom_threshold: f32,
    /// FXAA sur l'image finale
    pub fxaa: bool,
    /// étalonnage, profondeur de champ et netteté du projet
    pub fx: PostFx,
    /// distance de mise au point (m), déjà choisie (cible de la caméra ou réglage du projet)
    pub focus: f32,
    /// netteté appliquée ici ; coupée quand l'export la fait après réduction (`sharpen`)
    pub gpu_sharpen: bool,
    /// image entière dont la cible est une tuile : origine de la tuile et taille de l'image
    /// (pixels de rendu) ; `None` : la cible est l'image entière
    pub frame: Option<[f32; 4]>,
    /// taille d'un grain (pixels de rendu) : le suréchantillonnage d'un export
    pub grain_cell: f32,
    /// graine du grain (change à chaque image d'une vidéo)
    pub seed: f32,
    /// rayon maximal du flou de profondeur de champ (pixels de rendu ; marge des tuiles)
    pub max_coc: f32,
    /// photo par tuiles : projection de l'image entière et taille de sa version réduite, dont la
    /// profondeur (liée par [`PostTargets::set_ao_guide`]) sert à l'occlusion hors de la tuile
    pub ao_guide: Option<(Mat4, [u32; 2])>,
    /// étalonnage du jeu (`PostFx::game_grading`) : point blanc de la carte à l'heure du ciel,
    /// tables d'étalonnage chargées (`Post::set_luts`) et part de la seconde
    pub white_point: f32,
    pub luts: bool,
    pub lut_mix: f32,
}

impl Default for PostSettings {
    fn default() -> Self {
        Self {
            ao: 0.0,
            ao_radius: 0.3,
            bloom: 0.0,
            bloom_threshold: 1.0,
            fxaa: false,
            fx: PostFx::default(),
            focus: 5.0,
            gpu_sharpen: true,
            frame: None,
            grain_cell: 1.0,
            seed: 0.0,
            max_coc: 256.0,
            ao_guide: None,
            white_point: DEFAULT_WHITE_POINT,
            luts: false,
            lut_mix: 0.0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PostUniform {
    view_proj: [[f32; 4]; 4],
    inv_view_proj: [[f32; 4]; 4],
    eye: [f32; 4],
    params: [f32; 4],
    size: [f32; 4],
    frame: [f32; 4],
    grade: [f32; 4],
    grade2: [f32; 4],
    grade3: [f32; 4],
    dof: [f32; 4],
    full_vp: [[f32; 4]; 4],
    full_inv: [[f32; 4]; 4],
    guide: [f32; 4],
    game: [f32; 4],
}

/// Pipelines et dispositions du post-traitement (partagés par toutes les cibles).
pub struct Post {
    samples: u32,
    uniform: wgpu::Buffer,
    sampler: wgpu::Sampler,
    /// tables d'étalonnage du jeu (deux de 256 × 16, l'une sous l'autre), réécrites selon l'heure
    lut: wgpu::Texture,
    lut_view: wgpu::TextureView,
    ssao_layout: wgpu::BindGroupLayout,
    dof_layout: wgpu::BindGroupLayout,
    bloom_layout: wgpu::BindGroupLayout,
    composite_layout: wgpu::BindGroupLayout,
    resolve_layout: wgpu::BindGroupLayout,
    /// image finale lue par le FXAA et la netteté
    ldr_layout: wgpu::BindGroupLayout,
    ssao: wgpu::RenderPipeline,
    dof: wgpu::RenderPipeline,
    /// lissage du flou (grain des petites lumières)
    dof_smooth: wgpu::RenderPipeline,
    resolve: wgpu::RenderPipeline,
    fxaa: wgpu::RenderPipeline,
    cas: wgpu::RenderPipeline,
    /// format de l'image finale (et des images intermédiaires avant FXAA et netteté)
    final_format: wgpu::TextureFormat,
    prefilter: wgpu::RenderPipeline,
    down: wgpu::RenderPipeline,
    up: wgpu::RenderPipeline,
    composite: wgpu::RenderPipeline,
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn texture_entry(binding: u32, sample_type: wgpu::TextureSampleType, multisampled: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture { sample_type, view_dimension: wgpu::TextureViewDimension::D2, multisampled },
        count: None,
    }
}

fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

fn fullscreen_pipeline(
    device: &wgpu::Device,
    label: &str,
    source: String,
    layout: &wgpu::BindGroupLayout,
    entry: &str,
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
) -> wgpu::RenderPipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[layout],
        push_constant_ranges: &[],
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&pl),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_full"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some(entry),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState { format, blend, write_mask: wgpu::ColorWrites::ALL })],
        }),
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        multiview: None,
        cache: None,
    })
}

impl Post {
    pub fn new(device: &wgpu::Device, samples: u32, final_format: wgpu::TextureFormat) -> Self {
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("post"),
            size: std::mem::size_of::<PostUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("post"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let float = wgpu::TextureSampleType::Float { filterable: true };
        let bloom_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bloom"),
            entries: &[uniform_entry(0), texture_entry(1, float, false), sampler_entry(2)],
        });
        let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("composite"),
            entries: &[
                uniform_entry(0),
                texture_entry(1, float, false),
                texture_entry(2, float, false),
                texture_entry(3, float, false),
                sampler_entry(4),
                texture_entry(5, float, false),
            ],
        });
        let lut = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("colorkey"),
            size: wgpu::Extent3d { width: LUT_W, height: LUT_H * 2, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let lut_view = lut.create_view(&Default::default());
        let bloom_src = format!("{FULLSCREEN}{BLOOM_SHADER}");
        let add = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent::REPLACE,
        };
        let prefilter =
            fullscreen_pipeline(device, "bloom_prefilter", bloom_src.clone(), &bloom_layout, "fs_prefilter", HDR_FORMAT, None);
        let down = fullscreen_pipeline(device, "bloom_down", bloom_src.clone(), &bloom_layout, "fs_down", HDR_FORMAT, None);
        let up = fullscreen_pipeline(device, "bloom_up", bloom_src, &bloom_layout, "fs_up", HDR_FORMAT, Some(add));
        let composite = fullscreen_pipeline(
            device,
            "composite",
            format!("{FULLSCREEN}{POST_UNIFORM}{COMPOSITE_SHADER}{AO_READ}"),
            &composite_layout,
            "fs_composite",
            final_format,
            None,
        );
        let (ssao_layout, ssao) = Self::ssao_pipeline(device, samples);
        let (dof_layout, dof, dof_smooth) = Self::dof_pipeline(device, samples);
        let resolve_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("msaa_resolve"),
            entries: &[texture_entry(0, wgpu::TextureSampleType::Float { filterable: false }, true)],
        });
        let resolve = fullscreen_pipeline(
            device,
            "msaa_resolve",
            format!("{FULLSCREEN}{RESOLVE_SHADER}"),
            &resolve_layout,
            "fs_resolve",
            HDR_FORMAT,
            None,
        );
        let ldr_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ldr"),
            entries: &[uniform_entry(0), texture_entry(1, float, false), sampler_entry(2)],
        });
        let fxaa = fullscreen_pipeline(
            device,
            "fxaa",
            format!("{FULLSCREEN}{POST_UNIFORM}{LDR_INPUT}{FXAA_SHADER}"),
            &ldr_layout,
            "fs_fxaa",
            final_format,
            None,
        );
        let cas = fullscreen_pipeline(
            device,
            "cas",
            format!("{FULLSCREEN}{POST_UNIFORM}{LDR_INPUT}{CAS_SHADER}"),
            &ldr_layout,
            "fs_cas",
            final_format,
            None,
        );
        Self {
            samples,
            uniform,
            sampler,
            lut,
            lut_view,
            ssao_layout,
            dof_layout,
            bloom_layout,
            composite_layout,
            resolve_layout,
            ldr_layout,
            ssao,
            dof,
            dof_smooth,
            resolve,
            fxaa,
            cas,
            final_format,
            prefilter,
            down,
            up,
            composite,
        }
    }

    fn depth_type(samples: u32) -> &'static str {
        if samples > 1 { "texture_depth_multisampled_2d" } else { "texture_depth_2d" }
    }

    /// La lecture de la profondeur dépend de l'anticrénelage (texture multiéchantillonnée ou non).
    fn ssao_pipeline(device: &wgpu::Device, samples: u32) -> (wgpu::BindGroupLayout, wgpu::RenderPipeline) {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ssao"),
            entries: &[
                uniform_entry(0),
                texture_entry(1, wgpu::TextureSampleType::Depth, samples > 1),
                texture_entry(2, wgpu::TextureSampleType::Depth, samples > 1),
            ],
        });
        let src = format!("{FULLSCREEN}{POST_UNIFORM}{}", SSAO_SHADER.replace("DEPTH_TYPE", Self::depth_type(samples)));
        let pipeline = fullscreen_pipeline(device, "ssao", src, &layout, "fs_ssao", AO_FORMAT, None);
        (layout, pipeline)
    }

    /// Profondeur de champ : même dépendance à l'anticrénelage que l'occlusion.
    fn dof_pipeline(device: &wgpu::Device, samples: u32) -> (wgpu::BindGroupLayout, wgpu::RenderPipeline, wgpu::RenderPipeline) {
        let float = wgpu::TextureSampleType::Float { filterable: true };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dof"),
            entries: &[
                uniform_entry(0),
                texture_entry(1, float, false),
                texture_entry(2, float, false),
                texture_entry(3, wgpu::TextureSampleType::Depth, samples > 1),
                sampler_entry(4),
            ],
        });
        let src = format!(
            "{FULLSCREEN}{POST_UNIFORM}{}{AO_READ}",
            DOF_SHADER
                .replace("DEPTH_TYPE", Self::depth_type(samples))
                .replace("DEPTH_SAMPLES", &samples.max(1).to_string())
        );
        let pipeline = fullscreen_pipeline(device, "dof", src.clone(), &layout, "fs_dof", HDR_FORMAT, None);
        let smooth = fullscreen_pipeline(device, "dof_smooth", src, &layout, "fs_dof_smooth", HDR_FORMAT, None);
        (layout, pipeline, smooth)
    }

    /// Écrit les deux tables d'étalonnage (RGBA, 256 × 16 chacune) ; `None` : l'identité.
    pub fn set_luts(&self, queue: &wgpu::Queue, luts: Option<(&[u8], &[u8])>) {
        let identity = identity_lut();
        let (a, b) = luts.unwrap_or((&identity, &identity));
        for (row, data) in [a, b].into_iter().enumerate() {
            if data.len() != (LUT_W * LUT_H * 4) as usize {
                continue;
            }
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.lut,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: 0, y: LUT_H * row as u32, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                data,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(LUT_W * 4), rows_per_image: Some(LUT_H) },
                wgpu::Extent3d { width: LUT_W, height: LUT_H, depth_or_array_layers: 1 },
            );
        }
    }

    pub fn set_samples(&mut self, device: &wgpu::Device, samples: u32) {
        if samples != self.samples {
            (self.ssao_layout, self.ssao) = Self::ssao_pipeline(device, samples);
            (self.dof_layout, self.dof, self.dof_smooth) = Self::dof_pipeline(device, samples);
            self.samples = samples;
        }
    }
}

/// Images intermédiaires d'une cible.
pub struct PostTargets {
    /// scène en HDR (résolue si anticrénelage)
    pub hdr_view: wgpu::TextureView,
    /// scène floutée par la profondeur de champ (occlusion comprise)
    dof_view: wgpu::TextureView,
    /// flou avant lissage (alpha : rayon du cercle)
    dof_raw_view: wgpu::TextureView,
    ao_view: wgpu::TextureView,
    ao_size: (u32, u32),
    /// niveaux de bloom (1/2, 1/4…) avec leur taille
    bloom: Vec<(wgpu::TextureView, u32, u32)>,
    ssao_bind: wgpu::BindGroup,
    dof_bind: wgpu::BindGroup,
    dof_smooth_bind: wgpu::BindGroup,
    /// extraction du bloom depuis la scène nette (0) ou floutée (1)
    prefilter_binds: [wgpu::BindGroup; 2],
    /// paramètres de l'extraction : le seuil y est réécrit à chaque image
    prefilter_bufs: [wgpu::Buffer; 2],
    down_binds: Vec<wgpu::BindGroup>,
    up_binds: Vec<wgpu::BindGroup>,
    /// assemblage depuis la scène nette (0) ou floutée (1)
    composite_binds: [wgpu::BindGroup; 2],
    /// résolution du MSAA (absente sans anticrénelage)
    resolve_bind: Option<wgpu::BindGroup>,
    /// images finales intermédiaires (avant FXAA et netteté), et leur liaison en lecture
    ldr_views: [wgpu::TextureView; 2],
    ldr_binds: [wgpu::BindGroup; 2],
    width: u32,
    height: u32,
}

impl PostTargets {
    pub fn new(
        device: &wgpu::Device,
        post: &Post,
        width: u32,
        height: u32,
        depth: &wgpu::TextureView,
        msaa: Option<&wgpu::TextureView>,
    ) -> Self {
        let tex = |w: u32, h: u32, format, label| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        let hdr_view = tex(width, height, HDR_FORMAT, "hdr");
        let dof_view = tex(width, height, HDR_FORMAT, "dof");
        let dof_raw_view = tex(width, height, HDR_FORMAT, "dof_raw");
        let ao_size = ((width / 2).max(1), (height / 2).max(1));
        let ao_view = tex(ao_size.0, ao_size.1, AO_FORMAT, "ao");
        let mut bloom = Vec::new();
        let (mut w, mut h) = (width, height);
        while bloom.len() < BLOOM_LEVELS && w > 8 && h > 8 {
            w = (w / 2).max(1);
            h = (h / 2).max(1);
            bloom.push((tex(w, h, HDR_FORMAT, "bloom"), w, h));
        }
        // texels de la source et de la destination ; seuil et genou (extraction seulement)
        let level_bind = |src: &wgpu::TextureView, (sw, sh): (u32, u32), (dw, dh): (u32, u32)| {
            let u: [f32; 8] = [1.0 / sw as f32, 1.0 / sh as f32, 0.9, 0.5, 1.0 / dw as f32, 1.0 / dh as f32, 0.0, 0.0];
            let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("bloom_level"),
                contents: bytemuck::cast_slice(&u),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("bloom_level"),
                layout: &post.bloom_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(src) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&post.sampler) },
                ],
            });
            (bind, buf)
        };
        let first = bloom.first().map(|b| (b.1, b.2)).unwrap_or((1, 1));
        let (pb0, pf0) = level_bind(&hdr_view, (width, height), first);
        let (pb1, pf1) = level_bind(&dof_view, (width, height), first);
        let down_binds = (1..bloom.len())
            .map(|i| level_bind(&bloom[i - 1].0, (bloom[i - 1].1, bloom[i - 1].2), (bloom[i].1, bloom[i].2)).0)
            .collect();
        let up_binds = (0..bloom.len().saturating_sub(1))
            .map(|i| level_bind(&bloom[i + 1].0, (bloom[i + 1].1, bloom[i + 1].2), (bloom[i].1, bloom[i].2)).0)
            .collect();
        let ssao_bind = Self::ssao_bind(device, post, depth, depth);
        // flou : lit la scène ; lissage : lit le flou (même mise en page)
        let dof_bind_for = |src: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("dof"),
                layout: &post.dof_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: post.uniform.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(src) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&ao_view) },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(depth) },
                    wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(&post.sampler) },
                ],
            })
        };
        let dof_bind = dof_bind_for(&hdr_view);
        let dof_smooth_bind = dof_bind_for(&dof_raw_view);
        let bloom_view = bloom.first().map(|b| &b.0).unwrap_or(&hdr_view);
        let composite_bind = |src: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("composite"),
                layout: &post.composite_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: post.uniform.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(src) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(bloom_view) },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&ao_view) },
                    wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(&post.sampler) },
                    wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&post.lut_view) },
                ],
            })
        };
        let composite_binds = [composite_bind(&hdr_view), composite_bind(&dof_view)];
        let resolve_bind = msaa.map(|ms| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("msaa_resolve"),
                layout: &post.resolve_layout,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(ms) }],
            })
        });
        let ldr_views = [tex(width, height, post.final_format, "ldr_a"), tex(width, height, post.final_format, "ldr_b")];
        let ldr_bind = |v: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("ldr"),
                layout: &post.ldr_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: post.uniform.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(v) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&post.sampler) },
                ],
            })
        };
        let ldr_binds = [ldr_bind(&ldr_views[0]), ldr_bind(&ldr_views[1])];
        Self {
            hdr_view,
            dof_view,
            dof_raw_view,
            ao_view,
            ao_size,
            bloom,
            ssao_bind,
            dof_bind,
            dof_smooth_bind,
            prefilter_binds: [pb0, pb1],
            prefilter_bufs: [pf0, pf1],
            down_binds,
            up_binds,
            composite_binds,
            resolve_bind,
            ldr_views,
            ldr_binds,
            width,
            height,
        }
    }

    fn ssao_bind(device: &wgpu::Device, post: &Post, depth: &wgpu::TextureView, guide: &wgpu::TextureView) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ssao"),
            layout: &post.ssao_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: post.uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(depth) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(guide) },
            ],
        })
    }

    /// Profondeur de l'image entière réduite que l'occlusion lit hors de la tuile (photo par
    /// tuiles, même anticrénelage), ou `None` pour la délier. `depth` : celle de cette cible.
    pub fn set_ao_guide(&mut self, device: &wgpu::Device, post: &Post, depth: &wgpu::TextureView, guide: Option<&wgpu::TextureView>) {
        self.ssao_bind = Self::ssao_bind(device, post, depth, guide.unwrap_or(depth));
    }

    /// Encode résolution, occlusion, profondeur de champ, bloom, assemblage, FXAA et netteté
    /// vers `out` (l'image finale).
    #[allow(clippy::too_many_arguments)]
    pub fn run(
        &self,
        post: &Post,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        out: &wgpu::TextureView,
        view_proj: Mat4,
        eye: Vec3,
        settings: &PostSettings,
    ) {
        let fx = &settings.fx;
        let bloom_on = settings.bloom > 0.0 && !self.bloom.is_empty();
        let frame = settings.frame.unwrap_or([0.0, 0.0, self.width as f32, self.height as f32]);
        // flou à l'infini proportionnel à la hauteur de l'image entière, borné par la marge des tuiles
        let coc = (fx.blur.max(0.0) / 100.0 * frame[3]).min(settings.max_coc / 1.5);
        let dof_on = fx.dof && coc >= 0.5;
        let sharpen = if settings.gpu_sharpen { fx.sharpen.clamp(0.0, 1.0) } else { 0.0 };
        let wb = fx.white_balance();
        let u = PostUniform {
            view_proj: view_proj.to_cols_array_2d(),
            inv_view_proj: view_proj.inverse().to_cols_array_2d(),
            eye: eye.extend(1.0).to_array(),
            params: [
                settings.ao.clamp(0.0, 1.0),
                settings.ao_radius.max(0.01),
                // la remontée additionne tous les niveaux : on ramène la somme à un halo mesuré
                if bloom_on { settings.bloom.max(0.0) * 2.0 / self.bloom.len() as f32 } else { 0.0 },
                settings.bloom_threshold,
            ],
            size: [self.width as f32, self.height as f32, self.ao_size.0 as f32, self.ao_size.1 as f32],
            frame,
            grade: [
                2f32.powf(fx.exposure.clamp(-6.0, 6.0)),
                fx.contrast.clamp(0.3, 3.0),
                fx.saturation.clamp(0.0, 3.0),
                fx.tonemap.index(),
            ],
            grade2: [wb[0], wb[1], wb[2], fx.vignette.clamp(0.0, 1.0)],
            grade3: [fx.grain.clamp(0.0, 1.0), settings.grain_cell.max(1.0), settings.seed.max(0.0).floor(), sharpen],
            dof: [if dof_on { 1.0 } else { 0.0 }, settings.focus.max(0.05), coc, DOF_SAMPLES],
            full_vp: settings.ao_guide.map_or(Mat4::IDENTITY, |g| g.0).to_cols_array_2d(),
            full_inv: settings.ao_guide.map_or(Mat4::IDENTITY, |g| g.0.inverse()).to_cols_array_2d(),
            guide: match settings.ao_guide {
                Some((_, [w, h])) => [1.0, w as f32, h as f32, 0.0],
                None => [0.0; 4],
            },
            game: [
                if fx.game_grading { 1.0 } else { 0.0 },
                settings.lut_mix.clamp(0.0, 1.0),
                settings.white_point.max(0.1),
                if settings.luts { 1.0 } else { 0.0 },
            ],
        };
        queue.write_buffer(&post.uniform, 0, bytemuck::bytes_of(&u));

        let pass = |enc: &mut wgpu::CommandEncoder, view: &wgpu::TextureView, load: wgpu::LoadOp<wgpu::Color>, label: &str| {
            enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations { load, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            })
            .forget_lifetime()
        };
        let clear = wgpu::LoadOp::Clear(wgpu::Color::WHITE);
        let black = wgpu::LoadOp::Clear(wgpu::Color::BLACK);

        // résolution du MSAA vers l'image HDR
        if let Some(bind) = &self.resolve_bind {
            let mut p = pass(enc, &self.hdr_view, black, "msaa_resolve");
            p.set_pipeline(&post.resolve);
            p.set_bind_group(0, bind, &[]);
            p.draw(0..3, 0..1);
        }
        // occlusion ambiante (blanche = aucune quand elle est coupée)
        {
            let mut p = pass(enc, &self.ao_view, clear, "ssao");
            if settings.ao > 0.0 {
                p.set_pipeline(&post.ssao);
                p.set_bind_group(0, &self.ssao_bind, &[]);
                p.draw(0..3, 0..1);
            }
        }
        // profondeur de champ : la suite lit l'image floutée
        let src = if dof_on {
            {
                let mut p = pass(enc, &self.dof_raw_view, black, "dof");
                p.set_pipeline(&post.dof);
                p.set_bind_group(0, &self.dof_bind, &[]);
                p.draw(0..3, 0..1);
            }
            let mut p = pass(enc, &self.dof_view, black, "dof_smooth");
            p.set_pipeline(&post.dof_smooth);
            p.set_bind_group(0, &self.dof_smooth_bind, &[]);
            p.draw(0..3, 0..1);
            1
        } else {
            0
        };
        if bloom_on {
            // seuil et genou de l'extraction (octets 8 à 16 du tampon du premier niveau)
            let thr = settings.bloom_threshold.max(0.0);
            queue.write_buffer(&self.prefilter_bufs[src], 8, bytemuck::cast_slice(&[thr, (thr * 0.5).max(0.05)]));
            {
                let mut p = pass(enc, &self.bloom[0].0, black, "bloom_prefilter");
                p.set_pipeline(&post.prefilter);
                p.set_bind_group(0, &self.prefilter_binds[src], &[]);
                p.draw(0..3, 0..1);
            }
            for (i, bind) in self.down_binds.iter().enumerate() {
                let mut p = pass(enc, &self.bloom[i + 1].0, black, "bloom_down");
                p.set_pipeline(&post.down);
                p.set_bind_group(0, bind, &[]);
                p.draw(0..3, 0..1);
            }
            for (i, bind) in self.up_binds.iter().enumerate().rev() {
                let mut p = pass(enc, &self.bloom[i].0, wgpu::LoadOp::Load, "bloom_up");
                p.set_pipeline(&post.up);
                p.set_bind_group(0, bind, &[]);
                p.draw(0..3, 0..1);
            }
        }
        // assemblage, puis FXAA et netteté en relais entre les deux images intermédiaires
        let mut stages: Vec<(&wgpu::RenderPipeline, &str)> = Vec::new();
        if settings.fxaa {
            stages.push((&post.fxaa, "fxaa"));
        }
        if sharpen > 0.0 {
            stages.push((&post.cas, "cas"));
        }
        {
            let dst = if stages.is_empty() { out } else { &self.ldr_views[0] };
            let mut p = pass(enc, dst, black, "composite");
            p.set_pipeline(&post.composite);
            p.set_bind_group(0, &self.composite_binds[src], &[]);
            p.draw(0..3, 0..1);
        }
        let n = stages.len();
        for (k, (pipeline, label)) in stages.into_iter().enumerate() {
            let dst = if k + 1 == n { out } else { &self.ldr_views[(k + 1) % 2] };
            let mut p = pass(enc, dst, black, label);
            p.set_pipeline(pipeline);
            p.set_bind_group(0, &self.ldr_binds[k % 2], &[]);
            p.draw(0..3, 0..1);
        }
    }
}

/// Netteté d'une image RGBA8 sRGB, après la réduction d'un export suréchantillonné : même
/// calcul que le shader `fs_cas`, en lumière linéaire.
pub fn sharpen(rgba: &mut [u8], width: u32, height: u32, amount: f32) {
    use rayon::prelude::*;
    if amount <= 0.0 || width < 3 || height < 3 {
        return;
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
    let (w, h) = (width as usize, height as usize);
    let src = rgba.to_vec();
    let peak = -1.0 / (8.0 + (5.0 - 8.0) * amount.clamp(0.0, 1.0));
    rgba.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        let at = |x: usize, y: usize, k: usize| lut[src[(y * w + x) * 4 + k] as usize];
        let (yu, yd) = (y.saturating_sub(1), (y + 1).min(h - 1));
        for x in 0..w {
            let (xl, xr) = (x.saturating_sub(1), (x + 1).min(w - 1));
            for k in 0..3 {
                let (b, d, e, f, hh) = (at(x, yu, k), at(xl, y, k), at(x, y, k), at(xr, y, k), at(x, yd, k));
                let mn = b.min(d).min(e).min(f).min(hh);
                let mx = b.max(d).max(e).max(f).max(hh);
                let amp = (mn.min(1.0 - mx) / mx.max(1e-4)).clamp(0.0, 1.0).sqrt();
                let wgt = amp * peak;
                let o = ((b + d + f + hh) * wgt + e) / (1.0 + 4.0 * wgt);
                row[x * 4 + k] = enc(o);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sharpen_keeps_flat_and_boosts_edges() {
        // image uniforme : inchangée
        let mut flat = vec![128u8; 8 * 8 * 4];
        sharpen(&mut flat, 8, 8, 1.0);
        assert!(flat.iter().all(|&v| v == 128));
        // bord vertical : le côté sombre s'assombrit près du bord, le clair s'éclaircit
        let mut img = Vec::new();
        for _ in 0..8 {
            for x in 0..8 {
                let v = if x < 4 { 80u8 } else { 160u8 };
                img.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let before = img.clone();
        sharpen(&mut img, 8, 8, 1.0);
        let px = |im: &[u8], x: usize| im[(4 * 8 + x) * 4];
        assert!(px(&img, 3) < px(&before, 3));
        assert!(px(&img, 4) > px(&before, 4));
        assert_eq!(px(&img, 0), px(&before, 0));
    }

    #[test]
    fn neutral_white_balance() {
        assert!(PostFx::default().white_balance().iter().all(|c| (c - 1.0).abs() < 1e-5));
        let warm = PostFx { temperature: 1.0, ..PostFx::default() }.white_balance();
        assert!(warm[0] > warm[2]);
    }
}
