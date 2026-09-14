use crate::metadata::comfy_parse::{topology_from_ui, workflow_key_from_canonical};
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowTemplateDto {
    pub name: String,
    pub path: String,
    pub workflow_id: Option<String>,
    pub topology_signature: String,
    pub graph_json: String,
    pub node_count: usize,
    pub diffusion_models: Vec<String>,
    pub model_chain: Vec<String>,
}

pub fn find_templates_from_output(output: &Path) -> Vec<WorkflowTemplateDto> {
    let Some(comfy_root) = output.parent() else { return Vec::new() };
    let user_dir = comfy_root.join("user");
    if !user_dir.is_dir() { return Vec::new() }

    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(user_dir)
        .min_depth(1)
        .max_depth(5)
        .into_iter()
        .filter_map(|entry| entry.ok())
    {
        if !entry.file_type().is_file()
            || entry.path().extension().and_then(|ext| ext.to_str()) != Some("json")
        {
            continue;
        }
        if let Some(template) = parse_template(entry.path()) { out.push(template); }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

fn parse_template(path: &Path) -> Option<WorkflowTemplateDto> {
    let root: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let nodes = root.get("nodes")?.as_array()?;
    let canonical = topology_from_ui(&root)?;

    let mut models = std::collections::BTreeSet::new();
    let mut chain = Vec::new();
    for node in nodes {
        let node_type = node.get("type").and_then(Value::as_str).unwrap_or("");
        if node_type.is_empty() {
            continue;
        }
        let values = node.get("widgets_values").and_then(Value::as_array);
        // Loader classification is substring-based so custom loader nodes
        // (rgthree "Power Lora Loader", "Lora Loader (LoraManager)", Flux
        // packs, ...) are picked up the same way as the native ones. All
        // ComfyUI loader widgets put the model dropdown in widgets_values.
        let is_diff = node_type.contains("UNET")
            || node_type.contains("Unet")
            || node_type.contains("Checkpoint")
            || node_type.contains("Diffusion");
        let is_lora = node_type.to_lowercase().contains("lora");
        let is_vae = node_type.contains("VAE");
        // The first widget is the model file for every loader variant; for
        // custom LoRA organisers the widget list may hold dicts/slots, so
        // fall back to scanning for the first plain string path.
        let model = if is_diff || is_lora || is_vae {
            values
                .and_then(|v| v.first())
                .and_then(Value::as_str)
                .or_else(|| {
                    values.and_then(|v| {
                        v.iter()
                            .filter_map(|w| w.as_str())
                            .find(|s| !s.is_empty() && s.contains('/'))
                    })
                })
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        } else {
            None
        };
        if let Some(model) = model {
            if is_diff {
                models.insert(model.to_string());
            }
            chain.push(format!("{}:{}", node_type, model));
        }
    }

    Some(WorkflowTemplateDto {
        name: path.file_stem()?.to_string_lossy().to_string(),
        path: path.to_string_lossy().to_string(),
        workflow_id: root.get("id").and_then(Value::as_str).map(str::to_string),
        topology_signature: workflow_key_from_canonical(&canonical),
        graph_json: canonical,
        node_count: nodes.len(),
        diffusion_models: models.into_iter().collect(),
        model_chain: chain,
    })
}

pub fn output_path_from_source(source: &str) -> PathBuf { PathBuf::from(source) }

/// Match an image execution graph (canonical `{"t": [...], "e": [...]}`)
/// against saved workflow templates. Exact signature equality wins
/// (confidence 1.0); otherwise the image graph may be a valid subgraph of
/// a template (bypass/mute/disabled nodes make templates larger than the
/// executed graph) — confidence is the fraction of image edges covered.
/// Returns `(template_id, confidence)`.
pub fn match_template(
    image_graph_json: &str,
    templates: &[crate::db::WorkflowTemplateRow],
) -> Option<(i64, f64)> {
    let image: Value = serde_json::from_str(image_graph_json).ok()?;
    let image_types: std::collections::HashSet<String> = image
        .get("t")?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    let image_edges: std::collections::HashSet<String> = image
        .get("e")?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    if image_edges.is_empty() {
        return None;
    }

    let mut best: Option<(i64, f64)> = None;
    for tpl in templates {
        if tpl.graph_json.is_empty() {
            continue;
        }
        let Ok(tpl_value) = serde_json::from_str::<Value>(&tpl.graph_json) else {
            continue;
        };
        let Some(types) = tpl_value.get("t").and_then(Value::as_array) else {
            continue;
        };
        let Some(edges) = tpl_value.get("e").and_then(Value::as_array) else {
            continue;
        };
        let tpl_types: std::collections::HashSet<String> = types
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        let tpl_edges: std::collections::HashSet<String> = edges
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        if !image_types.is_subset(&tpl_types) || !image_edges.is_subset(&tpl_edges) {
            continue;
        }
        let covered = image_edges
            .iter()
            .filter(|e| tpl_edges.contains(*e))
            .count() as f64
            / image_edges.len() as f64;
        if covered >= 0.5 {
            let candidate = (tpl.id, covered);
            if best.as_ref().map(|(_, c)| covered > *c).unwrap_or(true) {
                best = Some(candidate);
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::WorkflowTemplateRow;

    const IMAGE_GRAPH: &str = r#"{"t":["CLIPTextEncode","KSampler","LoraLoader","UNETLoader"],"e":["CLIPTextEncode->KSampler","LoraLoader->CLIPTextEncode","LoraLoader->KSampler","UNETLoader->LoraLoader"]}"#;

    fn tpl(id: i64, graph: &str) -> WorkflowTemplateRow {
        WorkflowTemplateRow {
            id,
            name: format!("tpl{id}"),
            path: format!("/tpl/{id}.json"),
            workflow_id: None,
            topology_signature: crate::metadata::comfy_parse::workflow_key_from_canonical(graph),
            graph_json: graph.to_string(),
            node_count: 0,
            diffusion_models: String::new(),
            model_chain: String::new(),
        }
    }

    #[test]
    fn exact_match_wins() {
        let templates = vec![
            tpl(1, r#"{"t":["KSampler","UNETLoader"],"e":["UNETLoader->KSampler"]}"#),
            tpl(2, IMAGE_GRAPH),
        ];
        let (id, conf) = match_template(IMAGE_GRAPH, &templates).expect("must match");
        assert_eq!(id, 2);
        assert!((conf - 1.0).abs() < 1e-9);
    }

    #[test]
    fn subgraph_match_against_larger_template() {
        let bigger = r#"{"t":["CLIPTextEncode","KSampler","LoraLoader","MarkdownNote","UNETLoader"],"e":["CLIPTextEncode->KSampler","LoraLoader->CLIPTextEncode","LoraLoader->KSampler","UNETLoader->LoraLoader","MarkdownNote->KSampler"]}"#;
        let templates = vec![tpl(7, bigger)];
        let (id, conf) = match_template(IMAGE_GRAPH, &templates).expect("subgraph must match");
        assert_eq!(id, 7);
        assert!(conf >= 0.5);
    }

    #[test]
    fn disjoint_graph_does_not_match() {
        let unrelated = r#"{"t":["EmptyLatentImage","VAEDecode"],"e":["EmptyLatentImage->VAEDecode"]}"#;
        let templates = vec![tpl(9, unrelated)];
        assert!(match_template(IMAGE_GRAPH, &templates).is_none());
    }
}
