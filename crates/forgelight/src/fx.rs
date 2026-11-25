//! Effets de particules (`ParticleEntity`, `.xml`) : lecture des balises lumineuses fixes.
//!
//! Les antennes et les tours d'avant-poste jouent un effet `Lights_*_red` (par exemple
//! `Env_Lights_CommunicationEquip_02_Red` → `Lights_CommunicationEquip_02_red.xml`) : des points
//! rouges immobiles (émetteurs `SpriteEmitter` ou `StreakEmitter`, `FaceCamera`) posés en haut du
//! mât et le long de ses bras, qui s'allument une demi-seconde à chaque boucle de l'effet
//! (`EndTime`, 1 à 1,5 s). On n'en lit que ce qui sert à les placer et à les dessiner.

use anyhow::{Context, Result};
use glam::Vec3;

/// Un point lumineux fixe d'un effet, dans le repère de l'objet qui le joue.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Beacon {
    /// position (repère du modèle, m)
    pub pos: Vec3,
    /// côté du carré (m, `ConstantSize`)
    pub size: f32,
    /// couleur (sRGB 0..1, `ConstantColor`)
    pub color: [f32; 3],
    /// `Brightness` du matériau de l'émetteur
    pub brightness: f32,
    /// durée allumée (s, `LifetimeRange`) et durée d'une boucle de l'effet (s, `EndTime`)
    pub life: f32,
    pub period: f32,
}

/// Fichier `.xml` de l'effet d'un `.adr` quand c'est une balise lumineuse : le nom sans son
/// préfixe d'outil (`Env_`, `EFX_`, `PFX_`), s'il commence par `Lights_`.
pub fn beacon_file(effect: &str) -> Option<String> {
    let rest = effect.split_once('_').map(|(_, r)| r).unwrap_or(effect);
    rest.get(..7).filter(|p| p.eq_ignore_ascii_case("lights_")).map(|_| format!("{rest}.xml"))
}

/// Décalage de l'effet composite dans le repère de l'objet. Les définitions des effets
/// composites (`EFX_…`) ne sont pas dans les archives : celui de la tour d'avant-poste place ses
/// particules 90 m plus haut que leur `.xml` (relevé sur son modèle : le point du sommet et ceux de
/// la ligne tombent à moins de 10 cm de ses sommets).
pub fn composite_offset(effect: &str) -> Vec3 {
    if effect.eq_ignore_ascii_case("EFX_Lights_OutpostTower_Red") {
        Vec3::new(0.0, 90.0, 0.0)
    } else {
        Vec3::ZERO
    }
}

fn floats(s: &str) -> Vec<f32> {
    s.split(',').filter_map(|v| v.trim().parse().ok()).collect()
}

fn vec3(s: Option<&str>) -> Vec3 {
    match floats(s.unwrap_or_default())[..] {
        [x, y, z, ..] => Vec3::new(x, y, z),
        _ => Vec3::ZERO,
    }
}

