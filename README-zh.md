# AI Image Sorter

[English](README.md) | 简体中文

AI Image Sorter 是一款本地优先（local-first）的桌面应用，用于整理与审查 AI 生成图像。专为 ComfyUI 及兼容生成器产出的图像库设计，生成元数据与图像一同保存在本地 SQLite 数据库中。

## 功能特性

- 自动发现 ComfyUI 输出目录（常见路径与运行中的进程），也可手动导入任意本地文件夹；已注册目录在后台增量重扫描
- 解析 ComfyUI PNG 块、A1111 parameters（PNG/JPEG/WebP）及伴生 `.txt` 文件中的生成元数据
- 沿四个轴对图像分组：文件夹、模型偏差（鼠标滚轮切换维度）、提示词相似度（阈值可调）与独立提示词
- 提示词相似度这一轴有三种可切换的聚类手段——词面重合（Jaccard）、图像向量（CLIP 塔）、提示词语义（BGE 塔）——各带各的阈值：它们是不同的量，量纲不可互换
- 两座向量塔都在进程内跑（ONNX Runtime，不需要 Python、不依赖外部服务），直接处理你已有的图
- 移动分组时给出可能的目标组建议；组里没有视觉上足够接近的伙伴时，标出「疑似错分」
- 改名不丢分拣：每张图带内容指纹，改了名也能认出是同一张图，评分、标签与手动分组原样跟着走
- 从旧库找回评分、标签与手动分组：按内容身份认领旧库的行
- 签名只在你要求时才算：同步循环如实报出「还有多少张缺向量」，不再在后台偷偷跑
- 自述构建内容：可执行文件里带着能力标记串，发布探针直接扫二进制核对
- 匹配 ComfyUI 工作流拓扑，为每张图保留完整生成配方（recipe）
- 通过滑动评分或 Bradley–Terry 擂台对比审查图像；自定义标签、撤销，评审会话跨重启恢复
- 瀑布流网格浏览：视口虚拟化、八种排序、文件名搜索、仅隐藏过滤、批量选择/取消隐藏/拆分/移动/删除
- 真缩放查看器：适应/1:1/自由缩放、原生全屏、胶片条、元数据侧栏，并支持 PNG 提示词内联编辑回写
- 合并、拆分、移动与取消合并分组；手动分组在重新聚类后仍保留
- 隐藏、打标签、归档（复制）或送入系统回收站——日常整理从不修改源文件
- 可视化面板重新绑定全部键盘快捷键
- 导出图像数据为 CSV/JSON，并可导出脱敏诊断信息
- 所有应用数据仅本地存储；粗粒度应用事件只写入本地数据库，任何数据都不会离开本机

## 当前状态

`v0.5.0` 为最新发布版本（`v0.1.0` 为首个公开预览版）。核心工作流可用，但对非典型元数据格式和超大图像库的兼容性仍在扩展。主要支持平台为 Windows。

