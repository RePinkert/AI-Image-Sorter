use serde_json::Value;
use std::collections::HashMap;

use super::{ImageMeta, LoraInfo, SamplerInfo, SourceKind};

pub fn parse_comfy_workflow(json_str: &str) -> ImageMeta {
    let mut meta = ImageMeta {
        source_kind: SourceKind::Comfy,
        ..Default::default()
    };
    let root: Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return meta,
    };
    let obj = match root.as_object() {
        Some(o) => o,
        None => return meta,
    };
    meta.raw_ok = true;

    // Build node map: id -> node
    let mut nodes: HashMap<String, &Value> = HashMap::new();
    for (id, node) in obj {
        nodes.insert(id.clone(), node);
    }

    // Collect all CLIPTextEncode nodes and their resolved text.
    // Track which are wired to KSampler positive/negative.
    let mut positive_texts: Vec<String> = Vec::new();
    let mut negative_texts: Vec<String> = Vec::new();
    let mut standalone_clip_texts: Vec<String> = Vec::new();

    // First pass: find KSampler nodes and trace positive/negative.
    for (_, node) in &nodes {
        let Some(nobj) = node.as_object() else { continue };
        let class = nobj.get("class_type").and_then(|v| v.as_str()).unwrap_or("");
        let is_sampler = matches!(
            class,
            "KSampler" | "KSamplerAdvanced" | "SamplerCustom"
        );
        if !is_sampler {
            continue;
        }
        let inputs = nobj.get("inputs").and_then(|v| v.as_object());
        let Some(inputs) = inputs else { continue };

        if let Some(pos_ref) = inputs.get("positive") {
            if let Some(text) = trace_conditioning_text(pos_ref, &nodes, 0) {
                if !positive_texts.contains(&text) {
                    positive_texts.push(text);
                }
            }
        }
        if let Some(neg_ref) = inputs.get("negative") {
            // ConditioningZeroOut produces empty negative
            if is_conditioning_zero_out(neg_ref, &nodes) {
                // negative is zeroed -> empty
            } else if let Some(text) = trace_conditioning_text(neg_ref, &nodes, 0) {
                if !negative_texts.contains(&text) {
                    negative_texts.push(text);
                }
            }
        }
    }

    // Second pass: collect all CLIPTextEncode texts not traced via sampler (standalone).
    for (_, node) in &nodes {
        let Some(nobj) = node.as_object() else { continue };
        let class = nobj.get("class_type").and_then(|v| v.as_str()).unwrap_or("");
        if class != "CLIPTextEncode" {
            continue;
        }
        let inputs = nobj.get("inputs").and_then(|v| v.as_object());
        let Some(inputs) = inputs else { continue };
        if let Some(text_val) = inputs.get("text") {
            if let Some(resolved) = resolve_text_value(text_val, &nodes, 0) {
                if !positive_texts.contains(&resolved)
                    && !negative_texts.contains(&resolved)
                    && !standalone_clip_texts.contains(&resolved)
                {
                    standalone_clip_texts.push(resolved);
                }
            }
        }
    }

    // Assign: if we found positive via sampler, use those. Otherwise use standalone as positive.
    if positive_texts.is_empty() && !standalone_clip_texts.is_empty() {
        positive_texts = standalone_clip_texts.clone();
        standalone_clip_texts.clear();
    }
    // If still empty, fall back to keyword heuristic on standalone.
    if positive_texts.is_empty() && negative_texts.is_empty() && !standalone_clip_texts.is_empty() {
        let neg_markers = [
            "negative", "bad", "worst", "low quality", "blurry", "deformed", "ugly",
        ];
        let mut pos = Vec::new();
        let mut neg = Vec::new();
        for text in &standalone_clip_texts {
            let lower = text.to_lowercase();
            if neg_markers.iter().any(|m| lower.contains(*m)) && !pos.is_empty() {
                neg.push(text.clone());
            } else if pos.is_empty() {
                pos.push(text.clone());
            } else if neg.is_empty() {
                neg.push(text.clone());
            } else {
                pos.push(text.clone());
            }
        }
        positive_texts = pos;
        negative_texts = neg;
    }

    meta.prompt_pos = positive_texts.join("\n");
    meta.prompt_neg = negative_texts.join("\n");

    // Third pass: loaders.
    for (_, node) in &nodes {
        let Some(nobj) = node.as_object() else { continue };
        let class = nobj.get("class_type").and_then(|v| v.as_str()).unwrap_or("");
        let inputs = nobj.get("inputs").and_then(|v| v.as_object());
        let Some(inputs) = inputs else { continue };
        match class {
            "CheckpointLoaderSimple" | "UNETLoader" | "CheckpointLoader" | "UnetLoaderGGUF" => {
                if let Some(name) = inputs
                    .get("unet_name")
                    .and_then(|v| v.as_str())
                    .or_else(|| inputs.get("ckpt_name").and_then(|v| v.as_str()))
                {
                    if meta.checkpoint.is_empty() {
                        meta.checkpoint = name.to_string();
                    }
                }
            }
            "LoraLoader" | "LoraLoaderModelOnly" => {
                if let Some(name) = inputs.get("lora_name").and_then(|v| v.as_str()) {
                    let strength = inputs
                        .get("strength_model")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(1.0);
                    meta.loras.push(LoraInfo {
                        name: name.to_string(),
                        strength,
                    });
                }
            }
            "VAELoader" | "SeedVR2LoadVAEModel" => {
                if let Some(name) = inputs
                    .get("vae_name")
                    .and_then(|v| v.as_str())
                    .or_else(|| inputs.get("model").and_then(|v| v.as_str()))
                {
                    if meta.vae.is_empty() {
                        meta.vae = name.to_string();
                    }
                }
            }
            "KSampler" | "KSamplerAdvanced" | "SamplerCustom" | "KSamplerSelect" => {
                let sampler = inputs
                    .get("sampler_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let seed = inputs
                    .get("seed")
                    .and_then(|v| v.as_i64())
                    .or_else(|| inputs.get("noise_seed").and_then(|v| v.as_i64()))
                    .unwrap_or(0);
                let steps = inputs.get("steps").and_then(|v| v.as_i64()).unwrap_or(0);
                let cfg = inputs.get("cfg").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let scheduler = inputs
                    .get("scheduler")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                meta.samplers.push(SamplerInfo {
                    sampler,
                    seed,
                    steps,
                    cfg,
                    scheduler,
                });
            }
            _ => {}
        }
    }

    meta
}

