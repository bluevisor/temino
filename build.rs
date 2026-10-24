fn main() {
    println!("cargo:rerun-if-changed=src/haiku/shim.cpp");
    // The shim needs the Be API headers, so only a Haiku build compiles it;
    // `cargo check` elsewhere still type-checks the Rust side.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("haiku") {
        return;
    }
    cc::Build::new()
        .cpp(true)
        .file("src/haiku/shim.cpp")
        .flag("-std=c++17")
        .flag_if_supported("-Wno-unused-parameter")
        .compile("temino_shim");
    println!("cargo:rustc-link-lib=be");
}