/// Points lumineux fixes d'un effet : pour chaque `Instance`, son émetteur, à sa position. Une
/// forme `Line` en espacement régulier pose `Count` points, de `Start` vers `End` par pas de
/// `(End − Start) / SpacingSteps` ; une `Sphere` (rayon presque nul) un seul.
pub fn parse_beacons(data: &[u8]) -> Result<Vec<Beacon>> {
    let text = String::from_utf8_lossy(data);
    let text = text.trim_start_matches('\u{feff}');
    // (certains `.xml` du jeu ont un `>` en trop après une balise : sans effet sur le reste)
    let doc = roxmltree::Document::parse(text).context("effet : XML invalide")?;
    let root = doc.root_element();
    let period = root.attribute("EndTime").and_then(|v| v.parse().ok()).unwrap_or(1.0f32);
    let mut out = Vec::new();
    for inst in root.children().filter(|n| n.has_tag_name("Instance")) {
        if inst.attribute("Active") == Some("false") {
            continue;
        }
        let Some(name) = inst.attribute("Emitter") else { continue };
        let Some(em) = root
            .children()
            .find(|n| n.is_element() && n.tag_name().name().ends_with("Emitter") && n.attribute("Name") == Some(name))
        else {
            continue;
        };
        let count = em.attribute("Count").and_then(|v| v.parse().ok()).unwrap_or(1u32).max(1);
        let origin = vec3(inst.attribute("Position"));
        let mut size = 0.3;
        let mut color = [1.0; 3];
        let mut brightness = 1.0;
        let mut life = period;
        let mut points = vec![Vec3::ZERO];
        for n in em.children().filter(|n| n.is_element()) {
            match (n.tag_name().name(), n.attribute("Type")) {
                ("LifetimeRange", _) => life = n.attribute("Min").and_then(|v| v.parse().ok()).unwrap_or(life),
                ("Shape", Some("Line")) => {
                    let (a, b) = (vec3(n.attribute("Start")), vec3(n.attribute("End")));
                    let steps = n.attribute("SpacingSteps").and_then(|v| v.parse().ok()).unwrap_or(count.max(2) - 1).max(1);
                    let step = (b - a) / steps as f32;
                    points = (0..count).map(|k| a + step * k as f32).collect();
                }
                ("Controller", Some("ConstantSize")) => size = n.attribute("Size").and_then(|v| v.parse().ok()).unwrap_or(size),
                ("Controller", Some("ConstantColor")) => {
                    if let [r, g, b, ..] = floats(n.attribute("Color").unwrap_or_default())[..] {
                        color = [r, g, b];
                    }
                }
                ("Material", _) => {
                    for p in n.children().filter(|p| p.has_tag_name("Parameter")) {
                        if p.attribute("Name") == Some("Brightness") {
                            brightness = p.attribute("Value").and_then(|v| v.parse().ok()).unwrap_or(brightness);
                        }
                    }
                }
                _ => {}
            }
        }
        out.extend(points.into_iter().map(|p| Beacon { pos: origin + p, size, color, brightness, life, period }));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_of_effect() {
        assert_eq!(beacon_file("Env_Lights_CommunicationEquip_02_Red").as_deref(), Some("Lights_CommunicationEquip_02_Red.xml"));
        assert_eq!(beacon_file("EFX_Lights_OutpostTower_Red").as_deref(), Some("Lights_OutpostTower_Red.xml"));
        assert_eq!(beacon_file("EFX_PlantBulbs_1"), None);
        assert_eq!(beacon_file("EFX_Light_Purple"), None);
    }

    #[test]
    fn line_and_sphere() {
        // extrait de `Lights_CommunicationEquip_02_red.xml`
        let xml = r#"<?xml version="1.0" encoding="utf-8" ?>
<ParticleEntity Version="1" Continuous="true" EndTime="1.500000">
    <StreakEmitter Name="Sprite" Count="1">
        <LifetimeRange Min="0.500000" Max="0.500000" />
        <Shape Type="Sphere" Radius="0.000100" InnerRadius="0.000000" />
        <Material Name="StreakParticle">
            <Parameter Name="Brightness" Value="15.000000" />
            <Parameter Name="Scaler" Value="3.000000" />>
        </Material>
        <Controller Type="ConstantSize" Size="0.300000" />
        <Controller Type="ConstantColor" Color="0.50, 0.00, 0.00, 1.00" />
    </StreakEmitter>
    <StreakEmitter Name="SpriteLow" Count="2">
        <LifetimeRange Min="0.500000" Max="0.500000" />
        <Shape Type="Line" Start="0.00, 0.00, 0.00" End="-12.30, 0.00, 0.00" RegularSpacing="true" SpacingSteps="2" />
        <Controller Type="ConstantSize" Size="0.200000" />
        <Controller Type="ConstantColor" Color="0.50, 0.00, 0.00, 1.00" />
    </StreakEmitter>
    <Instance Name="Instance_0" Emitter="Sprite" Position="0.350000, 64.500000, 0.000000" Active="true" />
    <Instance Name="SpriteLow_0" Emitter="SpriteLow" Position="3.080000, 54.500000, 0.000000" Active="true" />
</ParticleEntity>"#;
        let b = parse_beacons(xml.as_bytes()).unwrap();
        assert_eq!(b.len(), 3);
        assert_eq!(b[0].pos, Vec3::new(0.35, 64.5, 0.0));
        assert_eq!((b[0].size, b[0].brightness, b[0].life, b[0].period), (0.3, 15.0, 0.5, 1.5));
        assert_eq!(b[0].color, [0.5, 0.0, 0.0]);
        // de part et d'autre du mât (±3,08 m), pas jusqu'au bout de la ligne
        assert!((b[1].pos - Vec3::new(3.08, 54.5, 0.0)).length() < 1e-4);
        assert!((b[2].pos - Vec3::new(-3.07, 54.5, 0.0)).length() < 1e-4);
        assert_eq!(b[2].size, 0.2);
    }
}
