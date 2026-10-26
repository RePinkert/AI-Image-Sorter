fn main() {
  tauri_build::build();

  // ── 前端资源：变了必须重新编译 ──────────────────────────────────────────
  // 前端资源是在 `generate_context!` **宏展开时**（编译期）嵌进二进制的，
  // 不是 build script 嵌的。cargo 不知道 `../dist` 变了 → 只改前端时
  // crate 不会重新编译 → **exe 里留着上一版的前端**。
  //
  // 声明这条依赖后：dist 变了 → build script 重跑 → 下面的构建指纹变 →
  // crate 重新编译 → 宏重新展开 → 新前端进二进制。
  println!("cargo:rerun-if-changed=../dist");

  // ── 构建指纹 ────────────────────────────────────────────────────────────
  // 目的：让「这是哪一次构建」能从二进制里**直接读出来**，不必靠文件
  // 时间戳去猜（复制 / 解包 / 同步都会改时间戳）。
  //
  // 之所以不写死成常量：发行版 EXE 是用户主动审查的产物，每一档改动
  // 都要交一个可辨认的新 exe。指纹让「这个 exe 是不是刚那一份」不需要
  // 靠记忆。
  //
  // 口径：build.rs 只在包内文件或 `../dist` 变动时重跑（cargo 的默认指纹），
  // 所以「没改代码就重建」不会平白产生一个新指纹。
  let ts = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_secs())
    .unwrap_or(0);
  println!("cargo:rustc-env=AI_SORTER_BUILD_TS={ts}");

  let id = std::process::Command::new("git")
    .args(["rev-parse", "--short", "HEAD"])
    .output()
    .ok()
    .filter(|o| o.status.success())
    .and_then(|o| String::from_utf8(o.stdout).ok())
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
    .unwrap_or_else(|| "nogit".to_string());
  println!("cargo:rustc-env=AI_SORTER_BUILD_ID={id}");
}

