use anyhow::Result;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use super::{ImageMeta, SourceKind};

pub fn parse_png(path: &Path) -> Result<ImageMeta> {
    let decoder = png::Decoder::new(BufReader::new(File::open(path)?));
    let reader = decoder.read_info()?;
    let mut prompt_json: Option<String> = None;
    let mut workflow_json: Option<String> = None;
    let mut parameters: Option<String> = None;

    for item in reader.info().uncompressed_latin1_text.iter() {
        if item.keyword == "prompt" {
            prompt_json = Some(item.text.clone());
        } else if item.keyword == "workflow" {
            workflow_json = Some(item.text.clone());
        } else if item.keyword == "parameters" {
            parameters = Some(item.text.clone());
        }
    }
    for item in reader.info().utf8_text.iter() {
        if item.keyword == "prompt" {
            prompt_json = Some(item.get_text()?.to_string());
        } else if item.keyword == "workflow" {
            workflow_json = Some(item.get_text()?.to_string());
        } else if item.keyword == "parameters" {
            parameters = Some(item.get_text()?.to_string());
        }
    }

    let mut meta = if let Some(json) = prompt_json {
        super::comfy_parse::parse_comfy_workflow(&json)
    } else if let Some(p) = parameters {
        super::a1111_parse::parse_a1111_parameters(&p)
    } else {
        ImageMeta {
            source_kind: SourceKind::Local,
            ..Default::default()
        }
    };
    // If the execution graph could not be extracted (or the API prompt is
    // missing), fall back to the UI-format `workflow` chunk for the
    // workflow identity only.
    if meta.workflow_key.is_empty() {
        if let Some(json) = &workflow_json {
            if let Some((key, canonical)) = super::comfy_parse::parse_comfy_ui_topology(json) {
                meta.workflow_key = key;
                meta.workflow_graph_json = canonical;
                if meta.source_kind == SourceKind::Local {
                    meta.source_kind = SourceKind::Comfy;
                }
                meta.raw_ok = true;
            }
        }
    }
    meta.width = reader.info().width;
    meta.height = reader.info().height;
    Ok(meta)
}
