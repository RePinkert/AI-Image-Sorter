<#
.SYNOPSIS
    发行版二进制探针 —— 「这个 exe 里到底有没有那套新能力」的可复跑断言。

.DESCRIPTION
    为什么要这个脚本

    发行版 EXE 是**用户主动审查的产物**。用户要能回答「这一份 exe 带没带
    新的视觉辅助聚类手段」，而这个问题**不能靠看文件大小 / 时间戳回答**：

    阶段七轮 2 的教训 —— 整座文本塔是死代码（`text.rs` 挂着
    `#![allow(dead_code)]`，`TextEngine::embed_prompts` 无调用方），再叠加
    `opt-level="s" + lto=true + strip=true`，链接器把整座塔剔除了。结果
    轮 2 的 exe 与轮 1 **逐字节等价**，功能上什么都没变，而这件事**肉眼
    完全看不出来**（连大小都一样：24,618,496）。

    所以探针做两件事：

    1. **静态**：扫出二进制里的全部能力标记串（`ais.cap.` 前缀），并逐条
       核对「标记串 + 实现路径上的特征串」是否同时在场。标记串由**实现
       模块**生成，所以「文档里声称有、代码里没有」会被立刻发现。
    2. **对比**：给了 -Baseline 就报出相对上一版的差异（大小 / 指纹 / 标记
       集合增减），把「变没变」变成一行断言。

    运行时那一半（应用自己报告的能力清单）在设置页「本构建能力」区块。

.PARAMETER Exe
    待探的 exe。默认 src-tauri\target\release\AI-Image-Sorter.exe

.PARAMETER Baseline
    对照的上一版 exe。给了就打印差异（大小 / 指纹 / 标记集合）。

