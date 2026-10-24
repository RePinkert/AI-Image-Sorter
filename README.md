# AI Image Sorter

English | [简体中文](README-zh.md)

AI Image Sorter is a local-first desktop application for organizing and reviewing AI-generated images. It is built for image libraries produced by ComfyUI and compatible generators, with generation metadata kept alongside the images in a local SQLite database.

## Features

- Discover ComfyUI output folders automatically (common paths and running processes), or import any local folder; registered folders are rescanned incrementally in the background
- Parse generation metadata from ComfyUI PNG chunks, A1111 parameters (PNG/JPEG/WebP), and companion `.txt` files
- Group images along four axes: folder, model deviation (mouse wheel switches the dimension), prompt similarity (adjustable threshold), and independent prompt
- Match ComfyUI workflow topology and retain the full generation recipe for each image
- Review with swipe scoring or Bradley–Terry arena comparisons; custom labels, undo, and a review session that survives a relaunch
- Browse a virtualized masonry grid with eight sort keys, filename search, hidden-only filter, and batch select/unhide/split/move/delete
- Inspect images in a true-zoom viewer: fit / 1:1 / free zoom, native fullscreen, filmstrip, metadata sidebar, and inline PNG prompt editing with write-back
- Merge, split, move, and unmerge groups; manual groups are preserved across re-clustering
- Hide, label, archive (copy), or send images to the OS recycle bin — normal organization never modifies source files
- Rebind every keyboard shortcut through a visual rebinding panel
- Export image data as CSV/JSON and export redacted diagnostics
- Store application data locally; coarse app events stay in the local database only — no data ever leaves the machine

## Status

`v0.3.0` is the latest release (`v0.1.0` was the first public preview). The core workflow is usable, but compatibility with unusual metadata formats and large libraries is still being expanded. Windows is the primary supported platform.

## Installation

Download the portable `AI-Image-Sorter.exe` from the [Releases](https://github.com/RePinkert/AI-Image-Sorter/releases) page. The release ships only this standalone `.exe` — no NSIS/MSI installer is provided.

The application does not upload or modify source images during indexing. Archiving copies images to a chosen folder, moving images between groups never touches files on disk, and only a recycle-bin action removes a file from its original folder.

### Portable use

The build in `Release` is a standalone `.exe` — you can run it directly, no installation required. Everything (database and logs) can be kept in the same place as the software:

1. Place `AI-Image-Sorter.exe` anywhere on the machine (such as a local disk partition or a USB flash drive).
2. Create an empty file named `ai-image-sorter.portable` next to the `.exe` file.
3. Launch the `.exe`; portable mode routes all app data through the `data\` directory next to the `.exe`:
   - `data\ai-image-sorter.db` — SQLite database
   - `data\logs\` — run logs
   - WebView2 profile — kept at the OS default location while running, **wiped automatically on exit**

Each normal exit erases the WebView2 profile (cache, cookies, browser state) from the host's local app data (`%LOCALAPPDATA%\com.aiimagesorter.app`), while the database and logs stay on the drive beside the application — exactly the portable design: data travels with the software, and the host keeps no traces of it. To uninstall, just delete the `.exe` and `data\`; no directory belonging to this software remains in system AppData. (If a crash preempts the cleanup, only the WebView2 cache survives on the host; the next normal exit removes it.)

Delete the `ai-image-sorter.portable` file to revert to the default application mode: data is written to the system application data directory (`%APPDATA%\com.aiimagesorter.app`).

## Usage

1. Launch the application.
2. Choose a detected ComfyUI output folder or select a local folder.
3. Let the initial scan finish. Registered folders are checked incrementally while the application is open.
4. Select a grouping level, then review images using swipe or arena mode.

The local database is stored in the platform-specific Tauri application data directory (in portable mode: `data\` beside the `.exe`). It contains indexes, scores, labels, recipes, and local telemetry events, but not copied image files.

## Architecture

The desktop shell is Tauri 2: a Rust backend drives a React 19 + TypeScript frontend built with Vite.

- **Frontend** — a single Zustand store with a view switcher (no router), plain-CSS design system, and viewport-window virtualization for large grids. Images are shown through Tauri's asset protocol.
- **Backend** — Rust exposes ~40 Tauri commands grouped by domain: folder discovery and incremental sync, grouping/clustering, review and scoring, labels and group operations, export, and telemetry. All image metadata parsing (ComfyUI PNG chunks, A1111 parameters, JPEG/WebP EXIF, workflow topology matching) happens here as well.
- **Storage** — one local SQLite database (WAL mode) holds indexes, scores, labels, recipes, and telemetry; image files are never copied unless you explicitly archive them.

## Development

### Requirements

- Node.js 20 or newer
- Rust 1.77.2 or newer
- Tauri 2 prerequisites for the target platform

### Commands

```bash
npm install
npm run dev          # Browser development shell
npm run tauri:dev   # Desktop development application
npm run build
npm run lint
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml
```

Some integration tests use a developer-local ComfyUI image directory and are not portable to a clean checkout. The portable unit tests do not require that directory.

## Privacy

The application is local-first. Coarse application events are recorded for debugging and product improvement, and they stay in the local database — the app has no networking at all. Events do not intentionally include prompts, image pixels, source paths, or other image contents. Recording can be disabled in Settings, and redacted diagnostics can be exported from the same panel.

## Contributing

Bug reports and pull requests are welcome. Please include the operating system, application version, reproduction steps, and a redacted diagnostics export when reporting a problem involving local data or metadata parsing.

## License

AI Image Sorter is licensed under the [MIT License](LICENSE).
