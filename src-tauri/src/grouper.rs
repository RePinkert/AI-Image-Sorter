use crate::metadata::ImageMeta;
use xxhash_rust::xxh3::xxh3_64;

/// The six single-selection dimensions of the L1 (Model偏差) fingerprint.
/// Exactly ONE dimension participates at a time (the user wheel-selects it);
/// grouping is an EXACT match on the parameter value — no stem stripping,
/// no extension normalization, no strength quantization, no fuzzy guess.
pub const DIMENSIONS: [&str; 6] = [
    "base_model",
    "text_encoder",
    "vae",
    "lora",
    "embedding",
    "controlnet",
];

pub const DEFAULT_DIMENSION: &str = "base_model";

/// View-level group key prefix for the per-LoRA SINGLE membership listing.
/// Unlike the other dimensions this listing is DERIVED read-only from the
/// `images.loras` JSON (one key per LoRA file name) rather than stored in
/// `group_key_l1`, so a stacked image appears in every LoRA folder it
/// belongs to. Never persisted; `group_key_l1` keeps the set-based
/// fingerprint untouched.
pub const LORA_SINGLE_PREFIX: &str = "lora-single:";
/// Sentinel bucket for images without any LoRA in the single listing.
pub const LORA_SINGLE_UNKNOWN: &str = "(unknown)";

pub fn lora_single_key(lora_name: &str) -> String {
    format!("{}{}", LORA_SINGLE_PREFIX, lora_name)
}

/// Parse a per-LoRA view key back into the LoRA file name (or `None` for the
/// unknown bucket).
pub fn lora_single_name(key: &str) -> Option<&str> {
    key.strip_prefix(LORA_SINGLE_PREFIX)
        .filter(|n| !n.is_empty() && *n != LORA_SINGLE_UNKNOWN)
}

/// True when the key addresses the per-LoRA membership listing (either a
/// real LoRA folder or the unknown bucket).
pub fn is_lora_single_key(key: &str) -> bool {
    key.starts_with(LORA_SINGLE_PREFIX)
}

pub struct GroupKeys {
    pub l0: String,
    pub l1: String,
    pub l2: String,
    pub l3: String,
}

/// Raw per-image model-parameter values fed into the dimension fingerprint.
pub struct ModelParams {
    /// diffusion_model, falling back to checkpoint (the primary generative
    /// model file, exact on-disk string).
    pub base_model: String,
    pub text_encoder: String,
    pub vae: String,
    /// LoRA FILE NAMES only (sorted, deduped); strengths deliberately
    /// excluded — the same LoRA at any strength is the same model condition.
    pub lora_keys: Vec<String>,
    /// Embedding / textual-inversion names referenced by the image
    /// (`embedding:name` tokens or A1111 `TI hashes`), sorted, deduped.
    pub embeddings: Vec<String>,
    pub controlnet: String,
}

/// A stable unknown-bucket value so images lacking the dimension's
/// parameter still land in exactly one group per source view.
pub const UNKNOWN_VALUE: &str = "(unknown)";

/// Compute the fingerprint for ONE dimension from raw values. Exact match:
/// whatever string is fed here defines group membership byte-for-byte.
pub fn l1_fingerprint(dimension: &str, params: &ModelParams) -> String {
    let value: Vec<String> = match dimension {
        "base_model" => vec![if params.base_model.is_empty() {
            UNKNOWN_VALUE.to_string()
        } else {
            params.base_model.clone()
        }],
        "text_encoder" => vec![if params.text_encoder.is_empty() {
            UNKNOWN_VALUE.to_string()
        } else {
            params.text_encoder.clone()
        }],
        "vae" => vec![if params.vae.is_empty() {
            UNKNOWN_VALUE.to_string()
        } else {
            params.vae.clone()
        }],
        "lora" => {
            if params.lora_keys.is_empty() {
                vec![UNKNOWN_VALUE.to_string()]
            } else {
                params.lora_keys.clone()
            }
        }
        "embedding" => {
            if params.embeddings.is_empty() {
                vec![UNKNOWN_VALUE.to_string()]
            } else {
                params.embeddings.clone()
            }
        }
        "controlnet" => vec![if params.controlnet.is_empty() {
            UNKNOWN_VALUE.to_string()
        } else {
            params.controlnet.clone()
        }],
        _ => vec![UNKNOWN_VALUE.to_string()],
    };
    let mut buf = String::from("model-deviation:v2");
    buf.push('\x01');
    buf.push_str(dimension);
    buf.push('\x01');
    for v in &value {
        buf.push_str(v);
        buf.push('\x1f');
    }
    format!("{:016x}", xxh3_64(buf.as_bytes()))
}

