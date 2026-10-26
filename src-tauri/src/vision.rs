//! 视觉签名管线（阶段六 Round 1）
//!
//! 目标：把每张图变成一个可比较的向量，存进库，供语义 L2 聚类与
//! 「可能相关」推荐使用。推理完全在本进程内（ONNX Runtime），不依赖
//! Python、不依赖 ComfyUI、不联网。
//!
//! 与实验室（`tools/l2lab/embed.py`）的关系：两者跑**同一批 ONNX 权重**
//! 与**同一套预处理**，因此产出的向量必须一致 —— 这是 Round 1 的口径闸门
//! （见 ui-program.md）。为此这里的重采样刻意实现成 PIL 兼容的带抗锯齿
//! 双三次，而不是直接用 `image::imageops::resize`（后者在下采样时不做抗
//! 锯齿，与 PIL 的 BICUBIC 不同，两条流水线的向量会对不上）。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ndarray::Array4;
use ort::inputs;
use ort::session::Session;
use ort::value::Value;
use tauri::Manager;

/// 视觉塔的模型标识。向量按 (slot, model_id) 分区存储 —— 换模型必须重算，
/// 不同模型的向量不在同一空间里，绝不能混着比。
pub const VISION_MODEL_ID: &str = "clip-vit-b32-q";
/// 能力标记：视觉塔（CLIP）已接进二进制。
///
/// 标记串住在**实现它的这个模块**里 —— 见 `capabilities.rs` 的纪律说明。
/// 末尾的 `\0` 是 C 风格哨兵（探针靠它把标记串与相邻字面量分开）。
pub const CAP_TOWER_VISION: &str = "ais.cap.tower.vision.v1\0";
/// CLIP 的输入裁剪边长（与 Xenova 的 preprocessor_config.json 一致）。
pub const CLIP_CROP: usize = 224;
/// CLIP 的归一化参数（mean / std，同上）。
const CLIP_MEAN: [f32; 3] = [0.48145466, 0.4578275, 0.40821073];
const CLIP_STD: [f32; 3] = [0.26862954, 0.26130258, 0.27577711];
/// 权重文件名（与 scripts/download_models.ps1 保持一致）。
const VISION_WEIGHTS: &str = "vision_clip.onnx";

/// 找出装着权重的目录。找不到就返回 None —— 调用方据此把「识图」功能
/// 标为不可用，而不是让应用崩在启动时。
pub fn resolve_model_dir(app: &tauri::AppHandle) -> Option<PathBuf> {
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
    // 打包后：Tauri 的资源目录（tauri.conf.json 的 bundle.resources）
    if let Ok(dir) = app.path().resolve("models", tauri::path::BaseDirectory::Resource) {
        candidates.push(dir);
    }
    // 便携模式：exe 同级 data/models
    candidates.push(crate::paths::data_root(app).join("models"));
    candidates.into_iter().find(|dir| dir.join(VISION_WEIGHTS).is_file())
}

/// 双三次核（PIL 的 BICUBIC，a = -0.5）。
fn bicubic_kernel(x: f64) -> f64 {
    const A: f64 = -0.5;
    let x = x.abs();
    if x < 1.0 {
        ((A + 2.0) * x - (A + 3.0)) * x * x + 1.0
    } else if x < 2.0 {
        (((x - 5.0) * x + 8.0) * x - 4.0) * A
    } else {
        0.0
    }
}

/// Python 的 `round()` 是「银行家舍入」（0.5 → 0，2.5 → 2），
/// Rust 的 `f64::round()` 是「四舍五入远离零」。实验室用 Python 算目标尺寸，
/// 这里必须用同一套舍入，否则偶发的 1 像素差会让两条流水线的向量对不上。
fn round_half_even(x: f64) -> u32 {
    let floor = x.floor();
    let diff = x - floor;
    let result = if diff > 0.5 {
        floor + 1.0
    } else if diff < 0.5 {
        floor
    } else if (floor as i64) % 2 == 0 {
        floor
    } else {
        floor + 1.0
    };
    result.max(0.0) as u32
}