`v0.5.0` 带入了上面那三样：改名不丢分拣、库恢复、签名按需计算。`v0.4.0` 的两座向量塔与语义 L2 档位保持不变。默认档位仍是词面重合；没有模型权重时应用照常可用——见[模型权重](#模型权重视觉与语义档位)。

## 安装

从 [Releases](https://github.com/RePinkert/AI-Image-Sorter/releases) 页面下载便携版 `AI-Image-Sorter.exe`。发布包仅提供这一个免安装的独立 exe，不提供 NSIS/MSI 安装器。

应用在建立索引时不会上传或修改源图像。归档是把图片复制到目标文件夹，组间移动只改变分组归属，不触碰磁盘文件；只有送入回收站才会把文件移出原目录。

### 便携使用

Release 构建产物是独立的 `.exe`，无需安装即可直接运行。所有数据（数据库、日志等）都可以跟随软件本体存放：

1. 将 `AI-Image-Sorter.exe` 放置在任意目录（如本地磁盘或 U 盘）。
2. 在 `.exe` 同级创建一个空文件 `ai-image-sorter.portable`。
3. 启动 `.exe` 即进入便携模式，应用数据统一写入 exe 旁的 `data\` 目录：
   - `data\ai-image-sorter.db` — SQLite 数据库
   - `data\logs\` — 运行日志
   - WebView2 配置（缓存、Cookie、浏览器存储）——运行时留在系统默认位置，**退出时自动清除**

每次正常退出时会自动清空宿主机本地应用数据目录（`%LOCALAPPDATA%\com.aiimagesorter.app`）中的 WebView2 配置（缓存与浏览器状态），数据库与日志保留在软件同目录——这正是便携设计：数据与软件同进同退，宿主机不留任何本软件痕迹。卸载时只需删除 `.exe` 与 `data\`，系统 AppData 中不会有任何属于本软件的目录。（若程序崩溃导致清理未完成，留下的只是 WebView2 缓存，下次正常退出即自动清除。）

删除 `ai-image-sorter.portable` 文件即恢复默认安装模式：数据写入系统应用数据目录（`%APPDATA%\com.aiimagesorter.app`）。

### 模型权重视觉与语义档位

词面重合档位开箱即用。视觉与语义档位需要三个 ONNX 模型文件，它们有意不入库（合计约 113 MB）：

- `vision_clip.onnx` —— CLIP ViT-B/32，视觉档位用
- `text_bge.onnx` 与 `tokenizer_bge.json` —— BGE-small-zh，语义档位用

用仓库自带的脚本拉取（每个文件都校验大小与 sha256）：

```powershell
powershell -ExecutionPolicy Bypass -File scripts\download_models.ps1
```

脚本写入 `src-tauri\resources\models\`，Tauri 打包配置会把它收进安装包。便携版 `.exe` 请把同样这三个文件复制到 exe 旁的 `data\models\`，或用环境变量 `AI_IMAGE_SORTER_MODELS` 指向存放它们的目录。没有权重时应用照常启动：它会明确报告视觉与语义档位不可用，词面重合档位不受任何影响。

## 使用方法

1. 启动应用。
2. 选择检测到的 ComfyUI 输出目录，或手动选择本地文件夹。
3. 等待首次扫描完成。应用运行期间会对已注册目录进行增量检查。
4. 选择分组层级，然后使用滑动评分或对比模式审查图像。

本地数据库存储于系统平台的应用数据目录（便携模式下为 exe 旁 `data\`），内含索引、评分、标签、生成配方与本地遥测事件，但**不会**复制图像文件。

## 架构

桌面外壳为 Tauri 2：Rust 后端驱动 React 19 + TypeScript（Vite 构建）前端。

- **前端** —— 单个 Zustand store 加视图切换（无路由），纯 CSS 设计系统，大网格使用视口窗口虚拟化；图像经 Tauri asset 协议加载。
- **后端** —— Rust 通过约 40 个 Tauri command 按域分组：目录发现与增量同步、分组/聚类、评分评审、标签与分组操作、导出、遥测。所有图像元数据解析（ComfyUI PNG 块、A1111 parameters、JPEG/WebP EXIF、工作流拓扑匹配）也在此完成。
- **向量塔** —— 视觉塔与文本塔经 ONNX Runtime（`ort`）在进程内推理：图像走 CLIP，提示词走 BGE 配手写 WordPiece 分词器。不需要 Python，不拉起外部进程，运行期没有网络流量。
- **自述机制** —— 每项能力都带一条住在实现模块里的标记串，因此可以直接扫二进制证明这份构建**真的**包含哪些功能，而不必只信文档。
- **存储** —— 单个本地 SQLite 数据库（WAL 模式）保存索引、评分、标签、配方与遥测；除非显式归档，图像文件不会被复制。

## 开发

### 环境要求

- Node.js 20 或更新
- Rust 1.85 或更新（向量塔用的 ONNX Runtime 2.0.0-rc.10 要求此版本）
- 目标平台的 Tauri 2 前置依赖

### 常用命令

本仓库为 Tauri 2 混合工程：`npm` 负责 Web UI（前端），`cargo` 负责 Rust 后端。`npm run dev` 仅在浏览器中拉起 Web UI 开发服务（不含 Rust 后端）；完整桌面应用请使用 `npm run tauri:dev`。

```bash
npm install
npm run dev          # 浏览器开发壳
npm run tauri:dev   # 桌面开发应用
npm run build
npm run lint
npm run release:exe  # 发行版 exe（内嵌前端）
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml
```

`npm run release:exe` 是发行版构建：先编前端，再带 `--features custom-protocol` 编后端。这个开关**不能省**——不开它，Tauri 的 build script 会走开发分支，产出的 exe 根本不含前端。

发行版探针负责核对一份构建：扫二进制里的 `ais.cap.` 能力标记串，逐条与实现路径上的特征串对照，并在前端资源缺失时直接判失败。

```powershell
powershell -ExecutionPolicy Bypass -File scripts\verify-release-binary.ps1 -Report artifacts\probe.txt
```

历次发行版产物的指纹台账在 `artifacts/README.md`。

部分集成测试依赖开发者本机的 ComfyUI 图像目录，无法在干净的检出中运行；便携单元测试不依赖该目录。

## 隐私

本应用为本地优先架构，且完全无网络请求。粗粒度的应用事件仅用于调试与产品改进，只记录在本地数据库中——应用没有任何上传通道。事件不会刻意包含提示词、图像像素、源路径或其他图像内容。可在设置中关闭记录，同一面板可导出脱敏诊断信息。

向量塔也守这条线：推理在进程内完成，模型权重由单独的脚本拉取，应用自身不碰网络。提示词同样按私密内容对待——开发桥可以对其做掩码（等宽保形，度量仍可与生产对比），供任何需要外传的截图使用。

## 参与贡献

欢迎反馈 Bug 与提交 Pull Request。涉及本地数据或元数据解析的问题，请一并附上操作系统、应用版本、复现步骤与脱敏诊断导出。

## 许可证

AI Image Sorter 基于 [MIT License](LICENSE) 授权。
