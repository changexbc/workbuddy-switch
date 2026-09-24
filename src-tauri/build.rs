fn main() {
    println!("cargo:rustc-env=AGENT_COMPANION_TARGET={}", std::env::var("TARGET").expect("Cargo TARGET"));
    tauri_build::build()
}
