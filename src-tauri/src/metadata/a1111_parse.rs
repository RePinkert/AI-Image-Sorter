use super::{ImageMeta, LoraInfo, SamplerInfo, SourceKind};

pub fn parse_a1111_parameters(p: &str) -> ImageMeta {
    let mut meta = ImageMeta {
        source_kind: SourceKind::A1111,
        ..Default::default()
    };
    meta.raw_ok = true;

    let mut lines = p.lines();
    let mut prompt_pos = String::new();
    let mut prompt_neg = String::new();
    let mut in_neg = false;
    let mut neg_started = false;
    let mut tail: Option<&str> = None;

    for line in lines.by_ref() {
        if line.trim_start().starts_with("Negative prompt:") {
            in_neg = true;
            neg_started = true;
            let rest = line.trim_start()[16..].trim_start();
            prompt_neg.push_str(rest);
            continue;
        }
        // A1111 puts generation params on a line starting with "Steps:" usually.
        if line.starts_with("Steps:") {
            tail = Some(line);
            break;
        }
        if in_neg {
            if !prompt_neg.is_empty() {
                prompt_neg.push('\n');
            }
            prompt_neg.push_str(line);
        } else {
            if !prompt_pos.is_empty() {
                prompt_pos.push('\n');
            }
            prompt_pos.push_str(line);
        }
    }
    let _ = neg_started;

    meta.prompt_pos = prompt_pos.trim().to_string();
    meta.prompt_neg = prompt_neg.trim().to_string();

    if let Some(t) = tail {
        let kv = parse_kv_line(t);
        let sampler = kv.get("Sampler").cloned().unwrap_or_default();
        let seed: i64 = kv.get("Seed").and_then(|s| s.parse().ok()).unwrap_or(0);
        let steps: i64 = kv.get("Steps").and_then(|s| s.parse().ok()).unwrap_or(0);
        let cfg: f64 = kv.get("CFG scale").and_then(|s| s.parse().ok()).unwrap_or(0.0);
        meta.checkpoint = kv.get("Model").cloned().unwrap_or_default();
        if let Some(vae) = kv.get("VAE") {
            meta.vae = vae.clone();
        }
        if let Some(lora_str) = kv.get("Lora hashes") {
            for part in lora_str.split(',') {
                let name = part.split(':').next().unwrap_or("").trim().to_string();
                if !name.is_empty() {
                    meta.loras.push(LoraInfo {
                        name,
                        strength: 1.0,
                    });
                }
            }
        }
        meta.samplers.push(SamplerInfo {
            sampler,
            seed,
            steps,
            cfg,
            scheduler: kv.get("Scheduler").cloned().unwrap_or_default(),
        });
    }
    meta
}

fn parse_kv_line(line: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    // Format: "Key: value, Key2: value2, ..."
    // Values may contain commas inside parentheses; naive split on ", " works for most A1111.
    for part in line.split(", ") {
        if let Some((k, v)) = part.split_once(": ") {
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    map
}