.PARAMETER Report
    报告文件（UTF-8）。给了就落一份 —— 控制台编码各机不同（GBK / UTF-8），
    报告会认编码，文件不会。

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts\verify-release-binary.ps1

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts\verify-release-binary.ps1 `
        -Baseline artifacts\AI-Image-Sorter.prev.exe -Report artifacts\probe.txt
#>
[CmdletBinding()]
param(
    [string]$Exe = '',
    [string]$Baseline = '',
    [string]$Report = ''
)

$ErrorActionPreference = 'Stop'

# 脚本所在目录（param 块里 $PSScriptRoot 还没初始化，所以放到这里算）。
$ScriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
if (-not $Exe) {
    $Exe = Join-Path (Split-Path -Parent $ScriptRoot) 'src-tauri\target\release\AI-Image-Sorter.exe'
}

# ── 能力登记表 ────────────────────────────────────────────────────────────
# 每个条目：标记串 + 一条**只有该实现路径上才有**的特征串。
# 两者同时在场才算这项能力就位 —— 只在文档里写一句、或只留一个空标记串，
# 都会被这里抓出来。
#
# 标记串住在实现模块里（Rust 侧 `capabilities.rs` 收集），所以「模块被
# LTO 当死代码剔掉」时标记串会一起消失 —— 这正是要抓的情况（阶段七轮 2）。
#
# 维护口径：新增能力时同步登记；漏登不会被这里报错，但会让「能力清单」
# 与「登记表」漂移，所以登记表的条目数应当等于设置页「本构建能力」显示
# 的条数。
$Registry = @(
    @{
        Id      = 'l2.text-jaccard'
        Marker  = 'ais.cap.l2.text-jaccard.v1'
        Literal = 'ais.cap.l2.text-jaccard.v1'
        Note    = 'L2 词 Jaccard 单链（历史行为）'
    },
    @{
        Id      = 'l2.visual-avg-linkage'
        Marker  = 'ais.cap.l2.visual-avg-linkage.v1'
        Literal = 'ais.cap.l2.visual-avg-linkage.v1'
        Note    = 'L2 视觉塔平均连接'
    },
    @{
        Id      = 'tower.vision'
        Marker  = 'ais.cap.tower.vision.v1'
        Literal = '加载视觉模型失败'
        Note    = '视觉塔（CLIP ViT-B/32）推理路径'
    },
    @{
        Id      = 'tower.text'
        Marker  = 'ais.cap.tower.text.v1'
        Literal = '加载文本模型失败'
        Note    = '文本塔（BGE-small-zh）推理路径（阶段七轮 1 起接进命令层）'
    },
    @{
        Id      = 'provenance.build-info'
        Marker  = 'ais.cap.provenance.build-info.v1'
        Literal = '构建能力自述'
        Note    = '构建能力自述（后端能力清单 + 设置页面板）'
    },
    @{
        Id      = 'rename.preserve'
        Marker  = 'ais.cap.rename.preserve.v1'
        Literal = '内容键（尺寸+种子+prompt）'
        Note    = '改名不丢分拣（内容身份认领 + 就地改路径）'
    },
    @{
        Id      = 'library.recover'
        Marker  = 'ais.cap.library.recover.v1'
        Literal = '从旧库找回分拣结果'
        Note    = '库恢复（按内容身份从旧库找回评分/标签/手动分组）'
    },
    @{
        Id      = 'signature.explicit'
        Marker  = 'ais.cap.signature.explicit.v1'
        Literal = '签名按需计算（无后台补算）'
        Note    = '签名只按需计算（设置页按钮），同步循环不再偷跑'
        # **否定检查**：这段 SQL 只存在于「后台补算签名」的取数函数里
        # （`Db::images_without_signatures`）。第 30 轮把它连同后台补算
        # 整段删掉了 —— 它还在二进制里 = 后台补算还在。
        # 「没有」也得能证明：删掉一段代码同样看不出来。
        Absent  = 'SELECT i.id, i.abs_path FROM images i'
    }
)

# 报告行（同时进控制台与报告文件）。
$Lines = New-Object System.Collections.Generic.List[string]
function Say {
    param([string]$Text = '', [string]$Color = '')
    [void]$Lines.Add($Text)
    if ($Color) { Write-Host $Text -ForegroundColor $Color } else { Write-Host $Text }
}

function Get-Markers {
    param([byte[]]$Bytes)
    $needle = [System.Text.Encoding]::ASCII.GetBytes('ais.cap.')
    $found = New-Object System.Collections.Generic.HashSet[string]
    for ($i = 0; $i -le $Bytes.Length - $needle.Length; $i++) {
        $hit = $true
        for ($j = 0; $j -lt $needle.Length; $j++) {
            if ($Bytes[$i + $j] -ne $needle[$j]) { $hit = $false; break }
        }
        if (-not $hit) { continue }
        # 标记串以 NUL 结尾（C 风格哨兵）—— 读到 NUL 为止，提取没有歧义。
        # Rust 字面量在 .rodata 里不带终止符，不设哨兵的话会把下一条字面量
        # 一起吞进来（实测踩过：`...v1` 后面紧跟着 `L2`，被读成了 `...v1L2`）。
        $end = $i + $needle.Length
        while ($end -lt $Bytes.Length -and $Bytes[$end] -ne 0) { $end++ }
        $text = [System.Text.Encoding]::ASCII.GetString($Bytes, $i, $end - $i)
        [void]$found.Add($text)
        $i = $end
    }
    return $found
}

function Test-HasBytes {
    param([byte[]]$Bytes, [string]$Needle)
    $n = [System.Text.Encoding]::UTF8.GetBytes($Needle)
    if ($n.Length -eq 0) { return $true }
    for ($i = 0; $i -le $Bytes.Length - $n.Length; $i++) {
        $hit = $true
        for ($j = 0; $j -lt $n.Length; $j++) {
            if ($Bytes[$i + $j] -ne $n[$j]) { $hit = $false; break }
        }
        if ($hit) { return $true }
    }
    return $false
}

function Show-File {
    param([string]$Path, [string]$Label)
    if (-not (Test-Path -LiteralPath $Path)) {
        Say "[$Label] 不存在: $Path" 'Red'
        return $null
    }
    $item = Get-Item -LiteralPath $Path
    $hash = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
    Say "[$Label] $($item.FullName)"
    Say ("         大小   {0} 字节" -f $item.Length)
    Say ("         SHA256 {0}" -f $hash)
    Say ("         修改   {0}" -f $item.LastWriteTime.ToString('yyyy-MM-dd HH:mm:ss'))
    return [pscustomobject]@{
        Path  = $item.FullName
        Size  = $item.Length
        Hash  = $hash
        Bytes = [System.IO.File]::ReadAllBytes($item.FullName)
    }
}

Say '══ 发行版二进制探针 ══' 'Cyan'

$cur = Show-File -Path $Exe -Label '本档'
if (-not $cur) { exit 2 }

$markers = Get-Markers -Bytes $cur.Bytes
Say ''
Say ('能力标记串（扫到 {0} 条）' -f $markers.Count)
foreach ($m in ($markers | Sort-Object)) { Say "  · $m" }

Say ''
Say '能力核对（标记串 + 实现特征串，两者都要在场）'
$fail = 0
foreach ($r in $Registry) {
    $hasMarker = $markers.Contains($r.Marker)
    $hasLiteral = Test-HasBytes -Bytes $cur.Bytes -Needle $r.Literal
    if ($hasMarker -and $hasLiteral) {
        Say ("  [OK]   {0,-24} {1}" -f $r.Id, $r.Note) 'Green'
    } else {
        $fail++
        $why = if (-not $hasMarker) { '缺标记串' } else { '缺实现特征串' }
        Say ("  [FAIL] {0,-24} {1}（{2}）" -f $r.Id, $r.Note, $why) 'Red'
    }
    # 否定检查：某些能力是用「**没有**某段代码」来定义的。删掉一段代码
    # 在二进制里同样看不出来（`opt-level="s" + lto=true` 会剔得干干净净），
    # 所以「没有」必须单独证明 —— 否则这条能力是**不可证**的。
    if ($r.Absent) {
        $stillThere = Test-HasBytes -Bytes $cur.Bytes -Needle $r.Absent
        if ($stillThere) {
            $fail++
            Say ("  [FAIL] {0,-24} {1}（旧机制还在二进制里）" -f $r.Id, $r.Note) 'Red'
        } else {
            Say ("  [OK]   {0,-24} 旧机制已不在二进制里" -f "$($r.Id).absent") 'Green'
        }
    }
}

# 交付前置条件：**前端必须真的在 exe 里**。
#
# tauri 的 build.rs 里 `cfg(dev) = !custom-protocol`。不开这个 feature 时
# `cargo build --release` 产出的是**不含前端**的 exe —— 它指向本机开发
# 服务器（devUrl），前端资源压根没进二进制。
#
# 后果很严重：用户审查的正是这个文件，而「exe 变没变」这句话只覆盖了
# Rust 那一半，前端改动完全不在里面。实测过：档 0 交付的 exe 里搜不到
# 任何 `assets/index-*.js` 资源键。
#
# 资源内容是 brotli 压缩的，搜不到内容；但**资源键名**在二进制里，所以
# 搜 `assets/index-` 就够。
if (Test-HasBytes -Bytes $cur.Bytes -Needle 'assets/index-') {
    Say ("  [OK]   {0,-24} 前端资源已嵌入二进制" -f 'frontend-embedded') 'Green'
} else {
    $fail++
    Say ("  [FAIL] {0,-24} 前端资源**不在**二进制里（这个 exe 指向开发服务器，不是发行版）" -f 'frontend-embedded') 'Red'
}

$unknown = @($markers | Where-Object { $m = $_; -not ($Registry | Where-Object { $_.Marker -eq $m }) })
if ($unknown.Count -gt 0) {
    Say ''
    Say '未登记的标记串（登记表需要同步更新）' 'Yellow'
    foreach ($m in ($unknown | Sort-Object)) { Say "  ? $m" 'Yellow' }
}

if ($Baseline) {
    Say ''
    $base = Show-File -Path $Baseline -Label '基线'
    if ($base) {
        $baseMarkers = Get-Markers -Bytes $base.Bytes
        $added = @($markers | Where-Object { -not $baseMarkers.Contains($_) })
        $removed = @($baseMarkers | Where-Object { -not $markers.Contains($_) })
        Say ''
        Say '相对基线的差异'
        if ($cur.Hash -eq $base.Hash) {
            Say '  指纹    未变（这一版与基线逐字节相同）' 'Yellow'
        } else {
            Say ('  指纹    已变（大小 {0} → {1}，差 {2:+#;-#;0} 字节）' -f $base.Size, $cur.Size, ($cur.Size - $base.Size)) 'Green'
        }
        if ($added.Count -gt 0) {
            Say '  新增能力'
            foreach ($m in ($added | Sort-Object)) { Say "    + $m" 'Green' }
        }
        if ($removed.Count -gt 0) {
            Say '  移除能力'
            foreach ($m in ($removed | Sort-Object)) { Say "    - $m" 'Red' }
        }
        if ($added.Count -eq 0 -and $removed.Count -eq 0) {
            Say '  能力集合未变' 'Yellow'
        }
    }
}

Say ''
if ($fail -gt 0) {
    Say ("结论：{0} 项能力未就位" -f $fail) 'Red'
} else {
    Say '结论：登记的能力全部就位' 'Green'
}

if ($Report) {
    $dir = Split-Path -Parent $Report
    if ($dir -and -not (Test-Path -LiteralPath $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
    # 报告文件固定写 UTF-8（带 BOM，记事本 / VS Code 都能直接认）。
    [System.IO.File]::WriteAllText($Report, ($Lines -join "`r`n"), (New-Object System.Text.UTF8Encoding($true)))
    Write-Host ''
    Write-Host "报告已写入 $Report"
}

if ($fail -gt 0) { exit 1 }
exit 0
