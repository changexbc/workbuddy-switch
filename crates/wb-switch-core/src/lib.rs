//! wb-switch 核心逻辑库（纯 Rust，不依赖 Tauri）。
//!
//! 可被三种宿主复用：
//! - 桌面 App（`src-tauri`）：通过 Tauri commands 调用
//! - HTTP server（`wb-switch-server`）：axum 路由直接调用
//! - CLI：终端命令直接调用

pub mod modules;
