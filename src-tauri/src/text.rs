//! 文本塔（阶段七 Round 2）
//!
//! 目标：把每条 prompt 变成一个可比较的向量（512 维 BGE CLS + L2 归一化），
//! 与视觉塔对称地落在 `image_signatures` 表的 `text` 槽位里。
//! 推理完全在本进程内（ONNX Runtime），不依赖 Python、不依赖 ComfyUI、不联网。
//!
//! ## 为什么分词器要手写
//!
//! HF `tokenizers` 有 Rust 版，但拖进来就是 tokenizers + onnx + protobuf
//! 一整条链（几百个 crate），与本项目「不引入新的运行/构建依赖」的硬约束 #4
//! 直接冲突。这里的分词器只依赖 `unicode_categories`（纯查表，零依赖），
//! 语义**照抄** HF `tokenizers` 0.23.3 的 BertNormalizer / BertPreTokenizer /
//! WordPiece —— 已逐字比对源码（`normalizers/bert.rs`、`pre_tokenizers/bert.rs`、
//! `models/wordpiece/mod.rs`），不是凭记忆。分词闸门保证「逐条相等」。
//!
//! ## 口径闸门（ui-program.md 阶段七「文本塔」）
//!
//! | 闸门 | 比什么 | 阈值 |
//! |---|---|---|
//! | 分词闸门 | Rust token id vs Python `tokenizers` 0.23.3 | **逐条相等** |
//! | 向量闸门 | Rust 向量 vs `tools/l2lab/out/text.json` | 余弦 > 0.97 |
//!
//! ## 已知的数值敏感性（不是 bug，是 int8 量化的固有性质）
//!
//! 实测（实验室 ORT 1.22.0，int8 量化权重）：
//!
//! | 变量 | 同一输入的余弦 |
//! |---|---|
//! | 批大小 1 ↔ 8（padding 宽度固定） | 0.9859 ~ 0.9957 |
//! | padding 宽度 7 ↔ 512（批大小固定为 1） | 0.9953 ~ 0.9959 |
//!
//! 也就是说**同一条 prompt 的向量会随同批其它条目浮动 ~0.5%**。因此：
//!
//! * 批大小恒为 8（`TEXT_BATCH`），与视觉塔同口径；
//!   * padding 宽度取**批内最长**（与实验室 `embed.py` 同口径 —— 改这条就要
//!   同时改实验室并重生成参考向量，两边必须一起动）；
//! * 向量闸门**按参考文件的 id 顺序复刻批次构成**，否则余弦会被上述噪声
//!   压到 0.986~0.996，0.97 阈值就失去判别力。
//!
//! 本模块是阶段七**轮 2** 的交付物（塔 + 两道闸门），**轮 3 已接线**：
//! `TextEngineSlot` + `compute_text_signatures` 命令把它接进命令层，
//! 所以这个 `dead_code` 豁免已经删掉 —— 留着就等于允许再出现一座没人
//! 调用的塔（轮 2 的整座文本塔就是那样被 LTO 剔出 exe 的）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ndarray::Array2;
use ort::inputs;
use ort::session::Session;
use ort::value::Value;
use tauri::Manager;
use unicode_categories::UnicodeCategories;

/// 文本塔的模型标识。向量按 (slot, model_id) 分区存储 —— 换模型必须重算，
/// 不同模型的向量不在同一空间里，绝不能混着比。
pub const TEXT_MODEL_ID: &str = "bge-small-zh-q";
/// 能力标记：文本塔（BGE）已接进二进制。
///
/// 标记串住在**实现它的这个模块**里 —— 见 `capabilities.rs` 的纪律说明。
/// 末尾的 `\0` 是 C 风格哨兵（探针靠它把标记串与相邻字面量分开）。
///
/// ⚠ 在本塔**接线之前**（阶段七轮 2）整个模块是死代码，链接器把它连同这
/// 条标记串一起剔除了 —— 探针因此抓不到它。接线之后标记串才出现。
pub const CAP_TOWER_TEXT: &str = "ais.cap.tower.text.v1\0";
/// 权重文件名（与 `scripts/download_models.ps1` 保持一致）。
pub const TEXT_WEIGHTS: &str = "text_bge.onnx";
/// 分词器文件名（同上）。
pub const TEXT_TOKENIZER: &str = "tokenizer_bge.json";
/// 截断长度（**含** `[CLS]`/`[SEP]`，与实验室 `embed.py` 的 `MAX_TEXT_TOKENS` 同值）。
pub const MAX_TEXT_TOKENS: usize = 512;
/// 批大小恒为 8 —— int8 量化模型对批大小敏感（见模块头的敏感性实测）。
pub const TEXT_BATCH: usize = 8;

// ───────────────────────────── 归一化 ─────────────────────────────
// 口径：HF `tokenizers` 0.23.3 `normalizers/bert.rs`（逐字照抄）

/// HF 的 `is_control`：Unicode 通用类别 C（Other），但 `\t` `\n` `\r` 除外。
///
/// HF 源码：`match c { '\t' | '\n' | '\r' => false, _ => c.is_other() }`
fn is_control(c: char) -> bool {
    !matches!(c, '\t' | '\n' | '\r') && c.is_other()
}

