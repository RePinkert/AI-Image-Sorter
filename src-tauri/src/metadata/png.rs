use anyhow::{anyhow, Result};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use super::{ImageMeta, LoraInfo, SamplerInfo, SourceKind};

pub fn parse_png(path: &Path) -> Result<ImageMeta> {
    let decoder = png::Decoder::new(BufReader::new(File::open(path)?));
    let reader = decoder.read_info()?;
    let mut prompt_json: Option<String> = None;
    let mut parameters: Option<String> = None;

    for item in reader.info().uncompressed_latin1_text.iter() {
        if item.keyword == "prompt" {
            prompt_json = Some(item.text.clone());
        } else if item.keyword == "parameters" {
            parameters = Some(item.text.clone());
        }
    }
    for item in reader.info().utf8_text.iter() {
        if item.keyword == "prompt" {
            prompt_json = Some(item.get_text()?.to_string());
        } else if item.keyword == "parameters" {
            parameters = Some(item.get_text()?.to_string());
        }
    }

    if let Some(json) = prompt_json {
        return Ok(super::comfy_parse::parse_comfy_workflow(&json));
    }
    if let Some(p) = parameters {
        return Ok(super::a1111_parse::parse_a1111_parameters(&p));
    }
    Ok(ImageMeta {
        source_kind: SourceKind::Local,
        ..Default::default()
    })
}

#[allow(dead_code)]
pub fn extract_dims(path: &Path) -> Result<(u32, u32)> {
    let decoder = png::Decoder::new(BufReader::new(File::open(path)?));
    let reader = decoder.read_info()?;
    Ok((reader.info().width, reader.info().height))
}

#[allow(dead_code)]
fn unused() {
    let _ = LoraInfo::default();
    let _ = SamplerInfo::default();
    let _: Result<()> = Err(anyhow!("x"));
}