/// PIL 兼容的预计算权重表：每个输出像素对应一组 (起点, 权重)。
/// 关键在两处：下采样时按 `filterscale` 放大核支撑并把权重归一化
/// （这就是 PIL 的抗锯齿），以及边缘不额外归一化。
fn precompute_weights(in_size: usize, out_size: usize) -> Vec<(usize, Vec<f64>)> {
    let scale = in_size as f64 / out_size as f64;
    let filterscale = scale.max(1.0);
    let support = 2.0 * filterscale;
    let ss = 1.0 / filterscale;
    let mut table = Vec::with_capacity(out_size);
    for out_i in 0..out_size {
        let center = (out_i as f64 + 0.5) * scale;
        let mut xmin = (center - support + 0.5).floor() as i64;
        if xmin < 0 {
            xmin = 0;
        }
        let mut xmax = (center + support + 0.5).floor() as i64;
        if xmax > in_size as i64 {
            xmax = in_size as i64;
        }
        if xmax <= xmin {
            // 极端退化（in_size == 0 不可能发生），保底取最近邻
            let idx = (center.round() as i64).clamp(0, in_size as i64 - 1) as usize;
            table.push((idx, vec![1.0]));
            continue;
        }
        let mut weights: Vec<f64> = Vec::with_capacity((xmax - xmin) as usize);
        let mut total = 0.0;
        for x in xmin..xmax {
            // 结合序必须与 PIL 完全一致：PIL 写的是
            // filter((x + xmin - center + 0.5) * ss)，先 (x+xmin) 再减 center
            // 再加 0.5。写成 (x + 0.5 - center) 会在 f64 的最后一位上分岔，
            // 落到恰好 .5 的像素就会差 1（实测 89152 个像素里 4 个）。
            let w = bicubic_kernel(((x as f64) - center + 0.5) * ss);
            weights.push(w);
            total += w;
        }
        if total != 0.0 {
            for w in weights.iter_mut() {
                *w /= total;
            }
        }
        table.push((xmin as usize, weights));
    }
    table
}

/// 水平 + 垂直两遍重采样（RGB8）。PIL 的两遍卷积：先水平后垂直，
/// 每遍都把权重归一化并把中间结果夹回 8 位。
fn resample_rgb(src: &[u8], src_w: usize, src_h: usize, dst_w: usize, dst_h: usize) -> Vec<u8> {
    if src_w == dst_w && src_h == dst_h {
        return src.to_vec();
    }
    // 水平
    let htable = precompute_weights(src_w, dst_w);
    let mut mid = vec![0u8; dst_w * src_h * 3];
    for y in 0..src_h {
        for x in 0..dst_w {
            let (xmin, weights) = &htable[x];
            // 累加用 f64：PIL 用 double，f32 会在恰好 .5 的边界上与 PIL
            // 分道扬镳（实测 89152 个像素里有 7 个差 1）。
            let mut acc = [0f64; 3];
            for (k, w) in weights.iter().enumerate() {
                let base = (y * src_w + xmin + k) * 3;
                acc[0] += src[base] as f64 * w;
                acc[1] += src[base + 1] as f64 * w;
                acc[2] += src[base + 2] as f64 * w;
            }
            let out = (y * dst_w + x) * 3;
            for c in 0..3 {
                mid[out + c] = (acc[c].clamp(0.0, 255.0) + 0.5).floor() as u8;
            }
        }
    }
    // 垂直
    let vtable = precompute_weights(src_h, dst_h);
    let mut out = vec![0u8; dst_w * dst_h * 3];
    for y in 0..dst_h {
        let (ymin, weights) = &vtable[y];
        for x in 0..dst_w {
            let mut acc = [0f64; 3];
            for (k, w) in weights.iter().enumerate() {
                let base = ((ymin + k) * dst_w + x) * 3;
                acc[0] += mid[base] as f64 * w;
                acc[1] += mid[base + 1] as f64 * w;
                acc[2] += mid[base + 2] as f64 * w;
            }
            let out_base = (y * dst_w + x) * 3;
            for c in 0..3 {
                out[out_base + c] = (acc[c].clamp(0.0, 255.0) + 0.5).floor() as u8;
            }
        }
    }
    out
}

