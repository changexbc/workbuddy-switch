fn main() {
    // 图标资源变更必须触发重新编译，否则 Windows 可执行文件不会重新嵌入图标
    // （tauri-build 默认不监听 icons/ 目录，改图标后不重建会一直沿用旧图标）。
    println!("cargo:rerun-if-changed=icons/icon.ico");
    println!("cargo:rerun-if-changed=icons/icon-windows.png");
    println!("cargo:rerun-if-changed=icons/32x32.png");
    println!("cargo:rerun-if-changed=icons/128x128.png");
    println!("cargo:rerun-if-changed=icons/128x128@2x.png");
    tauri_build::build()
}
