# AI Image Sorter

[English](README.md) | 简体中文

AI Image Sorter 是一款本地优先（local-first）的桌面应用，用于整理与审查 AI 生成图像。专为 ComfyUI 及兼容生成器产出的图像库设计，生成元数据与图像一同保存在本地 SQLite 数据库中。

## 功能特性

- 自动发现并导入 ComfyUI 输出目录，也支持手动选择本地文件夹
- 解析 PNG、JPEG、WebP 中的生成元数据（ComfyUI、A1111 及相关格式）
- 按文件夹、模型、提示词相似度与独立提示词对图像分组
- 匹配 ComfyUI 工作流拓扑并保留完整生成配方（recipe）
- 通过滑动评分（swipe）或两两对比（arena）审查图像
- 支持隐藏、打标签、归档、移动、合并、拆分与撤销等操作
- 对已注册目录进行后台增量重扫描
- 导出图像数据为 CSV/JSON，并可导出脱敏诊断信息
- 所有应用数据仅本地存储；粗粒度应用事件只写入本地数据库，任何数据都不会离开本机

## 当前状态

`v0.1.0` 为首个公开预览版。核心工作流可用，但对非典型元数据格式和超大图像库的兼容性仍在扩展。本版本主要支持平台为 Windows。

## 安装

从 [Releases](https://github.com/RePinkert/AI-Image-Sorter/releases) 页面下载 Windows 安装包。NSIS 安装器与 MSI 包均由 Tauri release 构建生成。

应用在建立索引时不会上传或修改源图像。除非显式请求归档、移动或送入回收站，图像文件始终保留在原目录。

### 便携使用

Release 构建产物是独立的 `.exe`，无需安装即可直接运行。所有数据（数据库、日志等）都可以跟随软件本体存放：

1. 将 `AI-Image-Sorter.exe` 放置在任意目录（如本地磁盘或 U 盘）。
2. 在 `.exe` 同级创建一个空文件 `ai-image-sorter.portable`。
3. 启动 `.exe` 即进入便携模式，应用数据统一写入 exe 旁的 `data\` 目录：
   - `data\ai-image-sorter.db` — SQLite 数据库
   - `data\logs\` — 运行日志
   - WebView2 配置（缓存、Cookie、浏览器存储）——运行时留在系统默认位置，**退出时自动清除**

每次正常退出时会自动清空宿主机 AppData 中的 WebView2 配置（缓存与浏览器状态），数据库与日志保留在软件同目录——这正是便携设计：数据与软件同进同退，宿主机不留任何本软件痕迹。卸载时只需删除 `.exe` 与 `data\`，系统 AppData 中不会有任何属于本软件的目录。（若程序崩溃导致清理未完成，留下的只是 WebView2 缓存，下次正常退出即自动清除。）

删除 `ai-image-sorter.portable` 文件即恢复默认安装模式：数据写入系统应用数据目录（`%APPDATA%\com.aiimagesorter.app`）。

## 使用方法

1. 启动应用。
2. 选择检测到的 ComfyUI 输出目录，或手动选择本地文件夹。
3. 等待首次扫描完成。应用运行期间会对已注册目录进行增量检查。
4. 选择分组层级，然后使用滑动评分或对比模式审查图像。

本地数据库存储于系统平台的应用数据目录（便携模式下为 exe 旁 `data\`），内含索引、评分、标签、生成配方与本地遥测事件，但**不会**复制图像文件。

## 开发

### 环境要求

- Node.js 20 或更新
- Rust 1.77.2 或更新
- 目标平台的 Tauri 2 前置依赖

### 常用命令

```bash
npm install
npm run dev          # 浏览器开发壳
npm run tauri:dev   # 桌面开发应用
npm run build
npm run lint
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml
```

部分集成测试依赖开发者本机的 ComfyUI 图像目录，无法在干净的检出中运行；便携单元测试不依赖该目录。

## 隐私

本应用为本地优先架构，且完全无网络请求。粗粒度的应用事件仅用于调试与产品改进，只记录在本地数据库中——应用没有任何上传通道。事件不会刻意包含提示词、图像像素、源路径或其他图像内容。可在设置中关闭记录，同一面板可导出脱敏诊断信息。

## 参与贡献

欢迎反馈 Bug 与提交 Pull Request。涉及本地数据或元数据解析的问题，请一并附上操作系统、应用版本、复现步骤与脱敏诊断导出。

## 许可证

AI Image Sorter 基于 [MIT License](LICENSE) 授权。
