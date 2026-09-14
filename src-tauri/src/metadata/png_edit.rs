use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

#[derive(Clone)]
struct Chunk {
    kind: [u8; 4],
    data: Vec<u8>,
}

pub fn write_prompts(path: &Path, positive: &str, negative: &str) -> Result<PathBuf> {
    let bytes = fs::read(path).with_context(|| format!("无法读取 {}", path.display()))?;
    let mut chunks = parse_chunks(&bytes)?;
    let prompt_idx = text_chunk_index(&chunks, b"prompt");
    let parameters_idx = text_chunk_index(&chunks, b"parameters");

    if let Some(index) = prompt_idx {
        let workflow_idx = text_chunk_index(&chunks, b"workflow")
            .ok_or_else(|| anyhow!("暂不支持：ComfyUI PNG 缺少可同步的 workflow 元数据"))?;
        let prompt = text_value(&chunks[index])?;
        let workflow = text_value(&chunks[workflow_idx])?;
        let (new_prompt, targets) = edit_comfy_prompt(prompt, positive, negative)?;
        let new_workflow = edit_comfy_workflow(workflow, &targets)?;
        chunks[index].data = text_data(text_keyword(&chunks[index])?, &new_prompt);
        chunks[workflow_idx].data = text_data(text_keyword(&chunks[workflow_idx])?, &new_workflow);
    } else if let Some(index) = parameters_idx {
        let parameters = text_value(&chunks[index])?;
        let updated = edit_a1111(parameters, positive, negative)?;
        chunks[index].data = text_data(text_keyword(&chunks[index])?, &updated);
    } else {
        bail!("暂不支持：PNG 中没有可编辑的 parameters 或 ComfyUI prompt 元数据");
    }

    let parent = path.parent().ok_or_else(|| anyhow!("图片路径没有父目录"))?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let temp = parent.join(format!(".ai-image-sorter-{nonce}.png"));
    let mut file = fs::File::create(&temp)?;
    file.write_all(SIGNATURE)?;
    for chunk in chunks {
        file.write_all(&(chunk.data.len() as u32).to_be_bytes())?;
        file.write_all(&chunk.kind)?;
        file.write_all(&chunk.data)?;
        let mut crc = crc32fast::Hasher::new();
        crc.update(&chunk.kind);
        crc.update(&chunk.data);
        file.write_all(&crc.finalize().to_be_bytes())?;
    }
    file.sync_all()?;
    Ok(temp)
}

fn parse_chunks(bytes: &[u8]) -> Result<Vec<Chunk>> {
    if bytes.get(..8) != Some(SIGNATURE) {
        bail!("文件不是有效 PNG");
    }
    let mut offset = 8usize;
    let mut out = Vec::new();
    while offset + 12 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        let end = offset
            .checked_add(12 + len)
            .ok_or_else(|| anyhow!("PNG chunk 长度溢出"))?;
        if end > bytes.len() {
            bail!("PNG chunk 不完整");
        }
        let kind: [u8; 4] = bytes[offset + 4..offset + 8].try_into().unwrap();
        let data = bytes[offset + 8..offset + 8 + len].to_vec();
        out.push(Chunk { kind, data });
        offset = end;
        if &kind == b"IEND" {
            break;
        }
    }
    if out.last().map(|c| &c.kind) != Some(b"IEND") {
        bail!("PNG 缺少 IEND");
    }
    Ok(out)
}

fn text_chunk_index(chunks: &[Chunk], keyword: &[u8]) -> Option<usize> {
    chunks.iter().enumerate().rev().find_map(|(i, c)| {
        (c.kind == *b"tEXt" && c.data.split(|b| *b == 0).next() == Some(keyword)).then_some(i)
    })
}

fn text_keyword(chunk: &Chunk) -> Result<&str> {
    let end = chunk
        .data
        .iter()
        .position(|b| *b == 0)
        .ok_or_else(|| anyhow!("无效 tEXt chunk"))?;
    std::str::from_utf8(&chunk.data[..end]).context("PNG metadata keyword 不是 UTF-8")
}

fn text_value(chunk: &Chunk) -> Result<&str> {
    let start = chunk
        .data
        .iter()
        .position(|b| *b == 0)
        .ok_or_else(|| anyhow!("无效 tEXt chunk"))?
        + 1;
    std::str::from_utf8(&chunk.data[start..]).context("PNG metadata 文本不是 UTF-8")
}

fn text_data(keyword: &str, value: &str) -> Vec<u8> {
    let mut data = Vec::with_capacity(keyword.len() + value.len() + 1);
    data.extend_from_slice(keyword.as_bytes());
    data.push(0);
    data.extend_from_slice(value.as_bytes());
    data
}

