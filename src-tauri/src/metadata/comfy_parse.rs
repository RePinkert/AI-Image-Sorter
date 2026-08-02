use serde_json::Value;
use std::collections::HashMap;
use xxhash_rust::xxh3::xxh3_64;

use super::{ImageMeta, LoraInfo, ModelChainItem, SamplerInfo, SourceKind};

/// Some ComfyUI workflows serialize invalid JSON values into the embedded
/// API prompt (`"is_changed": NaN`, `Infinity`, `-Infinity`). serde_json is
/// strict and rejects the whole document, which previously discarded ALL
/// metadata for those images. This scanner rewrites those bare tokens to
/// `null` while leaving string contents untouched.
pub fn sanitize_json(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut in_string = false;
    let mut i = 0usize;
    while i < b.len() {
        let c = b[i];
        if in_string {
            out.push(c as char);
            if c == b'\\' && i + 1 < b.len() {
                out.push(b[i + 1] as char);
                i += 2;
                continue;
            }
            if c == b'"' {
                in_string = false;
            }
            i += 1;
        } else if c == b'"' {
            in_string = true;
            out.push('"');
            i += 1;
        } else {
            let ident_len = if c == b'N' && b[i..].starts_with(b"NaN") {
                Some(3)
            } else if c == b'I' && b[i..].starts_with(b"Infinity") {
                Some(8)
            } else if c == b'-' && b[i..].starts_with(b"-Infinity") {
                Some(9)
            } else {
                None
            };
            if let Some(len) = ident_len {
                let prev_ok = i == 0 || !is_ident_char(b[i - 1]);
                let next_ok = i + len >= b.len() || !is_ident_char(b[i + len]);
                if prev_ok && next_ok {
                    out.push_str("null");
                    i += len;
                    continue;
                }
            }
            out.push(c as char);
            i += 1;
        }
    }
    out
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// Canonical topology from an API-format prompt graph: sorted node class
/// multiset + sorted class-pair edges. Node ids and ALL parameter values
/// (model names, LoRA names, seeds, …) are ignored, so switching the
/// diffusion model / LoRA inside the same pipeline yields the same key.
pub fn topology_from_api(root: &Value) -> Option<String> {
    let obj = root.as_object()?;
    let mut types: Vec<String> = Vec::new();
    let mut cls_by_id: HashMap<&str, &str> = HashMap::new();
    for (id, node) in obj {
        if let Some(t) = node.get("class_type").and_then(Value::as_str) {
            if !t.is_empty() {
                types.push(t.to_string());
                cls_by_id.insert(id.as_str(), t);
            }
        }
    }
    let mut edges: Vec<String> = Vec::new();
    for node in obj.values() {
        let Some(cls) = node.get("class_type").and_then(Value::as_str) else {
            continue;
        };
        let Some(inputs) = node.get("inputs").and_then(Value::as_object) else {
            continue;
        };
        for v in inputs.values() {
            if let Some(arr) = v.as_array() {
                if let Some(src_id) = arr.first().and_then(Value::as_str) {
                    if let Some(src_cls) = cls_by_id.get(src_id) {
                        edges.push(format!("{}->{}", src_cls, cls));
                    }
                }
            }
        }
    }
    types.sort();
    edges.sort();
    Some(serde_json::json!({ "t": types, "e": edges }).to_string())
}

/// Canonical topology from a UI-format workflow JSON (nodes + links arrays).
/// Produces the same canonical form as `topology_from_api` so template files
/// (ComfyUI `user/*/workflows/*.json`) and embedded API prompts compare
/// directly.
pub fn topology_from_ui(root: &Value) -> Option<String> {
    let nodes = root.get("nodes")?.as_array()?;
    let mut types: Vec<String> = Vec::new();
    let mut cls_by_id: HashMap<i64, String> = HashMap::new();
    for node in nodes {
        let t = node.get("type").and_then(Value::as_str).unwrap_or("");
        if t.is_empty() {
            continue;
        }
        types.push(t.to_string());
        if let Some(id) = node.get("id").and_then(Value::as_i64) {
            cls_by_id.insert(id, t.to_string());
        }
    }
    let mut edges: Vec<String> = Vec::new();
    if let Some(links) = root.get("links").and_then(Value::as_array) {
        for link in links {
            let Some(items) = link.as_array() else { continue };
            if items.len() < 6 {
                continue;
            }
            let (Some(src), Some(dst)) = (
                cls_by_id.get(&items[1].as_i64().unwrap_or_default()),
                cls_by_id.get(&items[3].as_i64().unwrap_or_default()),
            ) else {
                continue;
            };
            edges.push(format!("{}->{}", src, dst));
        }
    }
    types.sort();
    edges.sort();
    Some(serde_json::json!({ "t": types, "e": edges }).to_string())
}

pub fn workflow_key_from_canonical(canonical: &str) -> String {
    format!("{:016x}", xxh3_64(canonical.as_bytes()))
}

pub fn parse_comfy_workflow(json_str: &str) -> ImageMeta {
    let mut meta = ImageMeta {
        source_kind: SourceKind::Comfy,
        ..Default::default()
    };
    let root: Value = match serde_json::from_str(&sanitize_json(json_str)) {
        Ok(v) => v,
        Err(_) => return meta,
    };
    let obj = match root.as_object() {
        Some(o) => o,
        None => return meta,
    };
    meta.raw_ok = true;

    if let Some(canonical) = topology_from_api(&root) {
        meta.workflow_graph_json = canonical.clone();
        meta.workflow_key = workflow_key_from_canonical(&canonical);
    }

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
                    if meta.checkpoint.is_empty() { meta.checkpoint = name.to_string(); }
                    if meta.diffusion_model.is_empty() { meta.diffusion_model = name.to_string(); }
                    meta.model_chain.push(ModelChainItem {
                        node_type: class.to_string(), model: name.to_string(), strength: None, enabled: None,
                    });
                }
            }
            "LoraLoader" | "LoraLoaderModelOnly" => {
                if let Some(name) = inputs.get("lora_name").and_then(|v| v.as_str()) {
                    let strength = inputs
                        .get("strength_model")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(1.0);
                    let clip_strength = inputs.get("strength_clip").and_then(|v| v.as_f64());
                    meta.loras.push(LoraInfo {
                        name: name.to_string(),
                        strength,
                        clip_strength,
                    });
                    meta.model_chain.push(ModelChainItem {
                        node_type: class.to_string(), model: name.to_string(), strength: Some(strength), enabled: None,
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
            _ if class.contains("Model") || class.contains("Enhancer") => {
                let model = inputs.get("model_name").or_else(|| inputs.get("model"))
                    .and_then(|v| v.as_str()).unwrap_or("");
                if !model.is_empty() {
                    meta.model_chain.push(ModelChainItem {
                        node_type: class.to_string(), model: model.to_string(),
                        strength: inputs.get("strength").and_then(|v| v.as_f64()),
                        enabled: inputs.get("enabled").and_then(|v| v.as_bool()),
                    });
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

    meta.refresh_generation_recipe();
    meta
}

/// Parse only the topology of a UI-format workflow JSON. Used as a fallback
/// when a PNG embeds the `workflow` chunk but no (parseable) `prompt` chunk,
/// so the image still gets a workflow identity even though per-node
/// metadata (prompt text, models) cannot be extracted.
pub fn parse_comfy_ui_topology(json_str: &str) -> Option<(String, String)> {
    let root: Value = serde_json::from_str(&sanitize_json(json_str)).ok()?;
    let canonical = topology_from_ui(&root)?;
    let key = workflow_key_from_canonical(&canonical);
    Some((key, canonical))
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

#[cfg(test)]
mod tests {
    use super::*;

    const API_GRAPH: &str = r#"{
        "1": {"class_type": "UNETLoader", "inputs": {"unet_name": "moodyCutieMixKrea2_v30.safetensors"}, "_meta": {"title": "Load Diffusion Model", "changed": NaN}},
        "2": {"class_type": "LoraLoader", "inputs": {"model": ["1", 0], "lora_name": "Krea 2 NSFW.safetensors", "strength_model": 1.0}, "is_changed": NaN},
        "3": {"class_type": "CLIPTextEncode", "inputs": {"clip": ["2", 0], "text": "a beautiful girl"}},
        "4": {"class_type": "KSampler", "inputs": {"model": ["2", 0], "positive": ["3", 0], "seed": 123, "steps": 10, "cfg": 1.0, "sampler_name": "euler"}}
    }"#;

    const UI_GRAPH: &str = r#"{
        "nodes": [
            {"id": 1, "type": "UNETLoader", "widgets_values": ["moodyCutieMixKrea2_v30.safetensors"]},
            {"id": 2, "type": "LoraLoader", "widgets_values": ["Krea 2 NSFW.safetensors", 1.0]},
            {"id": 3, "type": "CLIPTextEncode"},
            {"id": 4, "type": "KSampler"}
        ],
        "links": [
            [0, 1, 0, 2, 0, "MODEL"],
            [1, 2, 0, 3, 0, "CLIP"],
            [2, 2, 1, 4, 0, "MODEL"],
            [3, 3, 0, 4, 1, "CONDITIONING"]
        ]
    }"#;

    #[test]
    fn lenient_parse_recovers_nan_workflow() {
        let meta = parse_comfy_workflow(API_GRAPH);
        assert!(meta.raw_ok, "workflow with NaN must parse after sanitizing");
        assert_eq!(meta.checkpoint, "moodyCutieMixKrea2_v30.safetensors");
        assert_eq!(meta.diffusion_model, "moodyCutieMixKrea2_v30.safetensors");
        assert_eq!(meta.loras.len(), 1);
        assert_eq!(meta.loras[0].name, "Krea 2 NSFW.safetensors");
        assert_eq!(meta.prompt_pos, "a beautiful girl");
        assert!(!meta.workflow_key.is_empty());
        assert!(!meta.workflow_graph_json.is_empty());
    }

    #[test]
    fn sanitizer_ignores_strings() {
        let out = sanitize_json(r#"{"a": "NaN is fine", "b": NaN, "c": [-Infinity, Infinity], "d": "x: NaN"}"#);
        let v: Value = serde_json::from_str(&out).expect("sanitized json must parse");
        assert_eq!(v["a"], "NaN is fine");
        assert_eq!(v["d"], "x: NaN");
        assert!(v["b"].is_null());
        assert_eq!(v["c"][0], Value::Null);
        assert_eq!(v["c"][1], Value::Null);
    }

    #[test]
    fn api_and_ui_topology_are_identical() {
        let api: Value = serde_json::from_str(&sanitize_json(API_GRAPH)).unwrap();
        let ui: Value = serde_json::from_str(UI_GRAPH).unwrap();
        let api_canonical = topology_from_api(&api).expect("api topology");
        let ui_canonical = topology_from_ui(&ui).expect("ui topology");
        assert_eq!(api_canonical, ui_canonical);
        assert_eq!(
            workflow_key_from_canonical(&api_canonical),
            workflow_key_from_canonical(&ui_canonical)
        );
    }

    #[test]
    fn topology_ignores_model_values() {
        let meta_a = parse_comfy_workflow(API_GRAPH);
        let swapped = API_GRAPH.replace("moodyCutieMixKrea2_v30.safetensors", "krea2_turbo_fp8.safetensors");
        let meta_b = parse_comfy_workflow(&swapped);
        assert_eq!(meta_a.workflow_key, meta_b.workflow_key, "switching models must keep workflow identity");
    }
}
