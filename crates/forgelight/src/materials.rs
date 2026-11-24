//! `materials_3.xml` : définitions de matériaux et dispositions des sommets.

use crate::hash::jenkins;
use anyhow::{Context, Result};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElemType {
    Float1,
    Float2,
    Float3,
    Float4,
    Half2,
    Half4,
    UByte4N,
    UByte4,
    D3dColor,
    Short2,
    Short4,
    Unknown,
}

impl ElemType {
    fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "float1" => Self::Float1,
            "float2" => Self::Float2,
            "float3" => Self::Float3,
            "float4" => Self::Float4,
            "float16_2" => Self::Half2,
            "float16_4" => Self::Half4,
            "ubyte4n" => Self::UByte4N,
            "ubyte4" => Self::UByte4,
            "d3dcolor" => Self::D3dColor,
            "short2" => Self::Short2,
            "short4" => Self::Short4,
            _ => Self::Unknown,
        }
    }

    pub fn size(self) -> usize {
        match self {
            Self::Float1 => 4,
            Self::Float2 => 8,
            Self::Float3 => 12,
            Self::Float4 => 16,
            Self::Half2 => 4,
            Self::Half4 => 8,
            Self::UByte4N | Self::UByte4 | Self::D3dColor => 4,
            Self::Short2 => 4,
            Self::Short4 => 8,
            Self::Unknown => 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct LayoutEntry {
    pub stream: usize,
    pub ty: ElemType,
    pub usage: String,
    pub usage_index: u32,
}

#[derive(Clone, Debug)]
pub struct InputLayout {
    pub name: String,
    pub entries: Vec<LayoutEntry>,
}

impl InputLayout {
    pub fn stream_stride(&self, stream: usize) -> usize {
        self.entries.iter().filter(|e| e.stream == stream).map(|e| e.ty.size()).sum()
    }
}

#[derive(Clone, Debug)]
pub struct MaterialDef {
    pub name: String,
    pub input_layout: String,
}

#[derive(Default)]
pub struct MaterialLibrary {
    pub layouts: HashMap<String, InputLayout>,
    pub defs: HashMap<u32, MaterialDef>,
}

impl MaterialLibrary {
    pub fn parse(xml: &str) -> Result<Self> {
        let doc = roxmltree::Document::parse(xml).context("XML materials_3 invalide")?;
        let mut lib = Self::default();
        for arr in doc.descendants().filter(|n| n.has_tag_name("Array")) {
            match arr.attribute("Name") {
                Some("InputLayouts") => {
                    for obj in arr.children().filter(|n| n.attribute("Class") == Some("InputLayout")) {
                        let name = obj.attribute("Name").unwrap_or_default().to_string();
                        let entries = obj
                            .descendants()
                            .filter(|n| n.attribute("Class") == Some("LayoutEntry"))
                            .map(|e| LayoutEntry {
                                stream: e.attribute("Stream").and_then(|s| s.parse().ok()).unwrap_or(0),
                                ty: ElemType::parse(e.attribute("Type").unwrap_or("")),
                                usage: e.attribute("Usage").unwrap_or("").to_string(),
                                usage_index: e.attribute("UsageIndex").and_then(|s| s.parse().ok()).unwrap_or(0),
                            })
                            .collect();
                        lib.layouts.insert(name.clone(), InputLayout { name, entries });
                    }
                }
                Some("MaterialDefinitions") => {
                    for obj in arr.children().filter(|n| n.attribute("Class") == Some("MaterialDefinition")) {
                        let name = obj.attribute("Name").unwrap_or_default().to_string();
                        let styles: Vec<_> = obj
                            .descendants()
                            .filter(|n| n.attribute("Class") == Some("DrawStyle"))
                            .collect();
                        let style = styles
                            .iter()
                            .find(|s| s.attribute("Name") == Some("Normal"))
                            .or(styles.first());
                        let input_layout =
                            style.and_then(|s| s.attribute("InputLayout")).unwrap_or("").to_string();
                        lib.defs.entry(jenkins(name.as_bytes())).or_insert(MaterialDef { name, input_layout });
                    }
                }
                _ => {}
            }
        }
        Ok(lib)
    }

    pub fn layout_for(&self, def_hash: u32) -> Option<&InputLayout> {
        self.defs.get(&def_hash).and_then(|d| self.layouts.get(&d.input_layout))
    }
}