/// Build `ModelParams` from freshly parsed metadata.
pub fn model_params_from_meta(meta: &ImageMeta) -> ModelParams {
    let base_model = if meta.diffusion_model.is_empty() {
        meta.checkpoint.clone()
    } else {
        meta.diffusion_model.clone()
    };
    let mut lora_keys: Vec<String> = meta
        .loras
        .iter()
        .map(|l| l.name.trim().to_string())
        .filter(|n| !n.is_empty())
        .collect();
    lora_keys.sort();
    lora_keys.dedup();
    let mut embeddings = meta.embeddings.clone();
    embeddings.sort();
    embeddings.dedup();
    ModelParams {
        base_model,
        text_encoder: meta.text_encoder.clone(),
        vae: meta.vae.clone(),
        lora_keys,
        embeddings,
        controlnet: meta.controlnet.clone(),
    }
}

/// Human-facing summary of one dimension's raw value (used by labels).
pub struct ModelParamLabel {
    pub base_model: String,
    pub text_encoder: String,
    pub vae: String,
    pub lora_count: usize,
    pub lora_names: Vec<String>,
    pub embeddings: Vec<String>,
    pub controlnet: String,
}

pub fn dimension_label(dimension: &str, m: &ModelParamLabel) -> String {
    let unknown = |v: &str| -> String {
        if v.is_empty() {
            UNKNOWN_VALUE.to_string()
        } else {
            v.to_string()
        }
    };
    match dimension {
        "base_model" => unknown(&m.base_model),
        "text_encoder" => unknown(&m.text_encoder),
        "vae" => unknown(&m.vae),
        "lora" => {
            if m.lora_names.is_empty() {
                UNKNOWN_VALUE.to_string()
            } else if m.lora_count == 1 {
                m.lora_names[0].clone()
            } else {
                format!("{} (×{})", m.lora_names.join(" + "), m.lora_count)
            }
        }
        "embedding" => {
            if m.embeddings.is_empty() {
                UNKNOWN_VALUE.to_string()
            } else {
                let names = m.embeddings.clone();
                if names.len() == 1 {
                    names.join("")
                } else {
                    format!("{} (×{})", names.join(" + "), names.len())
                }
            }
        }
        "controlnet" => unknown(&m.controlnet).to_string(),
        _ => UNKNOWN_VALUE.to_string(),
    }
}

/// Compute the 4-level group keys.
/// L0 = source folder (passed in, not computed from meta)
/// L1 = Model偏差: EXACT fingerprint of the single wheel-selected model
///      parameter dimension (see `DIMENSIONS`). Workflow identity is NOT
///      part of L1 anymore — it remains available on ImageMeta for
///      template matching / details display only.
/// L2 = prompt similarity cluster — placeholder here (== L1); assigned
///      post-scan by `clustering::recluster_l2` using real Jaccard
///      similarity within the same L1 so groups stop over-splitting.
/// L3 = individual prompt (L1 + full prompt_pos)
/// Negative prompt never participates.
pub fn compute_group_keys(meta: &ImageMeta, source_path: &str, dimension: &str) -> GroupKeys {
    // L0: source folder
    let l0 = format!("{:016x}", xxh3_64(source_path.as_bytes()));

    let params = model_params_from_meta(meta);
    let l1 = l1_fingerprint(dimension, &params);

    // L2 placeholder = L1 until recluster_l2 runs. Keeps pre-cluster L2
    // view coherent (one bucket per model condition) rather than bogus
    // prefixes.
    let l2 = l1.clone();

    // L3: individual prompt = L1 fingerprint + full prompt_pos
    let prompt_normalized = normalize_prompt(&meta.prompt_pos);
    let mut l3_buf = format!("model-deviation:v2\x01{}\x01{}", dimension, l1);
    l3_buf.push('\x03');
    l3_buf.push_str(&prompt_normalized);
    let l3 = format!("{:016x}", xxh3_64(l3_buf.as_bytes()));

    GroupKeys { l0, l1, l2, l3 }
}

/// Extract `embedding:name` references from a prompt body (Comfy custom
/// packs and A1111-style text both spell it this way). Names run to the
/// next separator and one trailing `.` is dropped. Pure extraction — no
/// guessing about the value itself.
pub fn extract_embedding_refs(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let lower = text.to_lowercase();
    let needle = "embedding:";
    let mut search_from = 0usize;
    while let Some(rel) = lower[search_from..].find(needle) {
        let start = search_from + rel + needle.len();
        // A word boundary must precede the marker.
        let boundary_ok = start == needle.len()
            || !text.as_bytes()[start - needle.len() - 1].is_ascii_alphanumeric();
        if boundary_ok {
            let rest = &text[start..];
            let end = rest
                .find(|c: char| c == ',' || c == '\n' || c == '\r' || c == '(' || c == ')' || c == ';')
                .unwrap_or(rest.len());
            let mut name = rest[..end].trim().to_string();
            while name.ends_with('.') {
                name.pop();
            }
            name = name.trim().to_string();
            if !name.is_empty() && !out.contains(&name) {
                out.push(name);
            }
        }
        search_from = start + rest_len_after_marker(&text[start..]);
    }
    out
}