/// Trace a conditioning reference to extract text.
/// `cond_ref` is typically `["node_id", output_index]`.
/// Follows: conditioning -> CLIPTextEncode -> text -> (string | CR Text / PrimitiveNode etc.)
fn trace_conditioning_text(cond_ref: &Value, nodes: &HashMap<String, &Value>, depth: u8) -> Option<String> {
    if depth > 8 {
        return None;
    }
    let arr = cond_ref.as_array()?;
    let node_id = arr.first()?.as_str()?;
    let node = nodes.get(node_id)?;
    let nobj = node.as_object()?;
    let class = nobj.get("class_type").and_then(|v| v.as_str()).unwrap_or("");

    // ConditioningZeroOut wraps another conditioning -> no text.
    if class == "ConditioningZeroOut" {
        return None;
    }
    // CLIPTextEncode: get text input, which may be a string or a link to a text source.
    if class == "CLIPTextEncode" {
        let inputs = nobj.get("inputs").and_then(|v| v.as_object())?;
        let text_val = inputs.get("text")?;
        return resolve_text_value(text_val, nodes, depth + 1);
    }
    // Some custom conditioning nodes may have text directly.
    let inputs = nobj.get("inputs").and_then(|v| v.as_object())?;
    for key in ["text", "prompt", "positive", "negative"] {
        if let Some(v) = inputs.get(key) {
            if let Some(s) = resolve_text_value(v, nodes, depth + 1) {
                return Some(s);
            }
        }
    }
    None
}

/// Resolve a value that may be a string or a link `["node_id", idx]` to a text source node.
fn resolve_text_value(val: &Value, nodes: &HashMap<String, &Value>, depth: u8) -> Option<String> {
    if depth > 8 {
        return None;
    }
    // Direct string
    if let Some(s) = val.as_str() {
        if !s.is_empty() {
            return Some(s.to_string());
        }
        return None;
    }
    // Link reference ["node_id", output_index]
    let arr = val.as_array()?;
    let node_id = arr.first()?.as_str()?;
    let node = nodes.get(node_id)?;
    let nobj = node.as_object()?;
    let inputs = nobj.get("inputs").and_then(|v| v.as_object())?;
    // Try common text field names in text-source nodes.
    for key in ["text", "value", "string", "prompt", "content", "strings"] {
        if let Some(v) = inputs.get(key) {
            // Could be string or array of strings.
            if let Some(s) = v.as_str() {
                if !s.is_empty() {
                    return Some(s.to_string());
                }
            }
            if let Some(arr2) = v.as_array() {
                let parts: Vec<String> = arr2
                    .iter()
                    .filter_map(|item| {
                        item.as_str()
                            .map(|s| s.to_string())
                            .or_else(|| resolve_text_value(item, nodes, depth + 1))
                    })
                    .filter(|s| !s.is_empty())
                    .collect();
                if !parts.is_empty() {
                    return Some(parts.join("\n"));
                }
            }
            // Nested link
            if let Some(s) = resolve_text_value(v, nodes, depth + 1) {
                return Some(s);
            }
        }
    }
    None
}

/// Check if a conditioning reference points to ConditioningZeroOut (negative = empty).
fn is_conditioning_zero_out(cond_ref: &Value, nodes: &HashMap<String, &Value>) -> bool {
    let Some(arr) = cond_ref.as_array() else { return false };
    let Some(node_id) = arr.first().and_then(|v| v.as_str()) else { return false };
    let Some(node) = nodes.get(node_id) else { return false };
    let Some(nobj) = node.as_object() else { return false };
    nobj.get("class_type")
        .and_then(|v| v.as_str())
        .map(|s| s == "ConditioningZeroOut")
        .unwrap_or(false)
}
