# 第三方字体许可声明 / Third-Party Font Licenses

本文件依据各字体的 SIL Open Font License 1.1 条款 2 要求而设 —— OFL 规定
「每份副本必须包含上述版权声明与本许可证」，且字体可与任何软件一同打包分发。

---

## Sarasa Gothic（更纱黑体 / 等距更纱黑体 SC）

本项目打包了 **Sarasa Mono SC** 的子集（`src/assets/fonts/sarasa-mono-sc-{400,600,700}.woff2`）。

```
Copyright (c) 2015-2025, Renzhi Li (aka. Belleve Invis, belleve@typeof.net).
Portions Copyright (c) 2016 The Inter Project Authors.
Portions Copyright (c) 2014-2021 Adobe Systems Incorporated (http://www.adobe.com/),
    with Reserved Font Name 'Source'.
Portions Copyright (c) 2012 Google Inc.

This Font Software is licensed under the SIL Open Font License, Version 1.1.
```

- 设计者：Belleve Invis（Renzhi Li，微软工程师兼字体设计师）
- 上游：https://github.com/be5invis/Sarasa-Gothic
- 采用版本：v1.0.42，`SarasaMonoSC-TTF` 官方发布包

### 本项目所做的子集化修改

使用 `fontTools.subset` 生成子集，**这是一份 Modified Version**：

- **移除**：韩文（Hangul，11,172 字形）、Nerd Font 私用区、emoji 区、数学/图形符号扩展区
  —— 本应用为中文 AI 图片分拣工具，上述字形零使用价值
- **移除**：TrueType hinting 指令（`--no-hinting`），节省体积且视觉差异不可察
- **保留**：Latin、通用标点、货币符号、箭头、数学运算符、制表符、几何图形、
  Misc Symbols、**Dingbats（真实 prompt 中出现过 ✓ ✕）**、CJK 标点、假名、
  **CJK 统一表意文字全集 U+4E00–U+9FFF（20,992 字形，保证任意中文 prompt 不缺字）**、
  全角半角形式
- **保留**：name 表 ID 1/2/3/4/5/6
- **输出格式**：WOFF2

子集化保持了排版度量：中英文字宽比仍为精确 **2:1**（拉丁 0.5em / 汉字 1.0em），
字形轮廓未作任何修改。

> 依 OFL 条款 3，Modified Version 不得使用声明的保留字体名。唯一的保留字体名为
> `'Source'`（源自 Adobe 的思源黑体部分），本项目未在任何字体名中使用该名称。
> 字体族名 `Sarasa Mono SC` 与上游一致。

本项目对子集字体的再分发同样遵循 OFL 1.1：该字体以 OFL-1.1 提供，未以其他许可分发。
应用主项目自身采用 MIT 许可 —— OFL 只约束字体本身，不约束使用它的软件或文档。

---

## SIL Open Font License, Version 1.1

```
SIL OPEN FONT LICENSE Version 1.1 - 26 February 2007
```

### Preamble

The goals of the Open Font License (OFL) are to stimulate worldwide development of
collaborative font projects, to support the font creation efforts of academic and
linguistic communities, and to provide a free and open framework in which fonts may
be shared and improved in partnership with others.

The OFL allows the licensed fonts to be used, studied, modified and redistributed
freely as long as they are not sold by themselves. The fonts, including any
derivative works, can be bundled, embedded, redistributed and/or sold with any
software provided that any reserved names are not used by derivative works. The
fonts and derivatives, however, cannot be released under any other type of license.
The requirement for fonts to remain under this license does not apply to any
document created using the fonts or their derivatives.

### Definitions

`"Font Software"` refers to the set of files released by the Copyright Holder(s)
under this license and clearly marked as such. This may include source files, build
scripts and documentation.

`"Reserved Font Name"` refers to any names specified as such after the copyright
statement(s).

`"Original Version"` refers to the collection of Font Software components as
distributed by the Copyright Holder(s).

`"Modified Version"` refers to any derivative made by adding to, deleting, or
substituting -- in part or in whole -- any of the components of the Original
Version, by changing formats or by porting the Font Software to a new environment.

`"Author"` refers to any designer, engineer, programmer, technical writer or any
other person who contributed to the Font Software.

### Permission & Conditions

Permission is hereby granted, free of charge, to any person obtaining a copy of the
Font Software, to use, study, copy, merge, embed, modify, redistribute, and sell
modified and unmodified copies of the Font Software, subject to the following
conditions:

1. Neither the Font Software nor any of its individual components, in Original or
   Modified Versions, may be sold by itself.

2. Original or Modified Versions of the Font Software may be bundled, redistributed
   and/or sold with any software, provided that each copy contains the above
   copyright notice and this license. These can be included either as stand-alone
   text files, human-readable headers or in the appropriate machine-readable
   metadata fields within text or binary files as long as those fields can be
   easily viewed by the user.

3. No Modified Version of the Font Software may use the Reserved Font Name(s)
   unless explicit written permission is granted by the corresponding Copyright
   Holder. This restriction only applies to the primary font name as presented to
   the users.

4. The name(s) of the Copyright Holder(s) or the Author(s) of the Font Software
   shall not be used to promote, endorse or advertise any Modified Version, except
   to acknowledge the contribution(s) of the Copyright Holder(s) and the Author(s)
   or with their explicit written permission.

5. The Font Software, modified or unmodified, in part or in whole, must be
   distributed entirely under this license, and must not be distributed under any
   other license. The requirement for fonts to remain under this license does not
   apply to any document created using the Font Software.

### Termination

This license becomes null and void if any of the above conditions are not met.

### DISCLAIMER

THE FONT SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO ANY WARRANTIES OF MERCHANTABILITY, FITNESS
FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT OF COPYRIGHT, PATENT, TRADEMARK, OR
OTHER RIGHT. IN NO EVENT SHALL THE COPYRIGHT HOLDER BE LIABLE FOR ANY CLAIM,
DAMAGES OR OTHER LIABILITY, INCLUDING ANY GENERAL, SPECIAL, INDIRECT, INCIDENTAL,
OR CONSEQUENTIAL DAMAGES, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE,
ARISING FROM, OUT OF THE USE OR INABILITY TO USE THE FONT SOFTWARE OR FROM OTHER
DEALINGS IN THE FONT SOFTWARE.

---

## 更新字体时

若升级 Sarasa 版本，务必同步更新本文件顶部的版权年份与版本号，并在
`THIRD_PARTY_NOTICES.md` 中记录新的子集化范围。OFL 要求分发时附带完整许可证，
不得省略。