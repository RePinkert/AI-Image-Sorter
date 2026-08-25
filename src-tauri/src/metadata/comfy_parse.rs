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
            "KSampler" | "KSamplerAdvanced" | "SamplerCustom" | "ClownsharKSampler_Beta"
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
            "KSampler" | "KSamplerAdvanced" | "SamplerCustom" | "KSamplerSelect"
            | "ClownsharKSampler_Beta" => {
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
    let class = nobj.get("class_type").and_then(|v| v.as_str()).unwrap_or("");
    let inputs = nobj.get("inputs").and_then(|v| v.as_object())?;

    // Switch-style nodes (ComfySwitchNode on_false/on_true/switch,
    // Any Switch (rgthree) inputN/select, ...): forward the selected branch.
    if class.contains("Switch") {
        if let Some(s) = resolve_switch_output(inputs, nodes, depth) {
            return Some(s);
        }
    }

    // Try common text field names in text-source nodes. Case-insensitive,
    // because custom nodes (e.g. AdvPromptEnhancer) use "Prompt"/"Instruction".
    for key in ["text", "value", "string", "prompt", "content", "strings"] {
        if let Some((_, v)) = inputs.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)) {
            // Could be string or array of strings.
            if let Some(s) = v.as_str() {
                if !s.is_empty() {
                    return Some(s.to_string());
                }
            }
            if let Some(arr2) = v.as_array() {
                // A link reference ["node_id", out_idx] must be followed, not joined.
                let is_link_ref = arr2
                    .first()
                    .and_then(Value::as_str)
                    .map_or(false, |s| nodes.contains_key(s));
                if !is_link_ref {
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
            }
            // Nested link
            if let Some(s) = resolve_text_value(v, nodes, depth + 1) {
                return Some(s);
            }
        }
    }
    // Passthrough nodes with a single input (e.g. PreviewAny.source).
    if inputs.len() == 1 {
        if let Some(s) = resolve_text_value(inputs.values().next().unwrap(), nodes, depth + 1) {
            return Some(s);
        }
    }
    None
}

/// The active branch of a switch node: a boolean choice (ComfySwitchNode) or
/// an input index (rgthree-style `inputN` / `select`).
enum SwitchChoice {
    On(bool),
    Index(usize),
}

/// Resolve the `switch`/`select` control of a switch node: a literal
/// boolean/index, or a link to a primitive node holding one.
fn resolve_switch_choice(
    val: &Value,
    nodes: &HashMap<String, &Value>,
    depth: u8,
) -> Option<SwitchChoice> {
    if depth > 8 {
        return None;
    }
    if let Some(b) = val.as_bool() {
        return Some(SwitchChoice::On(b));
    }
    if let Some(n) = val.as_i64() {
        return Some(SwitchChoice::Index(n.max(0) as usize));
    }
    // Link to a primitive (e.g. ["30:24", 0] -> PrimitiveBoolean "value").
    let arr = val.as_array()?;
    let node_id = arr.first()?.as_str()?;
    let node = nodes.get(node_id)?;
    let inputs = node.get("inputs").and_then(|v| v.as_object())?;
    for key in ["value", "switch", "select"] {
        if let Some((_, v)) = inputs.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)) {
            if let Some(b) = v.as_bool() {
                return Some(SwitchChoice::On(b));
            }
            if let Some(n) = v.as_i64() {
                return Some(SwitchChoice::Index(n.max(0) as usize));
            }
        }
    }
    None
}

/// Resolve the text produced by a switch node's selected branch.
fn resolve_switch_output(
    inputs: &serde_json::Map<String, Value>,
    nodes: &HashMap<String, &Value>,
    depth: u8,
) -> Option<String> {
    let choice = ["switch", "select", "selector", "active"]
        .iter()
        .find_map(|k| {
            inputs
                .iter()
                .find(|(k2, _)| k2.eq_ignore_ascii_case(k))
                .and_then(|(_, v)| resolve_switch_choice(v, nodes, depth))
        });
    match choice {
        Some(SwitchChoice::On(true)) => {
            resolve_input_text(inputs, &["on_true", "on_false"], nodes, depth)
        }
        Some(SwitchChoice::On(false)) => {
            resolve_input_text(inputs, &["on_false", "on_true"], nodes, depth)
        }
        Some(SwitchChoice::Index(n)) => {
            let key = format!("input{n}");
            resolve_input_text(inputs, &[key.as_str()], nodes, depth)
        }
        None => resolve_input_text(
            inputs,
            &[
                "on_false", "on_true", "input0", "input1", "input2", "input3", "input4", "input5",
                "input6", "input7", "input8", "input9",
            ],
            nodes,
            depth,
        ),
    }
}

