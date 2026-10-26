// mask.mjs 的类型声明（Node 侧实现，TS 侧引用）。
// 声明与实现同源：改动 mask.mjs 时同步这里，避免"类型说一套、代码做另一套"。
export declare const MASK_CJK: string
export declare const MASK_ASCII: string
export declare function isContentChar(ch: string): boolean
export declare function isCjkChar(ch: string): boolean
export declare function maskText(text: string | null | undefined): string
export declare function maskLabel(text: string | null | undefined): string