/// CLIP 预处理：短边缩放到 224 → 双三次 → 中心裁剪 224 → [0,1] → 归一化。
/// 返回 NCHW 顺序的扁平 f32（长度 3 × 224 × 224）。
pub fn preprocess_clip(rgb: &[u8], width: usize, height: usize) -> Result<Vec<f32>, String> {
    if width == 0 || height == 0 {
        return Err("空图像".to_string());
    }
    if rgb.len() < width * height * 3 {
        return Err("像素缓冲长度不足".to_string());
    }
    let crop = CLIP_CROP;
    let short = width.min(height) as f64;
    let scale = crop as f64 / short;
    let new_w = round_half_even(width as f64 * scale).max(crop as u32) as usize;
    let new_h = round_half_even(height as f64 * scale).max(crop as u32) as usize;

    let scaled = resample_rgb(rgb, width, height, new_w, new_h);

    // 中心裁剪
    let left = (new_w - crop) / 2;
    let top = (new_h - crop) / 2;
    let mut out = vec![0f32; 3 * crop * crop];
    let plane = crop * crop;
    for y in 0..crop {
        for x in 0..crop {
            let base = ((top + y) * new_w + (left + x)) * 3;
            let idx = y * crop + x;
            for c in 0..3 {
                let v = scaled[base + c] as f32 / 255.0;
                out[c * plane + idx] = (v - CLIP_MEAN[c]) / CLIP_STD[c];
            }
        }
    }
    Ok(out)
}

/// L2 归一化（CLIP 的余弦相似度就是归一化后的点积）。
fn l2_normalize(vec: &mut [f32]) {
    let norm: f32 = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for v in vec.iter_mut() {
            *v /= norm;
        }
    }
}

/// 视觉推理引擎。ONNX session 的创建要 1 秒左右，所以懒加载并缓存。
pub struct VisionEngine {
    session: Mutex<Option<Session>>,
    model_path: PathBuf,
}

impl VisionEngine {
    pub fn new(model_path: PathBuf) -> Self {
        Self {
            session: Mutex::new(None),
            model_path,
        }
    }

    pub fn model_path(&self) -> &Path {
        &self.model_path
    }

