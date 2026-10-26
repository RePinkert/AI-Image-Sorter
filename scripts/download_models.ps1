# download_models.ps1 — 取阶段六要随安装包分发的 ONNX 权重
#
# 口径（与 LAP 的 scripts/download_models.sh 同一套做法）：
#   · 权重**下载**，不进仓库（src-tauri/resources/models/ 已在 .gitignore）
#   · 记录期望字节数，下载后逐字节校验 —— 半个文件会让 ORT 在运行时报出
#     莫名其妙的错，比缺文件更难查
#   · 记录 sha256（与 HF 上公布的 LFS 指针一致），便于离线复现
#
# 用法（在仓库根目录）：
#   powershell -ExecutionPolicy Bypass -File scripts\download_models.ps1
# 构建前跑一次即可；产物会被 tauri.conf.json 的 bundle.resources 打进安装包。

$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$dest = Join-Path $here '..\src-tauri\resources\models'
New-Item -ItemType Directory -Force -Path $dest | Out-Null
$dest = (Resolve-Path $dest).Path

$base = 'https://huggingface.co'
$files = @(
  @{ url = "$base/Xenova/clip-vit-base-patch32/resolve/main/onnx/vision_model_quantized.onnx"; name = 'vision_clip.onnx'; size = 89117001; sha256 = '583fd1110a514667812fee7d684952aaf82a99b959760c8d7dca7e0ab9839299' },
  @{ url = "$base/Xenova/bge-small-zh-v1.5/resolve/main/onnx/model_quantized.onnx"; name = 'text_bge.onnx'; size = 24010842; sha256 = '15b717c382bcb518ba457b93ae6852edef4f1cd8937454aa06972366cd19bcc' },
  @{ url = "$base/Xenova/bge-small-zh-v1.5/resolve/main/tokenizer.json"; name = 'tokenizer_bge.json'; size = 439125; sha256 = '' }
)

foreach ($f in $files) {
  $path = Join-Path $dest $f.name
  if (Test-Path $path) {
    $len = (Get-Item $path).Length
    if ($len -eq $f.size) { Write-Output "skip $($f.name) (已存在, $len 字节)"; continue }
    Write-Output "重下 $($f.name)（大小不符: $len != $($f.size)）"
    Remove-Item $path
  }
  Write-Output "下载 $($f.name) ..."
  curl.exe -L -s -o $path $f.url
  $len = (Get-Item $path).Length
  if ($len -ne $f.size) {
    Write-Error "$($f.name) 大小不符: 得到 $len，期望 $($f.size)"
  }
  if ($f.sha256) {
    $hash = (Get-FileHash -Algorithm SHA256 $path).Hash.ToLower()
    if ($hash -ne $f.sha256) {
      Write-Error "$($f.name) sha256 不符: 得到 $hash，期望 $($f.sha256)"
    }
    Write-Output "  sha256 ok"
  }
  Write-Output "  ok ($len 字节)"
}
Write-Output "全部完成 → $dest"
