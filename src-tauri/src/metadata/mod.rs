use serde::{Deserialize, Serialize};
use xxhash_rust::xxh3::xxh3_64;

pub const PARSER_VERSION: &str = "generation-recipe-v4-model-deviation";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ImageMeta {
    pub prompt_pos: String,
    pub prompt_neg: String,
    pub checkpoint: String,
    /// The primary diffusion model, kept separate from checkpoint for
    /// workflows that use an UNET/Diffusion Model loader directly.
    pub diffusion_model: String,
    /// Ordered model-related nodes serialized for later workflow analysis.
    pub model_chain: Vec<ModelChainItem>,
    pub loras: Vec<LoraInfo>,
    pub vae: String,
    pub samplers: Vec<SamplerInfo>,
    pub raw_ok: bool,
    pub source_kind: SourceKind,
    /// Image dimensions in pixels, read without decoding pixels.
    pub width: u32,
    pub height: u32,
    /// Workflow identity: XXH3 of the canonical topology
    /// (sorted node classes + class-pair edges), independent of the
    /// model/LoRA files actually loaded.
    pub workflow_key: String,
    /// Canonical topology `{"t": [...], "e": [...]}` used for template
    /// (sub)graph matching.
    pub workflow_graph_json: String,
    /// Normalized generation settings used by recommendations. This excludes
    /// prompt text and file paths so it is safe to persist as a recipe.
    #[serde(default)]
    pub generation_recipe: GenerationRecipe,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelChainItem {
    pub node_type: String,
    pub model: String,
    pub strength: Option<f64>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    Comfy,
    A1111,
    Local,
}

impl Default for SourceKind {
    fn default() -> Self {
        SourceKind::Local
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LoraInfo {
    pub name: String,
    pub strength: f64,
    /// ComfyUI can apply a different strength to the CLIP branch. Older
    /// metadata does not have this value, so it remains optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip_strength: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SamplerInfo {
    pub sampler: String,
    pub seed: i64,
    pub steps: i64,
    pub cfg: f64,
    pub scheduler: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct GenerationRecipe {
    pub checkpoint: String,
    pub diffusion_model: String,
    pub loras: Vec<LoraInfo>,
    pub vae: String,
    pub sampler: String,
    pub scheduler: String,
    pub steps: i64,
    pub cfg: f64,
    pub width: u32,
    pub height: u32,
    pub aspect_ratio: f64,
}

impl GenerationRecipe {
    pub fn from_meta(meta: &ImageMeta) -> Self {
        let sampler = meta.samplers.first().cloned().unwrap_or_default();
        Self {
            checkpoint: meta.checkpoint.clone(),
            diffusion_model: meta.diffusion_model.clone(),
            loras: meta.loras.clone(),
            vae: meta.vae.clone(),
            sampler: sampler.sampler,
            scheduler: sampler.scheduler,
            steps: sampler.steps,
            cfg: sampler.cfg,
            width: meta.width,
            height: meta.height,
            aspect_ratio: aspect_ratio(meta.width, meta.height),
        }
    }

    /// Produce a stable representation for grouping and persistence. LoRA
    /// order is normalized because it is not meaningful for recommendations.
    pub fn normalized(&self) -> Self {
        let mut out = self.clone();
        out.checkpoint = out.checkpoint.trim().to_string();
        out.diffusion_model = out.diffusion_model.trim().to_string();
        out.vae = out.vae.trim().to_string();
        out.sampler = out.sampler.trim().to_string();
        out.scheduler = out.scheduler.trim().to_string();
        if !out.cfg.is_finite() {
            out.cfg = 0.0;
        }
        out.loras.retain(|l| !l.name.trim().is_empty());
        for lora in &mut out.loras {
            lora.name = lora.name.trim().to_string();
            if !lora.strength.is_finite() {
                lora.strength = 0.0;
            }
            if lora.clip_strength.is_some_and(|v| !v.is_finite()) {
                lora.clip_strength = None;
            }
        }
        out.loras.sort_by(|a, b| {
            a.name
                .cmp(&b.name)
                .then_with(|| a.strength.total_cmp(&b.strength))
                .then_with(|| {
                    a.clip_strength
                        .unwrap_or(0.0)
                        .total_cmp(&b.clip_strength.unwrap_or(0.0))
                })
        });
        out.aspect_ratio = aspect_ratio(out.width, out.height);
        out
    }

    pub fn signature(&self) -> String {
        let normalized = self.normalized();
        let json = serde_json::to_string(&normalized).unwrap_or_else(|_| "{}".to_string());
        format!("{:016x}", xxh3_64(json.as_bytes()))
    }

    /// Recommendations only use configurations with enough data to be
    /// meaningfully reproduced. VAE and LoRAs may legitimately be empty.
    pub fn is_complete(&self) -> bool {
        (!self.checkpoint.is_empty() || !self.diffusion_model.is_empty())
            && !self.sampler.is_empty()
            && !self.scheduler.is_empty()
            && self.steps > 0
            && self.cfg.is_finite()
            && self.width > 0
            && self.height > 0
    }
}

fn aspect_ratio(width: u32, height: u32) -> f64 {
    if width == 0 || height == 0 {
        0.0
    } else {
        width as f64 / height as f64
    }
}

impl ImageMeta {
    pub fn refresh_generation_recipe(&mut self) {
        self.generation_recipe = GenerationRecipe::from_meta(self).normalized();
    }

    pub fn normalized_generation_recipe(&self) -> GenerationRecipe {
        GenerationRecipe::from_meta(self).normalized()
    }

    pub fn generation_recipe_json(&self) -> String {
        serde_json::to_string(&self.normalized_generation_recipe())
            .unwrap_or_else(|_| "{}".to_string())
    }

    pub fn recipe_signature(&self) -> String {
        self.normalized_generation_recipe().signature()
    }
}

pub mod a1111_parse;
pub mod comfy_parse;
pub mod jpeg;
pub mod png;

use anyhow::Result;
use std::path::Path;

pub fn parse_file(path: &Path) -> Result<ImageMeta> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    let mut meta = match ext.as_str() {
        "png" => png::parse_png(path)?,
        "jpg" | "jpeg" | "webp" => jpeg::parse_jpeg_webp(path)?,
        _ => ImageMeta::default(),
    };
    // If no prompt was found, try companion .txt file (Save Text node output).
    if meta.prompt_pos.is_empty() && meta.prompt_neg.is_empty() {
        if let Some(txt_meta) = try_companion_txt(path) {
            if !txt_meta.prompt_pos.is_empty() || !txt_meta.prompt_neg.is_empty() {
                if meta.prompt_pos.is_empty() {
                    meta.prompt_pos = txt_meta.prompt_pos.clone();
                }
                if meta.prompt_neg.is_empty() {
                    meta.prompt_neg = txt_meta.prompt_neg.clone();
                }
                if meta.checkpoint.is_empty() {
                    meta.checkpoint = txt_meta.checkpoint.clone();
                }
                if meta.diffusion_model.is_empty() {
                    meta.diffusion_model = txt_meta.diffusion_model.clone();
                }
                if meta.loras.is_empty() {
                    meta.loras = txt_meta.loras.clone();
                }
                if meta.vae.is_empty() {
                    meta.vae = txt_meta.vae.clone();
                }
                if meta.samplers.is_empty() {
                    meta.samplers = txt_meta.samplers.clone();
                }
                if meta.width == 0 {
                    meta.width = txt_meta.width;
                }
                if meta.height == 0 {
                    meta.height = txt_meta.height;
                }
                if !txt_meta.raw_ok {
                    meta.raw_ok = true;
                }
                if meta.source_kind == SourceKind::Local {
                    meta.source_kind = SourceKind::Comfy;
                }
            }
        }
    }
    meta.refresh_generation_recipe();
    Ok(meta)
}

/// Try to read a companion .txt file (same stem as the image) produced by ComfyUI "Save Text" node.
fn try_companion_txt(image_path: &Path) -> Option<ImageMeta> {
    let stem = image_path.file_stem()?;
    let parent = image_path.parent()?;
    let txt_path = parent.join(format!("{}.txt", stem.to_string_lossy()));
    if !txt_path.exists() {
        return None;
    }
    let content = std::fs::read_to_string(&txt_path).ok()?;
    // The txt may contain: prompt text, or "Positive: ...\nNegative: ..." or A1111-style.
    if content.contains("Steps:") {
        return Some(a1111_parse::parse_a1111_parameters(&content));
    }
    let mut meta = ImageMeta {
        source_kind: SourceKind::Comfy,
        ..Default::default()
    };
    meta.raw_ok = true;
    // Try to split positive/negative by markers.
    if let Some(idx) = content.find("Negative prompt:") {
        meta.prompt_pos = content[..idx].trim().to_string();
        meta.prompt_neg = content[idx + 16..].trim().to_string();
    } else if let Some(idx) = content.find("Negative:") {
        meta.prompt_pos = content[..idx].trim().to_string();
        meta.prompt_neg = content[idx + 9..].trim().to_string();
    } else {
        meta.prompt_pos = content.trim().to_string();
    }
    Some(meta)
}