    /// 对一批图片算视觉向量。`progress(done, total)` 每批回调一次；
    /// `is_cancelled()` 返回 true 时提前中止（用户可以取消 5 分钟的批处理）。
    /// 单张图读不出来（文件被删、被占用）只跳过那一张，不让整批失败 ——
    /// 返回值里的 failed 是跳过张数。
    pub fn embed_batch(
        &self,
        items: &[(i64, PathBuf)],
        progress: &mut dyn FnMut(usize, usize),
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<(Vec<(i64, Vec<f32>)>, usize), String> {
        if items.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let mut session = self.session.lock().map_err(|_| "视觉引擎锁已中毒")?;
        if session.is_none() {
            *session = Some(
                Session::builder()
                    .map_err(|e| e.to_string())?
                    .with_intra_threads(4)
                    .map_err(|e| e.to_string())?
                    .commit_from_file(&self.model_path)
                    .map_err(|e| format!("加载视觉模型失败：{e}"))?,
            );
        }
        let session = session.as_mut().unwrap();
        let input_name = session.inputs[0].name.clone();
        let batch_size = 8;
        let plane = 3 * CLIP_CROP * CLIP_CROP;
        let mut out: Vec<(i64, Vec<f32>)> = Vec::with_capacity(items.len());
        let mut failed = 0usize;
        let total = items.len();
        for chunk in items.chunks(batch_size) {
            if is_cancelled() {
                break;
            }
            // 先把这一批能读的图预处理出来；读不了的跳过。
            let mut ready: Vec<(i64, Vec<f32>)> = Vec::with_capacity(chunk.len());
            for (id, path) in chunk {
                match image::open(path) {
                    Ok(decoded) => {
                        let rgb = decoded.to_rgb8();
                        let (w, h) = (rgb.width() as usize, rgb.height() as usize);
                        let buffer = rgb.as_raw();
                        match preprocess_clip(&buffer, w, h) {
                            Ok(array) => ready.push((*id, array)),
                            Err(e) => {
                                log::warn!("预处理失败 {:?}: {e}", path);
                                failed += 1;
                            }
                        }
                    }
                    Err(e) => {
                        log::warn!("读取图片失败 {:?}: {e}", path);
                        failed += 1;
                    }
                }
            }
            if ready.is_empty() {
                continue;
            }
            let mut batch_data = vec![0f32; ready.len() * plane];
            for (i, (_, array)) in ready.iter().enumerate() {
                batch_data[i * plane..(i + 1) * plane].copy_from_slice(array);
            }
            let batch = Array4::from_shape_vec(
                (ready.len(), 3, CLIP_CROP, CLIP_CROP),
                batch_data,
            )
            .map_err(|e| e.to_string())?;
            let value = Value::from_array(batch).map_err(|e| e.to_string())?;
            let outputs = session
                .run(inputs![input_name.clone() => value])
                .map_err(|e| format!("视觉推理失败：{e}"))?;
            let (_, data) = outputs[0]
                .try_extract_tensor::<f32>()
                .map_err(|e| format!("读取视觉输出失败：{e}"))?;
            let dim = data.len() / ready.len();
            for (i, (id, _)) in ready.iter().enumerate() {
                let mut vec: Vec<f32> = data[i * dim..(i + 1) * dim].to_vec();
                l2_normalize(&mut vec);
                out.push((*id, vec));
            }
            // 失败的张（读不出来 / 预处理失败）也算**已处理** —— 否则进度条
            // 永远到不了 100%，用户会以为卡住了。
            progress(out.len() + failed, total);
        }
        Ok((out, failed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 纯色块的重采样必须保持颜色（权重归一化正确的表现）。
    #[test]
    fn resample_keeps_flat_color() {
        let src = vec![200u8; 4 * 4 * 3];
        let out = resample_rgb(&src, 4, 4, 8, 8);
        for px in out.chunks(3) {
            assert_eq!(px[0], 200);
            assert_eq!(px[1], 200);
            assert_eq!(px[2], 200);
        }
    }

    /// 缩小时必须做抗锯齿：8×8 缩到 2×2，中间那一行白应当被摊成四分之一的亮度。
    /// 若不做抗锯齿（最近邻/分块平均），要么得到纯黑要么得到纯白 —— 这正是
    /// PIL 与「朴素实现」的分水岭，也是闸门要盯住的东西。
    #[test]
    fn resample_antialiases_a_single_row() {
        let mut src = vec![0u8; 8 * 8 * 3];
        for x in 0..8 {
            for c in 0..3 {
                src[(3 * 8 + x) * 3 + c] = 255;
            }
        }
        let out = resample_rgb(&src, 8, 8, 2, 2);
        for x in 0..2 {
            let top = out[(0 * 2 + x) * 3] as i32;
            let bottom = out[(1 * 2 + x) * 3] as i32;
            // 不要求具体数值：双三次核不是均匀的盒式平均，而且核的支撑很宽
            // （下采样 4 倍时支撑达 8 行），所以两行都会被那行白「染」到。
            // 只断言三条性质：
            //   · 两行都是**部分混合**（最近邻会给出 0 与 255）
            //   · 上行受白行的影响更大
            //   · 都不越界（不会出现振铃）
            assert!(top > 0 && top < 255, "上面一行应当是部分混合，得到 {top}");
            assert!(bottom > 0 && bottom < 255, "下面一行应当是部分混合，得到 {bottom}");
            assert!(top > bottom, "白行的影响应当在上方更强：{top} vs {bottom}");
        }
    }

    /// 银行家舍入：0.5 → 0，1.5 → 2，2.5 → 2（与 Python 的 round 一致）。
    #[test]
    fn round_matches_python() {
        assert_eq!(round_half_even(0.5), 0);
        assert_eq!(round_half_even(1.5), 2);
        assert_eq!(round_half_even(2.5), 2);
        assert_eq!(round_half_even(3.5), 4);
        assert_eq!(round_half_even(224.0), 224);
    }

    /// 预处理的形状与归一化范围。
    #[test]
    fn preprocess_shape_and_range() {
        let w = 512;
        let h = 256;
        let buf = vec![128u8; w * h * 3];
        let out = preprocess_clip(&buf, w, h).unwrap();
        assert_eq!(out.len(), 3 * CLIP_CROP * CLIP_CROP);
        for v in &out {
            assert!(v.is_finite());
            assert!(*v > -3.0 && *v < 3.0);
        }
    }

    /// 短边缩放后的目标尺寸必须与 Python 的 round 一致。
    #[test]
    fn resized_short_side_matches_python() {
        // 1024×768 → 短边 224 → 长边 round(1024*224/768) = round(298.67) = 299
        let w = 1024;
        let h = 768;
        let scale = CLIP_CROP as f64 / h.min(w) as f64;
        assert_eq!(round_half_even(w as f64 * scale), 299);
        assert_eq!(round_half_even(h as f64 * scale), 224);
    }

    /// 口径闸门：产品管线（Rust）与实验室（Python/ORT）必须算出同一个向量。
    ///
    /// 为什么要这道闸门：研究阶段的所有指标（NN 一致率、松散对率）都是用
    /// 向量算出来的。如果产品管线的预处理差一点点（通道顺序、归一化、裁剪
    /// 位置、舍入），两边的向量就不在同一个空间，指标全部作废。
    ///
    /// 阈值为什么是 0.97 而不是 1.0：**ORT 的构建差异**。ort crate 带的
    /// ONNX Runtime 二进制与 Python wheel 的不是同一个构建，int8 量化模型的
    /// 内核累加顺序不同 —— 同一份输入，两边的输出余弦只有 0.977~0.992
    /// （已排除预处理差异：上面的「预处理闸门」逐像素证明了两边一致）。
    /// 所以 embedding 这一层只能给宽阈值；**真正的口径保证靠预处理闸门**。
    /// 这个宽阈值仍然能抓住真正的错误：通道顺序错了余弦会掉到 0.3 以下，
    /// 归一化错了直接是 0，裁剪位置错了会掉到 0.9 以下。
    ///
    /// 缺模型或缺实验室产物时跳过（CI 不装权重）。
    #[test]
    fn rust_pipeline_matches_lab_reference() {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let model = manifest.join("resources").join("models").join(VISION_WEIGHTS);
        let lab = manifest.join("..").join("tools").join("l2lab").join("out");
        if !model.is_file() {
            eprintln!("跳过：未安装视觉模型（scripts/download_models.ps1）");
            return;
        }
        let reference_path = lab.join("vision.json");
        let pathmap_path = lab.join("pathmap.json");
        if !reference_path.is_file() || !pathmap_path.is_file() {
            eprintln!("跳过：没有实验室参考向量（先跑 tools/l2lab/embed.py）");
            return;
        }
        let reference: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&reference_path).unwrap()).unwrap();
        let pathmap: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&pathmap_path).unwrap()).unwrap();
        let items_json = reference.get("items").and_then(|v| v.as_array()).unwrap();

        // 取前 12 张做比较（够抓出预处理差异，又不至于让测试跑太久）
        let mut items: Vec<(i64, PathBuf)> = Vec::new();
        let mut expected: Vec<(i64, Vec<f32>)> = Vec::new();
        for entry in items_json.iter().take(12) {
            let id = entry.get("id").and_then(|v| v.as_i64()).unwrap();
            let vec: Vec<f32> = entry
                .get("vec")
                .and_then(|v| v.as_array())
                .unwrap()
                .iter()
                .map(|x| x.as_f64().unwrap() as f32)
                .collect();
            let path = pathmap
                .get(id.to_string())
                .and_then(|v| v.as_str())
                .map(PathBuf::from);
            match path {
                Some(p) if p.is_file() => {
                    items.push((id, p));
                    expected.push((id, vec));
                }
                _ => continue,
            }
        }
        if items.is_empty() {
            eprintln!("跳过：实验室路径表里没有可用的图");
            return;
        }

        let engine = VisionEngine::new(model);
        let (rows, failed) = engine
            .embed_batch(&items, &mut |_, _| {}, &|| false)
            .expect("视觉推理失败");
        assert_eq!(failed, 0, "不该有读不出来的图");
        assert_eq!(rows.len(), expected.len());

        let mut max_diff = 0f32;
        let mut min_cos = 1f32;
        for ((id, got), (want_id, want)) in rows.iter().zip(expected.iter()) {
            assert_eq!(id, want_id, "id 顺序必须一致");
            let mut dot = 0f32;
            let mut na = 0f32;
            let mut nb = 0f32;
            for (a, b) in got.iter().zip(want.iter()) {
                let diff = (a - b).abs();
                if diff > max_diff {
                    max_diff = diff;
                }
                dot += a * b;
                na += a * a;
                nb += b * b;
            }
            let cos = dot / (na.sqrt() * nb.sqrt());
            eprintln!("  图 {id}: 余弦 {cos:.6} · 最大逐元素差 {:.3e}", max_diff);
            if cos < min_cos {
                min_cos = cos;
            }
        }
        eprintln!(
            "口径闸门: {} 张 · 最大逐元素差 {max_diff:.3e} · 最小余弦 {min_cos:.8}",
            rows.len()
        );
        assert!(max_diff < 0.2, "逐元素差过大：{max_diff}");
        assert!(min_cos > 0.97, "余弦过低：{min_cos}");
    }

