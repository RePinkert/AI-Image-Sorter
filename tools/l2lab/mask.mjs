// 去敏化掩码（阶段六护栏 P1 的唯一掩码实现）
//
// 口径（三条同时成立才算合格）：
//  1. 内容不可还原：所有承载语义的字符（CJK / 拉丁字母 / 数字）都被替换，
//     替换物由固定映射决定，不保留任何原文信息。
//  2. 长度不变：字符数与原文一致，便于统计"这条 prompt 有多长"这类结构量。
//  3. 字宽不变：占位符与被替换字符的 advance 相同 —— 汉字与〇都是 1.0em，
//     拉丁字母与 x 都是 0.5em（已在字体的 hmtx 上实测）。因此掩码后的
//     DOM 版面度量与原文一致，截图通道的溢出基线仍然可比。
//
// 保留标点与空白：标点不承载语义，却承载结构（分段、括号、权重标记），
// 且 16 字泄露探针要求片段内至少 8 个内容字符，纯标点片段不会被计入。

export const MASK_CJK = '\u3007' // 〇 U+3007
export const MASK_ASCII = 'x' // x U+0061

/** 是否承载语义（泄露探针的"内容字符"判定）。 */
export function isContentChar(ch) {
  const code = ch.codePointAt(0) ?? 0
  if (code >= 0x4e00 && code <= 0x9fff) return true // CJK 统一表意文字
  if (code >= 0x3400 && code <= 0x4dbf) return true // CJK 扩展 A
  if (code >= 0x3040 && code <= 0x30ff) return true // 假名
  if (code >= 0xac00 && code <= 0xd7af) return true // 谚文
  return (code >= 0x30 && code <= 0x39) || (code >= 0x41 && code <= 0x5a) || (code >= 0x61 && code <= 0x7a)
}

/** 是否是汉字（探针的"高区分度字符"判定：汉字长片段几乎不会在别处重复）。 */
export function isCjkChar(ch) {
  const code = ch.codePointAt(0) ?? 0
  return (code >= 0x4e00 && code <= 0x9fff) || (code >= 0x3400 && code <= 0x4dbf)
}

/** 不可逆掩码：内容字符 → 等宽占位符，标点/空白原样保留。 */
export function maskText(text) {
  let out = ''
  for (const ch of String(text ?? '')) {
    if (isContentChar(ch)) {
      const code = ch.codePointAt(0) ?? 0
      out += code > 0x2fff ? MASK_CJK : MASK_ASCII
    } else {
      out += ch
    }
  }
  return out
}

/** 掩码后的长度标注：截图与日志里用它交代"这里原本有多长"，但不交代内容。 */
export function maskLabel(text) {
  const raw = String(text ?? '')
  return `【已脱敏 prompt · ${raw.length} 字】`
}