fn rest_len_after_marker(rest: &str) -> usize {
    rest.find(|c: char| c == ',' || c == '\n' || c == '\r' || c == '(' || c == ')' || c == ';')
        .unwrap_or(rest.len())
}

/// Normalize a prompt for tokenization (used by both grouper L3 and
/// clustering.rs L2 Jaccard so the two stay consistent).
pub fn normalize_prompt(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::{ImageMeta, LoraInfo};

    fn meta(dm: &str, loras: &[(&str, f64)]) -> ImageMeta {
        let mut m = ImageMeta::default();
        m.diffusion_model = dm.to_string();
        for (n, s) in loras {
            m.loras.push(LoraInfo {
                name: n.to_string(),
                strength: *s,
                clip_strength: None,
            });
        }
        m
    }

    #[test]
    fn base_model_exact_match_tiny_fragment() {
        let a = compute_group_keys(&meta("modelA.safetensors", &[]), "src", "base_model");
        let b = compute_group_keys(&meta("modelA.safetensors", &[]), "src", "base_model");
        let c = compute_group_keys(&meta("modelA", &[]), "src", "base_model");
        assert_eq!(a.l1, b.l1);
        assert_ne!(a.l1, c.l1, "any byte difference must split groups");
    }

    #[test]
    fn lora_strength_is_not_part_of_fingerprint() {
        let a = compute_group_keys(&meta("m.safetensors", &[("lora1.safetensors", -2.0)]), "src", "lora");
        let b = compute_group_keys(&meta("m.safetensors", &[("lora1.safetensors", -2.15)]), "src", "lora");
        let c = compute_group_keys(&meta("m.safetensors", &[("lora1.safetensors", -2.0), ("lora2.safetensors", 1.0)]), "src", "lora");
        assert_eq!(a.l1, b.l1, "same LoRA file names must group regardless of strength");
        assert_ne!(a.l1, c.l1, "a different lora name set must split");
    }

    #[test]
    fn lora_order_is_normalized() {
        let a = compute_group_keys(&meta("m", &[("a", 1.0), ("b", 1.0)]), "src", "lora");
        let b = compute_group_keys(&meta("m", &[("b", 1.0), ("a", 1.0)]), "src", "lora");
        assert_eq!(a.l1, b.l1);
    }

    #[test]
    fn empty_values_share_unknown_bucket() {
        let a = compute_group_keys(&meta("", &[]), "src", "base_model");
        let b = compute_group_keys(&meta("whatever", &[]), "src2", "text_encoder");
        let c = compute_group_keys(&meta("", &[]), "src", "text_encoder");
        assert_ne!(a.l1, c.l1, "different dimensions produce different keys");
        assert_eq!(
            l1_fingerprint("vae", &model_params_from_meta(&meta("", &[]))),
            l1_fingerprint("vae", &ModelParams {
                base_model: "unrelated".into(),
                text_encoder: String::new(),
                vae: String::new(),
                lora_keys: vec!["x".into()],
                embeddings: vec![],
                controlnet: String::new(),
            })
        );
        let _ = b;
    }

    #[test]
    fn dimensions_are_isolated() {
        let mut m = meta("m.safetensors", &[]);
        m.vae = "the_vae.safetensors".into();
        let base_only = compute_group_keys(&m, "src", "base_model");
        let without_vae = compute_group_keys(&meta("m.safetensors", &[]), "src", "base_model");
        assert_eq!(base_only.l1, without_vae.l1, "a single-selection dimension ignores other parameters");
    }

    #[test]
    fn embedding_extraction() {
        assert_eq!(
            extract_embedding_refs("causeintelligent is und mutually embedding:easynegative, embedding:bad-hands-5."),
            vec!["easynegative", "bad-hands-5"]
        );
        assert_eq!(extract_embedding_refs("no refs here"), Vec::<String>::new());
        assert_eq!(
            extract_embedding_refs("embedding:una3D"),
            vec!["una3D"]
        );
    }

    #[test]
    fn dimension_label_variants() {
        let label = ModelParamLabel {
            base_model: "m.safetensors".into(),
            text_encoder: String::new(),
            vae: String::new(),
            lora_count: 2,
            lora_names: vec!["a", "b"].iter().map(|s| s.to_string()).collect(),
            embeddings: vec![],
            controlnet: String::new(),
        };
        assert_eq!(dimension_label("base_model", &label), "m.safetensors");
        assert_eq!(dimension_label("lora", &label), "a + b (×2)");
        assert_eq!(dimension_label("text_encoder", &label), UNKNOWN_VALUE);
    }
}