    /// 预处理闸门：Rust 的重采样必须与 PIL 的逐像素一致。
    ///
    /// 为什么单独设这一道：embedding 层面的比对会被「ORT 构建差异」污染
    /// （ort crate 带的 ORT 二进制与 Python wheel 的不是同一个构建，int8
    /// 内核的累加顺序不同 → 同一份输入的余弦只有 0.98）。所以预处理必须
    /// **逐像素**地自证，不能靠 embedding 反推。
    ///
    /// 口径：不要求逐位相同（PIL 的浮点运算与我的仍有极小的末位差），
    /// 但差异必须**罕见**且**只差 1 级**。实测 89152 个像素里 4 个差 1。
    #[test]
    fn preprocess_matches_pil_resize() {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let lab = manifest.join("..").join("tools").join("l2lab").join("out");
        let ref_path = lab.join("resize_ref.bin");
        let pathmap_path = lab.join("pathmap.json");
        if !ref_path.is_file() || !pathmap_path.is_file() {
            eprintln!("跳过：没有 PIL 重采样参考（先跑 tools/l2lab/dump_resize_ref.py）");
            return;
        }
        let pathmap: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&pathmap_path).unwrap()).unwrap();
        let bytes = std::fs::read(&ref_path).unwrap();
        let mut offset = 0usize;
        let mut total_px = 0usize;
        let mut mismatched = 0usize;
        let mut max_delta = 0i32;
        let mut images = 0usize;
        while offset + 16 <= bytes.len() {
            let new_w = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
            let new_h = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
            let image_id = u64::from_le_bytes(bytes[offset + 8..offset + 16].try_into().unwrap()) as i64;
            offset += 16;
            let len = new_w * new_h * 3;
            if offset + len > bytes.len() {
                break;
            }
            let want = &bytes[offset..offset + len];
            offset += len;

            let path = PathBuf::from(pathmap.get(image_id.to_string()).unwrap().as_str().unwrap());
            let rgb = image::open(&path).unwrap().to_rgb8();
            let (w, h) = (rgb.width() as usize, rgb.height() as usize);
            let scale = CLIP_CROP as f64 / w.min(h) as f64;
            let expect_w = round_half_even(w as f64 * scale).max(CLIP_CROP as u32) as usize;
            let expect_h = round_half_even(h as f64 * scale).max(CLIP_CROP as u32) as usize;
            assert_eq!(
                (expect_w, expect_h),
                (new_w, new_h),
                "图 {image_id} 的目标尺寸与 PIL 不一致"
            );
            let got = resample_rgb(rgb.as_raw(), w, h, new_w, new_h);
            for (a, b) in got.iter().zip(want.iter()) {
                total_px += 1;
                let delta = (*a as i32 - *b as i32).abs();
                if delta != 0 {
                    mismatched += 1;
                    if delta > max_delta {
                        max_delta = delta;
                    }
                }
            }
            images += 1;
        }
        assert!(images > 0, "参考文件里没有可比的图");
        let rate = mismatched as f64 / total_px as f64;
        eprintln!(
            "预处理闸门: {images} 张 · {total_px} 通道 · 不一致 {mismatched} ({:.4}%) · 最大差 {max_delta}",
            rate * 100.0
        );
        assert!(rate < 5e-4, "不一致率过高：{rate}");
        assert!(max_delta <= 1, "出现了超过 1 级的差异：{max_delta}");
    }

    /// 产出**产品自己的**语料向量（`out/vision_rust.json`）。
    ///
    /// 为什么要有这一步：研究阶段的指标（NN 一致率、松散对率）描述的是
    /// 「产品实际会用的向量」才有意义。产品与实验室的 ORT 构建不同 →
    /// 同一张图的两份向量余弦 0.98（不影响聚类结论，但严格说指标得跑在
    /// 产品自己的向量上）。这个测试用**产品代码路径**（同一个 engine、同一个
    /// 预处理、同一个批大小）跑全语料，把结果交给仪器。
    ///
    /// 默认跳过：设 `AI_IMAGE_SORTER_COMPUTE=1` 才跑（要几分钟）。
    #[test]
    fn compute_corpus_signatures() {
        if std::env::var("AI_IMAGE_SORTER_COMPUTE").is_err() {
            eprintln!("跳过：设 AI_IMAGE_SORTER_COMPUTE=1 才会跑全语料");
            return;
        }
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let model = manifest.join("resources").join("models").join(VISION_WEIGHTS);
        let lab = manifest.join("..").join("tools").join("l2lab").join("out");
        if !model.is_file() || !lab.join("pathmap.json").is_file() {
            eprintln!("跳过：缺模型或缺路径表");
            return;
        }
        let pathmap: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(lab.join("pathmap.json")).unwrap()).unwrap();
        let mut items: Vec<(i64, PathBuf)> = Vec::new();
        if let Some(map) = pathmap.as_object() {
            for (id, path) in map {
                if let Some(p) = path.as_str() {
                    let p = PathBuf::from(p);
                    if p.is_file() {
                        items.push((id.parse().unwrap_or(0), p));
                    }
                }
            }
        }
        items.sort_by_key(|(id, _)| *id);
        let engine = VisionEngine::new(model);
        let started = std::time::Instant::now();
        let (rows, failed) = engine
            .embed_batch(&items, &mut |done, total| {
                if done % 200 == 0 {
                    eprintln!("  {done}/{total}");
                }
            }, &|| false)
            .expect("视觉推理失败");
        eprintln!(
            "产品向量: {} 张（跳过 {failed}）· 用时 {:.1}s · {:.1} img/s",
            rows.len(),
            started.elapsed().as_secs_f64(),
            rows.len() as f64 / started.elapsed().as_secs_f64()
        );
        let payload = serde_json::json!({
            "model": VISION_MODEL_ID,
            "dim": rows.first().map(|r| r.1.len()).unwrap_or(0),
            "count": rows.len(),
            "items": rows
                .iter()
                .map(|(id, vec)| serde_json::json!({
                    "id": id,
                    "vec": vec.iter().map(|v| (v * 1e6).round() / 1e6).collect::<Vec<f32>>(),
                }))
                .collect::<Vec<_>>(),
        });
        let out_path = lab.join("vision_rust.json");
        std::fs::write(&out_path, payload.to_string()).unwrap();
        eprintln!("→ {}", out_path.display());
    }

    /// 把签名算进**真实库**（新增 `image_signatures` 表，不动任何现有数据）。
    ///
    /// 与 `compute_corpus_signatures` 的区别：那个写 JSON 给仪器用；这个走
    /// 产品自己的存储路径（`db.upsert_signatures`），让「可能相关」推荐与
    /// 语义聚类在真实库里可用。
    ///
    /// 默认跳过：设 `AI_IMAGE_SORTER_COMPUTE=1` 才跑（要几分钟）。
    #[test]
    fn compute_signatures_into_db() {
        if std::env::var("AI_IMAGE_SORTER_COMPUTE").is_err() {
            eprintln!("跳过：设 AI_IMAGE_SORTER_COMPUTE=1 才会跑");
            return;
        }
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let model = manifest.join("resources").join("models").join(VISION_WEIGHTS);
        if !model.is_file() {
            eprintln!("跳过：未安装视觉模型");
            return;
        }
        let app_data = std::env::var("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default())
                    .join("AppData")
                    .join("Roaming")
            });
        let db_path = app_data
            .join("com.aiimagesorter.app")
            .join("ai-image-sorter.db");
        if !db_path.is_file() {
            eprintln!("跳过：没有真实库 {}", db_path.display());
            return;
        }
        let db = crate::db::open(&db_path).expect("打开库失败");
        let source_id = db
            .list_sources()
            .expect("读源失败")
            .first()
            .map(|s| s.id)
            .unwrap_or(1);
        let images = db.list_images_for_signatures(source_id).expect("读图失败");
        let items: Vec<(i64, PathBuf)> = images
            .into_iter()
            .filter(|(_, path)| Path::new(path).is_file())
            .map(|(id, path)| (id, PathBuf::from(path)))
            .collect();
        eprintln!("待算签名：{} 张", items.len());
        let engine = VisionEngine::new(model);
        let started = std::time::Instant::now();
        let (rows, failed) = engine
            .embed_batch(&items, &mut |done, total| {
                if done % 200 == 0 {
                    eprintln!("  {done}/{total}");
                }
            }, &|| false)
            .expect("视觉推理失败");
        let written = db
            .upsert_signatures(
                "vision",
                VISION_MODEL_ID,
                &rows
                    .iter()
                    .map(|(id, vec)| (*id, vec.as_slice()))
                    .collect::<Vec<_>>(),
            )
            .expect("写签名失败");
        eprintln!(
            "写入 {written} 条（跳过 {failed}）· 用时 {:.1}s · 库 {}",
            started.elapsed().as_secs_f64(),
            db_path.display()
        );
    }

    /// 诊断：张量的字节级校验和（与 Python 侧同算法）。
    ///
    /// 不是闸门（两边有 4 个像素差 1，校验和必然不同），是**定位工具**：
    /// 校验和一致 = 预处理逐位相同；不一致时用 `tools/l2lab/diag_resize.py`
    /// 逐像素定位差异。
    #[test]
    fn diag_tensor_checksum() {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let lab = manifest.join("..").join("tools").join("l2lab").join("out");
        let pathmap_path = lab.join("pathmap.json");
        if !pathmap_path.is_file() {
            eprintln!("跳过：没有实验室路径表");
            return;
        }
        let pathmap: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&pathmap_path).unwrap()).unwrap();
        let reference: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(lab.join("vision.json")).unwrap()).unwrap();
        let id = reference.get("items").and_then(|v| v.as_array()).unwrap()[0]
            .get("id")
            .and_then(|v| v.as_i64())
            .unwrap();
        let path = PathBuf::from(pathmap.get(id.to_string()).unwrap().as_str().unwrap());
        let rgb = image::open(&path).unwrap().to_rgb8();
        let (w, h) = (rgb.width() as usize, rgb.height() as usize);
        let out = preprocess_clip(rgb.as_raw(), w, h).unwrap();
        // FNV-1a 64
        let mut hash: u64 = 0xcbf29ce484222325;
        for v in &out {
            for byte in v.to_le_bytes() {
                hash ^= byte as u64;
                hash = hash.wrapping_mul(0x100000001b3);
            }
        }
        eprintln!("Rust 张量校验和 FNV1a64 = {hash:016x}  (id={id}, {w}x{h})");
        // 把 resize 后的原始像素落盘，供 Python 侧逐元素比对
        let scale = CLIP_CROP as f64 / w.min(h) as f64;
        let new_w = round_half_even(w as f64 * scale).max(CLIP_CROP as u32) as usize;
        let new_h = round_half_even(h as f64 * scale).max(CLIP_CROP as u32) as usize;
        let scaled = resample_rgb(rgb.as_raw(), w, h, new_w, new_h);
        let out_path = lab.join("rust_resized.bin");
        let mut bytes = Vec::with_capacity(scaled.len() + 16);
        bytes.extend_from_slice(&(new_w as u32).to_le_bytes());
        bytes.extend_from_slice(&(new_h as u32).to_le_bytes());
        bytes.extend_from_slice(&scaled);
        std::fs::write(&out_path, bytes).unwrap();
        eprintln!("Rust resize 输出 → {}", out_path.display());
    }
}