fn edit_a1111(raw: &str, positive: &str, negative: &str) -> Result<String> {
    let steps = raw
        .find("\nSteps:")
        .or_else(|| raw.starts_with("Steps:").then_some(0))
        .ok_or_else(|| anyhow!("暂不支持：A1111 parameters 缺少 Steps 段"))?;
    let tail = if steps == 0 { raw } else { &raw[steps + 1..] };
    let mut out = positive.trim().to_string();
    if !negative.trim().is_empty() {
        out.push_str("\nNegative prompt: ");
        out.push_str(negative.trim());
    }
    out.push('\n');
    out.push_str(tail);
    Ok(out)
}

struct ComfyTargets {
    positive: String,
    old_positive: String,
    new_positive: String,
    negative: Option<(String, String, String)>,
}

fn edit_comfy_prompt(raw: &str, positive: &str, negative: &str) -> Result<(String, ComfyTargets)> {
    let sanitized = super::comfy_parse::sanitize_json(raw);
    let mut root: Value = serde_json::from_str(&sanitized).context("ComfyUI prompt JSON 无效")?;
    let nodes = root
        .as_object()
        .ok_or_else(|| anyhow!("ComfyUI prompt 不是对象"))?;
    let mut pos = HashSet::new();
    let mut neg = HashSet::new();
    for node in nodes.values() {
        let class = node.get("class_type").and_then(Value::as_str).unwrap_or("");
        if !matches!(class, "KSampler" | "KSamplerAdvanced") {
            continue;
        }
        let inputs = node
            .get("inputs")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("采样器缺少 inputs"))?;
        pos.insert(direct_clip_id(nodes, inputs.get("positive"), false)?.unwrap());
        if let Some(id) = direct_clip_id(nodes, inputs.get("negative"), negative.trim().is_empty())?
        {
            neg.insert(id);
        }
    }
    if pos.len() != 1 || neg.len() > 1 {
        bail!("暂不支持：Prompt 必须唯一连接到一个可编辑文本节点");
    }
    let positive_id = pos.into_iter().next().unwrap();
    let old_positive = set_api_text(&mut root, &positive_id, positive)?;
    let negative_target = if let Some(negative_id) = neg.into_iter().next() {
        if positive_id == negative_id {
            bail!("暂不支持：正负 Prompt 共用同一节点");
        }
        let old_negative = set_api_text(&mut root, &negative_id, negative)?;
        Some((negative_id, old_negative, negative.to_string()))
    } else {
        None
    };
    Ok((
        serde_json::to_string(&root)?,
        ComfyTargets {
            positive: positive_id,
            old_positive,
            new_positive: positive.to_string(),
            negative: negative_target,
        },
    ))
}

fn direct_clip_id(
    nodes: &serde_json::Map<String, Value>,
    reference: Option<&Value>,
    allow_zeroed: bool,
) -> Result<Option<String>> {
    let id = reference
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("暂不支持：Prompt 不是直接节点连接"))?;
    let node = nodes.get(id).ok_or_else(|| anyhow!("Prompt 节点不存在"))?;
    if node.get("class_type").and_then(Value::as_str) == Some("ConditioningZeroOut") {
        if allow_zeroed {
            return Ok(None);
        }
        bail!("暂不支持：该工作流禁用了负 Prompt，无法写入非空内容");
    }
    if node.get("class_type").and_then(Value::as_str) != Some("CLIPTextEncode") {
        bail!("暂不支持：Prompt 经过了复杂节点");
    }
    if !node.pointer("/inputs/text").is_some_and(Value::is_string) {
        bail!("暂不支持：Prompt 文本不是直接字符串");
    }
    Ok(Some(id.to_string()))
}

