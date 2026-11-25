//! Fichiers `.adr` (ActorRuntime) : décrivent un acteur (modèle de base, textures, LOD).

use anyhow::{Context, Result};

#[derive(Clone, Debug, Default)]
pub struct TextureAlias {
    pub alias: String,
    pub texture: String,
    pub semantic_hash: i64,
}

#[derive(Clone, Debug, Default)]
pub struct Adr {
    /// modèle principal (.dme)
    pub base_model: Option<String>,
    pub palette: Option<String>,
    pub animation_network: Option<String>,
    pub anim_set: Option<String>,
    pub textures: Vec<TextureAlias>,
    pub lods: Vec<String>,
    pub equip_slot: Option<String>,
    /// emplacements masqués quand cet acteur est porté : `<CoveredSlots name="CustomizationHead"/>`
    /// d'une cagoule intégrale cache la tête (`EquippedSlotName="CustomizationHead"`)
    pub covered_slots: Vec<String>,
    /// effets composites joués en continu sur l'acteur (`<EffectList><Effect type="compositeEffect"
    /// name="EFX_PlantBulbs_1"/>`) : lueurs, fumées, insectes autour d'un objet du décor
    pub effects: Vec<String>,
}

impl Adr {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let text = String::from_utf8_lossy(data);
        let text = text.trim_start_matches('\u{feff}');
        let doc = roxmltree::Document::parse(text).context("ADR : XML invalide")?;
        let mut adr = Adr::default();
        for n in doc.descendants() {
            match n.tag_name().name() {
                "Base" => {
                    adr.base_model = n.attribute("fileName").map(String::from);
                    adr.palette = n.attribute("paletteName").map(String::from);
                }
                "AnimationNetwork" => {
                    adr.animation_network = n.attribute("fileName").map(String::from);
                    adr.anim_set = n.attribute("animSetName").map(String::from);
                }
                "Alias" if n.attribute("textureName").is_some() => adr.textures.push(TextureAlias {
                    alias: n.attribute("aliasName").unwrap_or_default().into(),
                    texture: n.attribute("textureName").unwrap_or_default().into(),
                    semantic_hash: n.attribute("semanticHash").and_then(|s| s.parse().ok()).unwrap_or(0),
                }),
                "Lod" => {
                    if let Some(f) = n.attribute("fileName") {
                        adr.lods.push(f.into())
                    }
                }
                "EquipType" => adr.equip_slot = n.attribute("EquippedSlotName").map(String::from),
                "Effect" if n.parent().is_some_and(|p| p.has_tag_name("EffectList")) => {
                    if let Some(s) = n.attribute("name").filter(|s| !s.is_empty()) {
                        adr.effects.push(s.into())
                    }
                }
                "CoveredSlots" => {
                    if let Some(s) = n.attribute("name").filter(|s| !s.is_empty()) {
                        adr.covered_slots.push(s.into())
                    }
                }
                _ => {}
            }
        }
        Ok(adr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effect_list() {
        let xml = r#"<ActorRuntime>
    <Base fileName="Hossin_Props_SwampBulbs02_LOD0.dme" paletteName="Hossin_Props_SwampBulbs02_Lod0.dma" />
    <EffectList>
        <Effect toolName="fx" type="compositeEffect" name="EFX_PlantBulbs_1" id="5071" />
    </EffectList>
</ActorRuntime>"#;
        let adr = Adr::parse(xml.as_bytes()).unwrap();
        assert_eq!(adr.effects, ["EFX_PlantBulbs_1"]);
        assert_eq!(adr.base_model.as_deref(), Some("Hossin_Props_SwampBulbs02_LOD0.dme"));
    }
}