/// HF 的 `clean_text`：删控制字符与替换字符，空白一律归一为 `' '`。
///
/// HF 源码：`filter(|c| !(c == 0 || c == 0xfffd || is_control(c)))
///            .map(|c| if is_whitespace(c) { ' ' } else { c })`
fn clean_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        if c == '\0' || c == '\u{FFFD}' || is_control(c) {
            continue;
        }
        if c.is_whitespace() {
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

/// HF 的 `is_chinese_char`：CJK 统一表意文字各段（含扩展 A 与兼容表意文字）。
fn is_chinese_char(c: char) -> bool {
    matches!(
        c as usize,
        0x4E00..=0x9FFF
            | 0x3400..=0x4DBF
            | 0x20000..=0x2A6DF
            | 0x2A700..=0x2B73F
            | 0x2B740..=0x2B81F
            | 0x2B920..=0x2CEAF
            | 0xF900..=0xFAFF
            | 0x2F800..=0x2FA1F
    )
}

/// HF 的 `handle_chinese_chars`：在每个 CJK 字符前后各插一个空格，
/// 好让预分词把汉字**逐字**切开。
fn handle_chinese_chars(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        if is_chinese_char(c) {
            out.push(' ');
            out.push(c);
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

// ───────────────────────────── 预分词 ─────────────────────────────
// 口径：HF `tokenizers` 0.23.3 `pre_tokenizers/bert.rs`（逐字照抄）

/// HF 的 `is_bert_punc`：ASCII 标点 ∪ Unicode 通用类别 P\*。
fn is_bert_punc(c: char) -> bool {
    c.is_ascii_punctuation() || c.is_punctuation()
}

/// HF 的 `BertPreTokenizer`：先按空白切（空白**移除**），再把标点**隔离**
/// 成各自独立的片段。
///
/// HF 源码：`split(|_, s| s.split(char::is_whitespace, Removed))`
/// 然后 `split(|_, s| s.split(is_bert_punc, Isolated))`
fn pre_tokenize(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // `split_whitespace` 与 HF 的 `char::is_whitespace` 切分等价：
    // 连续空白不产生空片段，首尾空白被丢掉。
    for word in text.split_whitespace() {
        let mut cur = String::new();
        for ch in word.chars() {
            if is_bert_punc(ch) {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                out.push(ch.to_string());
            } else {
                cur.push(ch);
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
    }
    out
}

// ─────────────────────────── WordPiece 模型 ───────────────────────────
// 口径：HF `tokenizers` 0.23.3 `models/wordpiece/mod.rs`（逐字照抄）

/// WordPiece 贪心最长匹配。
///
/// HF 源码（`WordPiece::tokenize`）的三条规则：
/// 1. 整词字符数 > `max_input_chars_per_word` → **整个词**变 `[UNK]`；
/// 2. 逐个起点贪心取最长的可匹配子串（非词首加 `##` 前缀）；
/// 3. 任一位置匹配不上 → **整个词**变 `[UNK]`（不是逐字符退化成 `[UNK]`）。
fn wordpiece_ids(
    word: &str,
    vocab: &HashMap<String, i64>,
    prefix: &str,
    max_input_chars_per_word: usize,
    unk_id: i64,
) -> Vec<i64> {
    if word.chars().count() > max_input_chars_per_word {
        return vec![unk_id];
    }
    let mut ids: Vec<i64> = Vec::new();
    let mut start = 0usize;
    let mut buf = String::new();
    while start < word.len() {
        let mut end = word.len();
        let mut found: Option<i64> = None;
        while start < end {
            buf.clear();
            if start > 0 {
                buf.push_str(prefix);
            }
            buf.push_str(&word[start..end]);
            if let Some(&id) = vocab.get(buf.as_str()) {
                found = Some(id);
                break;
            }
            // 去掉最后一个字符再试。
            // HF 源码：`end -= substr.chars().last().map_or(1, |c| c.len_utf8())`
            end -= word[..end].chars().last().map_or(1, |c| c.len_utf8());
        }
        match found {
            Some(id) => {
                ids.push(id);
                start = end;
            }
            // 整词退化成一个 [UNK]
            None => return vec![unk_id],
        }
    }
    ids
}

// ─────────────────────────── 分词器 ───────────────────────────

/// `tokenizer.json` 的 `model` 段。
#[derive(serde::Deserialize)]
struct ModelFile {
    #[serde(rename = "type")]
    kind: String,
    unk_token: String,
    continuing_subword_prefix: String,
    max_input_chars_per_word: usize,
    vocab: HashMap<String, i64>,
}

/// `tokenizer.json` 里我们关心的几段（其余忽略）。
#[derive(serde::Deserialize)]
struct TokenizerFile {
    normalizer: Option<serde_json::Value>,
    pre_tokenizer: Option<serde_json::Value>,
    post_processor: Option<serde_json::Value>,
    model: ModelFile,
}

/// WordPiece 分词器（照抄 HF `tokenizers` 0.23.3 的语义）。
pub struct WordPieceTokenizer {
    vocab: HashMap<String, i64>,
    unk_id: i64,
    cls_id: i64,
    sep_id: i64,
    prefix: String,
    max_input_chars_per_word: usize,
    max_len: usize,
}

impl WordPieceTokenizer {
    /// 从 `tokenizer.json` 载入。
    ///
    /// **只支持我们照抄的那几种配置**：配置与实现对不上就报错，
    /// 绝不静默降级 —— 静默降级会让分词闸门测的是一个不存在的东西。
    pub fn from_file(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path).map_err(|e| format!("读分词器失败：{e}"))?;
        let file: TokenizerFile =
            serde_json::from_str(&raw).map_err(|e| format!("解析分词器失败：{e}"))?;

        if file.model.kind != "WordPiece" {
            return Err(format!(
                "分词器模型类型是 {}，只实现了 WordPiece",
                file.model.kind
            ));
        }

        // 归一化：只认 clean_text + handle_chinese_chars，不做 strip_accents / lowercase。
        let norm = file
            .normalizer
            .ok_or_else(|| "分词器缺 normalizer 段".to_string())?;
        expect_flag(&norm, "type", "BertNormalizer")?;
        expect_bool(&norm, "clean_text", true)?;
        expect_bool(&norm, "handle_chinese_chars", true)?;
        expect_bool(&norm, "lowercase", false)?;
        // `strip_accents` 为 null 时 HF 取 `lowercase`（false）→ 不做去重音。
        match norm.get("strip_accents") {
            None | Some(serde_json::Value::Null) => {}
            Some(v) if v == &serde_json::json!(false) => {}
            Some(v) => {
                return Err(format!(
                    "分词器开了 strip_accents（{v}），本实现不支持（那会改字形）"
                ))
            }
        }

        // 预分词：只认 BertPreTokenizer。
        let pre = file
            .pre_tokenizer
            .ok_or_else(|| "分词器缺 pre_tokenizer 段".to_string())?;
        expect_flag(&pre, "type", "BertPreTokenizer")?;

        // 后处理：TemplateProcessing 的 single 必须是 [CLS] A [SEP]。
        let post = file
            .post_processor
            .ok_or_else(|| "分词器缺 post_processor 段".to_string())?;
        expect_flag(&post, "type", "TemplateProcessing")?;
        let specials = post
            .get("special_tokens")
            .and_then(|v| v.as_object())
            .ok_or_else(|| "分词器缺 post_processor.special_tokens".to_string())?;
        let cls_id = special_id(specials, "[CLS]")?;
        let sep_id = special_id(specials, "[SEP]")?;
        let single = post
            .get("single")
            .and_then(|v| v.as_array())
            .ok_or_else(|| "分词器缺 post_processor.single".to_string())?;
        let template_ok = single.len() == 3
            && single[0].get("SpecialToken").and_then(|v| v.get("id")) == Some(&serde_json::json!("[CLS]"))
            && single[1].get("Sequence").is_some()
            && single[2].get("SpecialToken").and_then(|v| v.get("id")) == Some(&serde_json::json!("[SEP]"));
        if !template_ok {
            return Err("post_processor.single 不是 [CLS] A [SEP] 的模板".to_string());
        }

        let unk_id = *file
            .model
            .vocab
            .get(&file.model.unk_token)
            .ok_or_else(|| format!("词表里没有 {} 词条", file.model.unk_token))?;

        Ok(Self {
            vocab: file.model.vocab,
            unk_id,
            cls_id,
            sep_id,
            prefix: file.model.continuing_subword_prefix,
            max_input_chars_per_word: file.model.max_input_chars_per_word,
            max_len: MAX_TEXT_TOKENS,
        })
    }

    /// 词表大小。
    pub fn vocab_size(&self) -> usize {
        self.vocab.len()
    }

    /// 编码一条文本 → (`input_ids`, `token_type_ids`)。
    ///
    /// 截断口径（实测 HF 行为，不是推测）：`max_length` **计入** `[CLS]` 与
    /// `[SEP]`，所以内容最多留 `max_len - 2` 个 token，右截断。
    /// 例：600 个汉字 → 总长 512，`[SEP]` 在末位。
    pub fn encode(&self, text: &str) -> (Vec<i64>, Vec<i64>) {
        let normalized = handle_chinese_chars(&clean_text(text));
        let mut content: Vec<i64> = Vec::new();
        for word in pre_tokenize(&normalized) {
            content.extend(wordpiece_ids(
                &word,
                &self.vocab,
                &self.prefix,
                self.max_input_chars_per_word,
                self.unk_id,
            ));
        }
        let cap = self.max_len.saturating_sub(2);
        if content.len() > cap {
            content.truncate(cap);
        }
        let mut ids = Vec::with_capacity(content.len() + 2);
        ids.push(self.cls_id);
        ids.extend_from_slice(&content);
        ids.push(self.sep_id);
        let type_ids = vec![0i64; ids.len()];
        (ids, type_ids)
    }
}

/// 断言配置里某个字段等于期望值。
fn expect_flag(v: &serde_json::Value, key: &str, want: &str) -> Result<(), String> {
    let got = v.get(key).and_then(|x| x.as_str()).unwrap_or("");
    if got != want {
        return Err(format!("分词器 {key} = {got:?}，本实现只支持 {want:?}"));
    }
    Ok(())
}

/// 断言配置里某个布尔字段等于期望值。
fn expect_bool(v: &serde_json::Value, key: &str, want: bool) -> Result<(), String> {
    let got = v.get(key).and_then(|x| x.as_bool());
    if got != Some(want) {
        return Err(format!(
            "分词器 {key} = {got:?}，本实现只支持 {want}"
        ));
    }
    Ok(())
}

/// 取 `post_processor.special_tokens` 里某个特殊 token 的 id。
fn special_id(specials: &serde_json::Map<String, serde_json::Value>, name: &str) -> Result<i64, String> {
    let entry = specials
        .get(name)
        .ok_or_else(|| format!("post_processor.special_tokens 缺 {name}"))?;
    entry
        .get("ids")
        .and_then(|v| v.as_array())
        .and_then(|v| v.first())
        .and_then(|v| v.as_i64())
        .ok_or_else(|| format!("{name} 没有可用的 id"))
}

// ───────────────────────────── 推理引擎 ─────────────────────────────

/// L2 归一化（与视觉塔同一套口径：余弦相似度 = 归一化后的点积）。
///
/// 与 `vision.rs` 里的同名函数重复了一份：两个塔互相独立，
/// 共享一个 6 行的纯函数不值得引入耦合。
fn l2_normalize(vec: &mut [f32]) {
    let norm: f32 = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for v in vec.iter_mut() {
            *v /= norm;
        }
    }
}

/// 文本推理引擎。ONNX session 的创建要 1 秒左右，所以懒加载并缓存。
pub struct TextEngine {
    session: Mutex<Option<Session>>,
    model_path: PathBuf,
    tokenizer: WordPieceTokenizer,
}

impl TextEngine {
    /// 构造引擎。`tokenizer` 单独传入 —— 权重与分词器是两份文件，
    /// 缺任何一份这个塔都不可用。
    pub fn new(model_path: PathBuf, tokenizer: WordPieceTokenizer) -> Self {
        Self {
            session: Mutex::new(None),
            model_path,
            tokenizer,
        }
    }

    pub fn tokenizer(&self) -> &WordPieceTokenizer {
        &self.tokenizer
    }

    /// 对一批 prompt 算文本向量。`progress(done, total)` 每批回调一次；
    /// `is_cancelled()` 返回 true 时提前中止。
    ///
    /// 口径：批大小恒为 8；padding 宽度取**批内最长**（与实验室 `embed.py`
    /// 一致 —— 两边必须同口径，否则向量闸门失去判别力）。
    pub fn embed_prompts(
        &self,
        items: &[(i64, String)],
        progress: &mut dyn FnMut(usize, usize),
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<(Vec<(i64, Vec<f32>)>, usize), String> {
        if items.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let mut session = self.session.lock().map_err(|_| "文本引擎锁已中毒")?;
        if session.is_none() {
            *session = Some(
                Session::builder()
                    .map_err(|e| e.to_string())?
                    .with_intra_threads(4)
                    .map_err(|e| e.to_string())?
                    .commit_from_file(&self.model_path)
                    .map_err(|e| format!("加载文本模型失败：{e}"))?,
            );
        }
        let session = session.as_mut().unwrap();

        // 输入名对不上就报错 —— 静默喂错张量的后果是「向量看着正常但全错」。
        let names: Vec<&str> = session.inputs.iter().map(|i| i.name.as_str()).collect();
        for required in ["input_ids", "attention_mask", "token_type_ids"] {
            if !names.contains(&required) {
                return Err(format!(
                    "文本模型的输入与口径不符：缺 {required}（实际有 {names:?}）"
                ));
            }
        }

        let mut out: Vec<(i64, Vec<f32>)> = Vec::with_capacity(items.len());
        let total = items.len();
        for chunk in items.chunks(TEXT_BATCH) {
            if is_cancelled() {
                break;
            }
            let encoded: Vec<(Vec<i64>, Vec<i64>)> = chunk
                .iter()
                .map(|(_, text)| self.tokenizer.encode(text))
                .collect();
            let width = encoded
                .iter()
                .map(|(ids, _)| ids.len())
                .max()
                .unwrap_or(1)
                .max(1);
            let rows = chunk.len();
            let mut input_ids = vec![0i64; rows * width];
            let mut attention_mask = vec![0i64; rows * width];
            let mut token_type_ids = vec![0i64; rows * width];
            for (i, (ids, tids)) in encoded.iter().enumerate() {
                let n = ids.len();
                input_ids[i * width..i * width + n].copy_from_slice(ids);
                attention_mask[i * width..i * width + n].iter_mut().for_each(|v| *v = 1);
                token_type_ids[i * width..i * width + n].copy_from_slice(tids);
            }
            let shape = (rows, width);
            let ids_value = Value::from_array(
                Array2::from_shape_vec(shape, input_ids).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            let mask_value = Value::from_array(
                Array2::from_shape_vec(shape, attention_mask).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            let type_value = Value::from_array(
                Array2::from_shape_vec(shape, token_type_ids).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;

            let outputs = session
                .run(inputs![
                    "input_ids" => ids_value,
                    "attention_mask" => mask_value,
                    "token_type_ids" => type_value,
                ])
                .map_err(|e| format!("文本推理失败：{e}"))?;
            let (out_shape, data) = outputs[0]
                .try_extract_tensor::<f32>()
                .map_err(|e| format!("读取文本输出失败：{e}"))?;
            // 输出是 [B, L, H]：B=批, L=序列长（含 padding）, H=隐藏维。
            if out_shape.len() != 3 {
                return Err(format!("文本输出维度异常：{out_shape:?}"));
            }
            let (b, l, h) = (
                out_shape[0] as usize,
                out_shape[1] as usize,
                out_shape[2] as usize,
            );
            for i in 0..b.min(rows) {
                // 取 CLS（第 0 个 token）—— BGE 的句向量口径。
                let offset = i * l * h;
                let mut vec: Vec<f32> = data[offset..offset + h].to_vec();
                l2_normalize(&mut vec);
                out.push((chunk[i].0, vec));
            }
            progress(out.len(), total);
        }
        Ok((out, 0))
    }
}

/// 文本塔的**引擎槽**。
///
/// 与视觉塔的差别：文本塔要**两份**文件（权重 + 分词器），缺任何一份这个塔
/// 都不可用。所以不能在启动时直接构造 `TextEngine`（它要一个已加载的分词器）
/// —— 那样缺文件就会让应用起不来。
///
/// 槽的行为：
/// * `available()` 只看文件在不在，**不做任何加载**（启动要快）；
/// * 真正要用时才懒加载分词器与 ONNX session（session 要 1 秒左右）；
/// * 缺文件或加载失败 → 返回 Err，调用方把功能标成不可用，而不是崩。
pub struct TextEngineSlot {
    model_path: PathBuf,
    tokenizer_path: PathBuf,
    inner: Mutex<Option<TextEngine>>,
}

impl TextEngineSlot {
    pub fn new(model_path: PathBuf, tokenizer_path: PathBuf) -> Self {
        Self {
            model_path,
            tokenizer_path,
            inner: Mutex::new(None),
        }
    }

    /// 权重与分词器是否**都在**。
    pub fn available(&self) -> bool {
        self.model_path.is_file() && self.tokenizer_path.is_file()
    }

    pub fn model_path(&self) -> &Path {
        &self.model_path
    }

    pub fn tokenizer_path(&self) -> &Path {
        &self.tokenizer_path
    }

    /// 懒加载引擎；已加载则复用。
    fn ensure_loaded(&self) -> Result<(), String> {
        if !self.available() {
            return Err(format!(
                "文本塔资源缺失（需要 {} 与 {}）",
                self.model_path.display(),
                self.tokenizer_path.display()
            ));
        }
        let mut guard = self.inner.lock().map_err(|_| "文本引擎锁已中毒")?;
        if guard.is_none() {
            let tokenizer = WordPieceTokenizer::from_file(&self.tokenizer_path)?;
            *guard = Some(TextEngine::new(self.model_path.clone(), tokenizer));
        }
        Ok(())
    }

    /// 用引擎做一件事。加载失败 → Err（调用方据此把功能标成不可用）。
    pub fn with_engine<T>(&self, f: impl FnOnce(&TextEngine) -> T) -> Result<T, String> {
        self.ensure_loaded()?;
        let guard = self.inner.lock().map_err(|_| "文本引擎锁已中毒")?;
        let engine = guard.as_ref().ok_or("文本引擎不可用")?;
        Ok(f(engine))
    }
}

/// 找出装着文本塔权重的目录（权重与分词器**都要**在）。
///
/// 找不到就返回 None —— 调用方据此把「文本塔」功能标为不可用，
/// 而不是让应用崩在启动时。
pub fn resolve_text_model_dir(app: &tauri::AppHandle) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(dir) = std::env::var("AI_IMAGE_SORTER_MODELS") {
        candidates.push(PathBuf::from(dir));
    }
    // debug 构建：仓库里的 resources/models（打包后不存在这一项）
    #[cfg(debug_assertions)]
    {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        candidates.push(manifest.join("resources").join("models"));
    }
    // 打包后：Tauri 的资源目录
    if let Ok(dir) = app.path().resolve("models", tauri::path::BaseDirectory::Resource) {
        candidates.push(dir);
    }
    // 便携模式：exe 同级 data/models
    candidates.push(crate::paths::data_root(app).join("models"));
    candidates.into_iter().find(|dir| {
        dir.join(TEXT_WEIGHTS).is_file() && dir.join(TEXT_TOKENIZER).is_file()
    })
}

// ───────────────────────────── 闸门（测试） ─────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    /// 产品侧的分词器文件（`scripts/download_models.ps1` 下载到这里的）。
    fn product_tokenizer_path() -> PathBuf {
        manifest_dir()
            .join("resources")
            .join("models")
            .join(TEXT_TOKENIZER)
    }

    fn product_weights_path() -> PathBuf {
        manifest_dir()
            .join("resources")
            .join("models")
            .join(TEXT_WEIGHTS)
    }

    fn lab_dir() -> PathBuf {
        manifest_dir().join("..").join("tools").join("l2lab")
    }

    /// FNV-1a 64 位校验和（与 `tools/l2lab/dump_text_tokens.py` 同算法）。
    fn fnv1a64(data: &[u8]) -> String {
        let mut hash: u64 = 0xcbf29ce484222325;
        for byte in data {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        format!("{hash:016x}")
    }

    /// 余弦相似度。
    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        let n = a.len().min(b.len());
        let mut dot = 0f32;
        let mut na = 0f32;
        let mut nb = 0f32;
        for i in 0..n {
            dot += a[i] * b[i];
            na += a[i] * a[i];
            nb += b[i] * b[i];
        }
        if na <= 0.0 || nb <= 0.0 {
            return 0.0;
        }
        dot / (na.sqrt() * nb.sqrt())
    }

    /// 真实库的位置（与 `tools/l2lab/embed.py` 同一处）。
    fn real_db_path() -> PathBuf {
        let app_data = std::env::var("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default())
                    .join("AppData")
                    .join("Roaming")
            });
        app_data
            .join("com.aiimagesorter.app")
            .join("ai-image-sorter.db")
    }

    /// 按 id 取 prompt —— **只在内存里**，不落盘、不进日志（P1 护栏）。
    ///
    /// 返回 None 表示库不可用或某条 prompt 已被清空（库变了 → 闸门跳过）。
    fn read_prompts_by_ids(ids: &[i64]) -> Option<Vec<(i64, String)>> {
        let path = real_db_path();
        if !path.is_file() {
            return None;
        }
        let conn = rusqlite::Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .ok()?;
        let mut stmt = conn
            .prepare("SELECT id, prompt_pos FROM images WHERE id = ?1 AND prompt_pos != ''")
            .ok()?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let mut row = stmt
                .query_row([*id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
                .ok()?;
            out.push(std::mem::replace(&mut row, (0, String::new())));
        }
        Some(out)
    }

    // ── 分词器的语义单测（解释「为什么」，fixture 闸门证明「一样」） ──

    /// `clean_text`：控制字符与替换字符被删，空白归一为 ' '。
    #[test]
    fn clean_text_drops_controls_and_normalizes_space() {
        assert_eq!(clean_text("ab\x00c\x07d\u{FFFD} e"), "abcd e");
        assert_eq!(clean_text("tab\there\nnl\rcr"), "tab here nl cr");
        // 非断空格 / 表意空格也是空白
        assert_eq!(clean_text("a\u{00a0}b\u{3000}c"), "a b c");
    }

    /// `handle_chinese_chars`：CJK 前后各插一个空格。
    ///
    /// 注意相邻两个汉字之间是**两个**空格（前一字的尾空格 + 后一字的头空格），
    /// 这是 HF 的行为（实测 `normalize_str('甲乙')` = `' 甲  乙 '`）。
    /// 对分词结果没有影响 —— 预分词会把连续空白折叠掉。
    #[test]
    fn chinese_chars_get_surrounded_by_space() {
        assert_eq!(handle_chinese_chars("甲乙"), " 甲  乙 ");
        assert_eq!(handle_chinese_chars("甲"), " 甲 ");
        // CJK 扩展 A 也算
        assert_eq!(handle_chinese_chars("\u{3400}"), " \u{3400} ");
        // 假名不算（HF 明确说明：那套文字是空格分隔的）
        assert_eq!(handle_chinese_chars("ア"), "ア");
    }

    /// `pre_tokenize`：标点被隔离成独立片段。
    #[test]
    fn pre_tokenizer_isolates_punctuation() {
        assert_eq!(
            pre_tokenize("alpha, bravo"),
            vec!["alpha".to_string(), ",".to_string(), "bravo".to_string()]
        );
        // 全角逗号也是标点（Unicode P*）
        assert_eq!(
            pre_tokenize("甲，乙"),
            vec!["甲".to_string(), "，".to_string(), "乙".to_string()]
        );
        // 连续标点各自成片段
        assert_eq!(
            pre_tokenize("..."),
            vec![".".to_string(), ".".to_string(), ".".to_string()]
        );
        // 连续空白不产生空片段
        assert_eq!(pre_tokenize("a   b"), vec!["a".to_string(), "b".to_string()]);
    }

    /// WordPiece：整词匹配不上就**整个词**退化成 [UNK]，不逐字符退化。
    #[test]
    fn wordpiece_falls_back_to_whole_word_unk() {
        let mut vocab = HashMap::new();
        vocab.insert("ab".to_string(), 1i64);
        vocab.insert("##c".to_string(), 2i64);
        // "abc" 能匹配上（ab + ##c）
        assert_eq!(wordpiece_ids("abc", &vocab, "##", 100, 0), vec![1, 2]);
        // "abd" 匹配不上 → 整词 [UNK]（不是 [1, 0]）
        assert_eq!(wordpiece_ids("abd", &vocab, "##", 100, 0), vec![0]);
    }

    /// WordPiece 的 100 字符边界：判据是 **> 100**，不是 >= 100。
    #[test]
    fn wordpiece_length_boundary_is_exclusive() {
        let mut vocab = HashMap::new();
        vocab.insert("x".to_string(), 1i64);
        vocab.insert("##x".to_string(), 2i64);
        // 恰好 100 字符 → 正常贪心匹配（100 个片段）
        let exact = "x".repeat(100);
        let ids = wordpiece_ids(&exact, &vocab, "##", 100, 0);
        assert_eq!(ids.len(), 100);
        assert_ne!(ids, vec![0]);
        // 101 字符 → 整词 [UNK]
        let over = "x".repeat(101);
        assert_eq!(wordpiece_ids(&over, &vocab, "##", 100, 0), vec![0]);
    }

    // ── 闸门 ──

    /// **分词闸门**：Rust 的 token id 必须与 Python `tokenizers` 0.23.3
    /// **逐条相等**。
    ///
    /// 基准是 `tools/l2lab/fixtures/text_tokens.json`（**合成语料**，可提交，
    /// 任何人都能跑这道闸门）。真实 prompt 是用户私密内容，不能进仓库。
    #[test]
    fn tokenizer_matches_python_reference() {
        let tokenizer_path = product_tokenizer_path();
        if !tokenizer_path.is_file() {
            eprintln!("跳过：未安装分词器（scripts/download_models.ps1）");
            return;
        }
        let fixture_path = lab_dir().join("fixtures").join("text_tokens.json");
        if !fixture_path.is_file() {
            eprintln!("跳过：没有分词基准（先跑 tools/l2lab/dump_text_tokens.py）");
            return;
        }
        let fixture: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&fixture_path).unwrap()).unwrap();

        // 产品侧的分词器文件必须与生成基准时用的那一份 —— 否则这道闸门
        // 在测别的东西（换模型/换词表 → 必须重生成基准）。
        let want_hash = fixture
            .get("tokenizer_fnv1a64")
            .and_then(|v| v.as_str())
            .unwrap();
        let got_hash = fnv1a64(&std::fs::read(&tokenizer_path).unwrap());
        assert_eq!(
            got_hash, want_hash,
            "产品侧的分词器文件与生成基准时用的不是同一份（重跑 tools/l2lab/dump_text_tokens.py）"
        );

        let tokenizer = WordPieceTokenizer::from_file(&tokenizer_path).expect("载入分词器失败");
        let want_vocab = fixture.get("vocab_size").and_then(|v| v.as_u64()).unwrap() as usize;
        assert_eq!(
            tokenizer.vocab_size(),
            want_vocab,
            "词表大小与基准不符"
        );

        let cases = fixture.get("cases").and_then(|v| v.as_array()).unwrap();
        assert!(!cases.is_empty(), "基准里没有用例");
        let mut mismatches: Vec<String> = Vec::new();
        for case in cases {
            let name = case.get("name").and_then(|v| v.as_str()).unwrap();
            let text = case.get("text").and_then(|v| v.as_str()).unwrap();
            let want_ids: Vec<i64> = case
                .get("ids")
                .and_then(|v| v.as_array())
                .unwrap()
                .iter()
                .map(|x| x.as_i64().unwrap())
                .collect();
            let want_types: Vec<i64> = case
                .get("type_ids")
                .and_then(|v| v.as_array())
                .unwrap()
                .iter()
                .map(|x| x.as_i64().unwrap())
                .collect();
            let (got_ids, got_types) = tokenizer.encode(text);
            if got_ids != want_ids || got_types != want_types {
                mismatches.push(format!(
                    "{name}\n    期望 ids {want_ids:?}\n    实得 ids {got_ids:?}"
                ));
            }
        }
        assert!(
            mismatches.is_empty(),
            "分词闸门失败（{} 条不符）：\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );
        eprintln!("分词闸门: {} 条用例逐条相等", cases.len());
    }

    /// **向量闸门**：Rust 的 BGE 向量必须与实验室参考（`out/text.json`）一致。
    ///
    /// 基准是 `tools/l2lab/out/text.json`（真库 prompt 算出来的向量，
    /// 该目录已 gitignore —— 向量可逆，绝不能进仓库）。缺基准时跳过。
    ///
    /// 关键：**按参考文件的 id 顺序复刻实验室的批次构成**（每 8 条一组、
    /// padding 宽度取批内最长）。不这么做的话，余弦会被 int8 量化的
    /// 数值噪声（实测 0.986~0.996）压下来，0.97 的阈值就没有判别力了。
    #[test]
    fn text_pipeline_matches_lab_reference() {
        let weights = product_weights_path();
        let tokenizer_path = product_tokenizer_path();
        if !weights.is_file() || !tokenizer_path.is_file() {
            eprintln!("跳过：未安装文本塔权重（scripts/download_models.ps1）");
            return;
        }
        let reference_path = lab_dir().join("out").join("text.json");
        if !reference_path.is_file() {
            eprintln!("跳过：没有实验室参考向量（先跑 tools/l2lab/embed.py --mode text）");
            return;
        }
        let reference: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&reference_path).unwrap()).unwrap();
        let items = reference.get("items").and_then(|v| v.as_array()).unwrap();
        if items.is_empty() {
            eprintln!("跳过：参考向量是空的");
            return;
        }
        let ids: Vec<i64> = items
            .iter()
            .map(|e| e.get("id").and_then(|v| v.as_i64()).unwrap())
            .collect();
        let Some(prompts) = read_prompts_by_ids(&ids) else {
            eprintln!("跳过：真实库不可用，或有 prompt 已被清空（库变了 → 请重生成参考）");
            return;
        };

        let tokenizer = WordPieceTokenizer::from_file(&tokenizer_path).expect("载入分词器失败");
        let engine = TextEngine::new(weights, tokenizer);
        let started = std::time::Instant::now();
        let (rows, _failed) = engine
            .embed_prompts(&prompts, &mut |_, _| {}, &|| false)
            .expect("文本推理失败");
        eprintln!(
            "向量闸门: {} 条 · 用时 {:.1}s",
            rows.len(),
            started.elapsed().as_secs_f64()
        );
        assert_eq!(rows.len(), items.len(), "条数对不上");

        let mut min_cos = 1f32;
        let mut worst_id = 0i64;
        let mut cosines: Vec<f32> = Vec::with_capacity(rows.len());
        for ((id, got), entry) in rows.iter().zip(items.iter()) {
            assert_eq!(*id, entry.get("id").and_then(|v| v.as_i64()).unwrap());
            let want: Vec<f32> = entry
                .get("vec")
                .and_then(|v| v.as_array())
                .unwrap()
                .iter()
                .map(|x| x.as_f64().unwrap() as f32)
                .collect();
            let cos = cosine(got, &want);
            cosines.push(cos);
            if cos < min_cos {
                min_cos = cos;
                worst_id = *id;
            }
        }
        // 分布（诊断用）：分位数 + 落在各档的数量。
        let mut sorted = cosines.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let pct = |p: f64| -> f32 {
            let i = ((sorted.len() as f64 - 1.0) * p).round() as usize;
            sorted[i]
        };
        let mean: f32 = cosines.iter().sum::<f32>() / cosines.len() as f32;
        let median = pct(0.5);
        let drifted: Vec<i64> = rows
            .iter()
            .zip(cosines.iter())
            .filter(|(_, c)| **c < 0.999)
            .map(|((id, _), _)| *id)
            .collect();
        eprintln!(
            "向量闸门: {} 条 · 最小 {min_cos:.8}（图 {worst_id}）· 均值 {mean:.8} · \
             p50 {median:.8} · p10 {:.8} · p01 {:.8} · <0.999 {} 条",
            rows.len(),
            pct(0.1),
            pct(0.01),
            drifted.len(),
        );
        if !drifted.is_empty() {
            let head: Vec<String> = drifted.iter().take(10).map(|i| i.to_string()).collect();
            eprintln!("  余弦偏低的 id（最多列 10 个）: {}", head.join(", "));
        }
        // 阈值（比 ui-program.md 的 0.97 更严一档，理由见下）：
        //  · 最小余弦 > 0.97 —— 口径书定的阈值，兜住「管线写错」这种灾难；
        //  · **中位数 > 0.999** —— 这才是真正有判别力的那一档。
        // 实测：批次构成复刻到位且参考是新的时，中位数 = 1.00000000；
        // 若参考是旧的（库里的 prompt 在生成参考之后被改过），个别行会掉到
        // 0.99 附近 —— 那时中位数仍是 1.0，闸门照样通过（测的是管线，
        // 管线没坏）。反过来，管线坏了（分词错 / 取错 CLS / 归一化错）
        // 会让**所有**行一起掉下来，中位数立刻塌陷。
        //  0.97 那一档单独用是不够的：它容得下「参考陈旧导致整体偏 0.995」
        // 这种完全蒙混过关的情形。
        assert!(
            min_cos > 0.97,
            "向量闸门失败：最小余弦 {min_cos}（图 {worst_id}）。\
             若库里的 prompt 在参考生成之后被改过，先重生成参考：\
             tools/l2lab/.venv/Scripts/python.exe tools/l2lab/embed.py --mode text --out tools/l2lab/out/text.json"
        );
        assert!(
            median > 0.999,
            "向量闸门失败：中位余弦只有 {median} —— 管线有问题（而不是个别行漂移）。\
             参考可能陈旧，先重生成参考：\
             tools/l2lab/.venv/Scripts/python.exe tools/l2lab/embed.py --mode text --out tools/l2lab/out/text.json"
        );
    }
}