/// Resolve the first of the given input names that yields text.
fn resolve_input_text(
    inputs: &serde_json::Map<String, Value>,
    keys: &[&str],
    nodes: &HashMap<String, &Value>,
    depth: u8,
) -> Option<String> {
    for key in keys {
        if let Some((_, v)) = inputs.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)) {
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

    /// Mirror of a subgraph-based workflow flattened into an API prompt by the
    /// ComfyUI frontend (node ids become "parent:inner", e.g. "30:6"). The
    /// positive text runs through ComfySwitchNode selected by a PrimitiveBoolean.
    const V10_FLATTENED_SWITCH: &str = r#"{
        "30:10": {"class_type": "UNETLoader", "inputs": {"unet_name": "krea2_turbo_fp8_scaled.safetensors", "weight_dtype": "default"}},
        "30:11": {"class_type": "CLIPLoader", "inputs": {"clip_name": "qwen3vl_4b_fp8_scaled.safetensors", "type": "krea2"}},
        "30:12": {"class_type": "VAELoader", "inputs": {"vae_name": "qwen_image_vae.safetensors"}},
        "30:15": {"class_type": "LoraLoaderModelOnly", "inputs": {"model": ["30:10", 0], "lora_name": "Krea2/Krea2 NSFW+.safetensors", "strength_model": 0.85}},
        "30:19": {"class_type": "PrimitiveStringMultiline", "inputs": {"value": "a Chinese schoolgirl jumping rope on the sports field"}},
        "30:24": {"class_type": "PrimitiveBoolean", "inputs": {"value": false}},
        "30:21": {"class_type": "ComfySwitchNode", "inputs": {"on_false": ["30:19", 0], "on_true": ["30:54", 0], "switch": ["30:24", 0]}},
        "30:54": {"class_type": "PreviewAny", "inputs": {"source": ["30:55", 0]}},
        "30:55": {"class_type": "AdvPromptEnhancer", "inputs": {"Instruction": "You are an expert NSFW prompt engineer", "Prompt": "a Chinese schoolgirl jumping rope"}},
        "30:13": {"class_type": "ConditioningZeroOut", "inputs": {"conditioning": ["30:6", 0]}},
        "30:6": {"class_type": "CLIPTextEncode", "inputs": {"clip": ["30:11", 0], "text": ["30:21", 0]}},
        "30:5": {"class_type": "EmptyLatentImage", "inputs": {"width": ["49", 0], "height": ["49", 1]}},
        "30:23": {"class_type": "PrimitiveBoolean", "inputs": {"value": true}},
        "30:22": {"class_type": "ComfySwitchNode", "inputs": {"on_false": ["30:10", 0], "on_true": ["30:15", 0], "switch": ["30:23", 0]}},
        "30:3": {"class_type": "KSampler", "inputs": {"model": ["30:22", 0], "positive": ["30:6", 0], "negative": ["30:13", 0], "latent_image": ["30:5", 0], "seed": 1, "steps": 10, "cfg": 1.0, "sampler_name": "euler", "scheduler": "beta57"}},
        "30:8": {"class_type": "VAEDecode", "inputs": {"samples": ["30:3", 0], "vae": ["30:12", 0]}},
        "49": {"class_type": "ResolutionSelector", "inputs": {"resolution": "720x1280 (9:16)"}},
        "29": {"class_type": "SaveImage", "inputs": {"images": ["30:8", 0]}}
    }"#;

    fn node_map_from_root(root: &Value) -> HashMap<String, &Value> {
        root.as_object()
            .unwrap()
            .iter()
            .map(|(id, node)| (id.clone(), node))
            .collect()
    }

    #[test]
    fn flattened_subgraph_switch_false_resolves_user_prompt() {
        let meta = parse_comfy_workflow(V10_FLATTENED_SWITCH);
        assert!(meta.raw_ok, "flattened subgraph prompt must parse");
        assert!(meta.prompt_pos.contains("schoolgirl jumping rope on the sports field"));
        assert!(meta.prompt_pos.contains("schoolgirl"), "prompt: {}", meta.prompt_pos);
        assert_eq!(meta.prompt_neg, "", "negative is zeroed via ConditioningZeroOut");
        assert_eq!(meta.checkpoint, "krea2_turbo_fp8_scaled.safetensors");
        assert_eq!(meta.diffusion_model, "krea2_turbo_fp8_scaled.safetensors");
        assert_eq!(meta.loras.len(), 1);
        assert_eq!(meta.loras[0].name, "Krea2/Krea2 NSFW+.safetensors");
        assert_eq!(meta.vae, "qwen_image_vae.safetensors");
        assert_eq!(meta.samplers.len(), 1);
        assert_eq!(meta.samplers[0].sampler, "euler");
        assert!(!meta.workflow_key.is_empty());
    }

    #[test]
    fn flattened_subgraph_switch_true_uses_on_true_branch() {
        // Inline literal `true` (frontend inlines primitives) -> must pick on_true.
        let inline = V10_FLATTENED_SWITCH.replace(r#""switch": ["30:24", 0]"#, r#""switch": true"#);
        let meta = parse_comfy_workflow(&inline);
        assert!(meta.prompt_pos.contains("schoolgirl"), "on_true path via PreviewAny -> AdvPromptEnhancer.Prompt");
        assert!(!meta.prompt_pos.contains("expert NSFW prompt engineer"), "must not pick the system Instruction");
    }

    #[test]
    fn model_switch_does_not_produce_text() {
        let root: Value = serde_json::from_str(V10_FLATTENED_SWITCH).unwrap();
        let nodes = node_map_from_root(&root);
        let model_out = serde_json::json!(["30:22", 0]);
        assert!(
            resolve_text_value(&model_out, &nodes, 0).is_none(),
            "MODEL-typed switch must not resolve to text"
        );
    }

    #[test]
    fn rgthree_switch_selects_input_by_index() {
        let graph = r#"{
            "1": {"class_type": "Any Switch (rgthree)", "inputs": {"input0": "first option", "input1": "second option", "select": 1}},
            "2": {"class_type": "CLIPTextEncode", "inputs": {"clip": ["5", 0], "text": ["1", 0]}},
            "3": {"class_type": "KSampler", "inputs": {"positive": ["2", 0], "model": ["5", 0], "seed": 0}}
        }"#;
        let meta = parse_comfy_workflow(graph);
        assert_eq!(meta.prompt_pos, "second option");
    }

    #[test]
    fn case_insensitive_known_keys_pick_prompt_over_instruction() {
        let root: Value = serde_json::from_str(V10_FLATTENED_SWITCH).unwrap();
        let nodes = node_map_from_root(&root);
        let enhancer_out = serde_json::json!(["30:55", 0]);
        let text = resolve_text_value(&enhancer_out, &nodes, 0).expect("must resolve");
        assert_eq!(text, "a Chinese schoolgirl jumping rope");
    }

    #[test]
    fn single_input_passthrough_node_resolves() {
        let graph = r#"{
            "1": {"class_type": "PreviewAny", "inputs": {"source": ["2", 0]}},
            "2": {"class_type": "CR Text", "inputs": {"text": "hello from cr text"}}
        }"#;
        let root: Value = serde_json::from_str(graph).unwrap();
        let nodes = node_map_from_root(&root);
        let out = serde_json::json!(["1", 0]);
        assert_eq!(resolve_text_value(&out, &nodes, 0).as_deref(), Some("hello from cr text"));
    }

    #[test]
    fn clownshark_sampler_beta_extracts_sampler_info() {
        let graph = r#"{
            "127:116": {"class_type": "ClownsharKSampler_Beta", "inputs": {
                "sampler_name": "linear/euler", "scheduler": "simple", "steps": 8, "cfg": 1.0,
                "seed": 1003949590422403, "model": ["21", 0], "positive": ["127:115", 0]
            }}
        }"#;
        let meta = parse_comfy_workflow(graph);
        assert!(meta.raw_ok);
        assert_eq!(meta.samplers.len(), 1, "ClownsharKSampler_Beta must be recognized");
        let s = &meta.samplers[0];
        assert_eq!(s.sampler, "linear/euler");
        assert_eq!(s.scheduler, "simple");
        assert_eq!(s.steps, 8);
        assert!((s.cfg - 1.0).abs() < 1e-9);
        assert_eq!(s.seed, 1003949590422403);
    }

    #[test]
    fn unresolvable_switch_control_falls_back_to_on_false() {
        // Mirrors the CivitAI workflow: switch driven by a ComfyOrNode whose
        // value cannot be determined from the saved prompt -> must resolve
        // on_false (the user prompt) instead of returning nothing.
        let graph = r#"{
            "46": {"class_type": "PrimitiveStringMultiline", "inputs": {"value": "a girl by a dark gray Audi at golden hour"}},
            "56:49": {"class_type": "ImpactIfNone", "inputs": {}},
            "56:48": {"class_type": "ImpactIfNone", "inputs": {}},
            "56:50": {"class_type": "ComfyOrNode", "inputs": {"values.value0": ["56:49", 1], "values.value1": ["56:48", 1]}},
            "56:54": {"class_type": "ComfySwitchNode", "inputs": {"switch": ["56:50", 0], "on_false": ["46", 0], "on_true": ["56:55", 0]}},
            "56:55": {"class_type": "TextGenerate", "inputs": {"prompt": ["56:52", 0]}},
            "56:52": {"class_type": "JoinStrings", "inputs": {"string1": ["56:51", 0], "string2": ["46", 0]}},
            "56:51": {"class_type": "PrimitiveStringMultiline", "inputs": {"value": "You are an image prompt engineer"}},
            "127:115": {"class_type": "CLIPTextEncode", "inputs": {"clip": ["6", 0], "text": ["56:54", 0]}}
        }"#;
        let meta = parse_comfy_workflow(graph);
        assert!(meta.prompt_pos.contains("Audi"), "prompt: {}", meta.prompt_pos);
        assert!(!meta.prompt_pos.contains("prompt engineer"), "must not pick the system prompt");
    }

    /// Mirrors the real Krea2 workflow: the positive text runs through a
    /// RegexExtract whose `string` input is a link reference to a
    /// ComfySwitchNode driven by an unresolvable ComfyOrNode. The link
    /// reference must be followed (falling back to on_false), not joined
    /// element-wise (which would leak the node id "56:54" as text).
    const KREA2_ENHANCED_CHAIN: &str = r#"{
        "46": {"class_type": "PrimitiveStringMultiline", "inputs": {"value": "ToonDaal, a young girl with icy white hair"}},
        "56:48": {"class_type": "ImpactIfNone", "inputs": {}},
        "56:49": {"class_type": "ImpactIfNone", "inputs": {}},
        "56:50": {"class_type": "ComfyOrNode", "inputs": {"values.value0": ["56:49", 1], "values.value1": ["56:48", 1]}},
        "56:54": {"class_type": "ComfySwitchNode", "inputs": {"switch": ["56:50", 0], "on_false": ["46", 0], "on_true": ["56:55", 0]}},
        "56:55": {"class_type": "TextGenerate", "inputs": {"prompt": ["56:52", 0]}},
        "187": {"class_type": "RegexExtract", "inputs": {"string": ["56:54", 0], "regex_pattern": "(?s)(?:\\*\\*Final Prompt:\\*\\*\\s*)?(.*)", "mode": "First Group", "case_insensitive": true, "multiline": false, "dotall": true, "group_index": 1}},
        "127:114": {"class_type": "ConditioningZeroOut", "inputs": {"conditioning": ["127:115", 0]}},
        "127:115": {"class_type": "CLIPTextEncode", "inputs": {"clip": ["6", 0], "text": ["187", 0]}},
        "127:116": {"class_type": "ClownsharKSampler_Beta", "inputs": {"model": ["21", 0], "positive": ["127:115", 0], "negative": ["127:114", 0], "latent_image": ["11", 0], "seed": 1, "steps": 8, "cfg": 1.0, "sampler_name": "linear/euler", "scheduler": "beta"}}
    }"#;

    #[test]
    fn link_reference_input_is_followed_not_joined() {
        let meta = parse_comfy_workflow(KREA2_ENHANCED_CHAIN);
        assert!(meta.raw_ok);
        assert!(meta.prompt_pos.contains("ToonDaal"), "prompt: {}", meta.prompt_pos);
        assert!(!meta.prompt_pos.contains("56:54"), "must not leak node id: {}", meta.prompt_pos);
    }
}
