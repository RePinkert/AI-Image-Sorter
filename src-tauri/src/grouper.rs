use crate::metadata::ImageMeta;
use xxhash_rust::xxh3::xxh3_64;

pub struct GroupKeys {
    pub l0: String,
    pub l1: String,
    pub l2: String,
    pub l3: String,
}

/// Compute the 4-level group keys.
/// L0 = source folder (passed in, not computed from meta)
/// L1 = workflow (checkpoint + loras + vae)
/// L2 = prompt similarity cluster — placeholder here (== L1); assigned
///      post-scan by `clustering::recluster_l2` using real Jaccard
///      similarity within the same L1 so groups stop over-splitting.
/// L3 = individual prompt (L1 + full prompt_pos)
/// Negative prompt never participates.
pub fn compute_group_keys(meta: &ImageMeta, source_path: &str) -> GroupKeys {
    // L0: source folder
    let l0 = format!("{:016x}", xxh3_64(source_path.as_bytes()));

    // L1: workflow = checkpoint + loras + vae
    let mut l1_buf = String::new();
    l1_buf.push_str(&meta.checkpoint);
    l1_buf.push('\x01');
    let mut loras_sorted = meta.loras.clone();
    loras_sorted.sort_by(|a, b| a.name.cmp(&b.name));
    for l in &loras_sorted {
        l1_buf.push_str(&l.name);
        l1_buf.push(',');
        l1_buf.push_str(&format!("{:.2}", l.strength));
        l1_buf.push(';');
    }
    l1_buf.push('\x01');
    l1_buf.push_str(&meta.vae);
    let l1 = format!("{:016x}", xxh3_64(l1_buf.as_bytes()));

    // L2 placeholder = L1 until recluster_l2 runs. Keeps pre-cluster L2
    // view coherent (one bucket per workflow) rather than bogus prefixes.
    let l2 = l1.clone();

    // L3: individual prompt = L1 raw + full prompt_pos
    let prompt_normalized = normalize_prompt(&meta.prompt_pos);
    let mut l3_buf = l1_buf.clone();
    l3_buf.push('\x03');
    l3_buf.push_str(&prompt_normalized);
    let l3 = format!("{:016x}", xxh3_64(l3_buf.as_bytes()));

    GroupKeys { l0, l1, l2, l3 }
}

/// Normalize a prompt for tokenization (used by both grouper L3 and
/// clustering.rs L2 Jaccard so the two stay consistent).
pub fn normalize_prompt(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}