fn set_api_text(root: &mut Value, id: &str, value: &str) -> Result<String> {
    let text = root
        .pointer(&format!("/{id}/inputs/text"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Prompt 文本节点不可编辑"))?
        .to_string();
    *root.pointer_mut(&format!("/{id}/inputs/text")).unwrap() = Value::String(value.to_string());
    Ok(text)
}

fn edit_comfy_workflow(raw: &str, targets: &ComfyTargets) -> Result<String> {
    let sanitized = super::comfy_parse::sanitize_json(raw);
    let mut root: Value = serde_json::from_str(&sanitized).context("ComfyUI workflow JSON 无效")?;
    let nodes = root
        .get_mut("nodes")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| anyhow!("workflow 缺少 nodes"))?;
    update_ui_node(
        nodes,
        &targets.positive,
        &targets.old_positive,
        &targets.new_positive,
    )?;
    if let Some((id, old, new)) = &targets.negative {
        update_ui_node(nodes, id, old, new)?;
    }
    Ok(serde_json::to_string(&root)?)
}

fn update_ui_node(nodes: &mut [Value], id: &str, old: &str, new: &str) -> Result<()> {
    let numeric = id.parse::<i64>().ok();
    let node = nodes
        .iter_mut()
        .find(|n| {
            n.get("id").and_then(Value::as_i64) == numeric
                || n.get("id").and_then(Value::as_str) == Some(id)
        })
        .ok_or_else(|| anyhow!("暂不支持：workflow 中找不到 Prompt 节点 {id}"))?;
    let widgets = node
        .get_mut("widgets_values")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| anyhow!("暂不支持：workflow Prompt 节点缺少 widgets_values"))?;
    let matches: Vec<usize> = widgets
        .iter()
        .enumerate()
        .filter_map(|(i, v)| (v.as_str() == Some(old)).then_some(i))
        .collect();
    if matches.len() != 1 {
        bail!("暂不支持：workflow Prompt 文本无法唯一定位");
    }
    widgets[matches[0]] = Value::String(new.to_string());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn chunk(kind: &[u8; 4], data: &[u8], out: &mut Vec<u8>) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let mut crc = crc32fast::Hasher::new();
        crc.update(kind);
        crc.update(data);
        out.extend_from_slice(&crc.finalize().to_be_bytes());
    }

    #[test]
    fn preserves_a1111_settings() {
        let raw = "cat\nNegative prompt: blur\nSteps: 20, Sampler: Euler, Custom: x";
        assert_eq!(
            edit_a1111(raw, "dog", "noise").unwrap(),
            "dog\nNegative prompt: noise\nSteps: 20, Sampler: Euler, Custom: x"
        );
    }

    #[test]
    fn rewrites_metadata_without_touching_image_chunks() {
        let mut bytes = SIGNATURE.to_vec();
        chunk(
            b"IHDR",
            &[0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0],
            &mut bytes,
        );
        chunk(
            b"tEXt",
            b"parameters\0cat\nNegative prompt: blur\nSteps: 20, Seed: 1",
            &mut bytes,
        );
        chunk(b"IDAT", b"unchanged-image-data", &mut bytes);
        chunk(b"IEND", &[], &mut bytes);
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "ai-image-sorter-png-edit-{}.png",
            std::process::id()
        ));
        fs::write(&path, &bytes).unwrap();
        let temp = write_prompts(&path, "dog", "noise").unwrap();
        let output = fs::read(&temp).unwrap();
        let chunks = parse_chunks(&output).unwrap();
        assert_eq!(
            text_value(&chunks[text_chunk_index(&chunks, b"parameters").unwrap()]).unwrap(),
            "dog\nNegative prompt: noise\nSteps: 20, Seed: 1"
        );
        assert_eq!(
            chunks.iter().find(|c| c.kind == *b"IDAT").unwrap().data,
            b"unchanged-image-data"
        );
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(temp);
    }

    #[test]
    fn accepts_comfy_non_standard_numbers() {
        let prompt = r#"{
            "1":{"class_type":"CLIPTextEncode","inputs":{"text":"cat"}},
            "2":{"class_type":"CLIPTextEncode","inputs":{"text":"blur"}},
            "3":{"class_type":"KSampler","inputs":{"positive":["1",0],"negative":["2",0],"is_changed":NaN}}
        }"#;
        let workflow = r#"{"nodes":[
            {"id":1,"widgets_values":["cat"]},
            {"id":2,"widgets_values":["blur"]}
        ],"extra":Infinity}"#;
        let (updated_prompt, targets) = edit_comfy_prompt(prompt, "dog", "noise").unwrap();
        let updated_workflow = edit_comfy_workflow(workflow, &targets).unwrap();
        assert!(updated_prompt.contains("\"text\":\"dog\""));
        assert!(updated_prompt.contains("\"is_changed\":null"));
        assert!(updated_workflow.contains("\"noise\""));
        assert!(updated_workflow.contains("\"extra\":null"));
    }

    #[test]
    fn allows_empty_zeroed_negative_prompt() {
        let prompt = r#"{
            "1":{"class_type":"CLIPTextEncode","inputs":{"text":"cat"}},
            "2":{"class_type":"ConditioningZeroOut","inputs":{"conditioning":["1",0]}},
            "3":{"class_type":"KSamplerAdvanced","inputs":{"positive":["1",0],"negative":["2",0]}}
        }"#;
        let workflow =
            r#"{"nodes":[{"id":1,"widgets_values":["cat"]},{"id":2,"widgets_values":[]}]}"#;
        let (updated_prompt, targets) = edit_comfy_prompt(prompt, "dog", "").unwrap();
        let updated_workflow = edit_comfy_workflow(workflow, &targets).unwrap();
        assert!(updated_prompt.contains("\"text\":\"dog\""));
        assert!(updated_workflow.contains("\"dog\""));
        assert!(edit_comfy_prompt(prompt, "dog", "noise").is_err());
    }
}
