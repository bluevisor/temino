fn main() {
    println!("cargo:rerun-if-changed=src/haiku/shim.cpp");
    println!("cargo:rerun-if-changed=src/macos/shim.m");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new().file("src/macos/shim.m").flag("-fobjc-arc").compile("temino_macos");
        for framework in ["AppKit", "Carbon", "Metal", "CoreText", "WebKit"] {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
        // Run outside the bundle, AppKit takes the name (and the camera and
        // microphone reasons) from this copy of Info.plist.
        println!("cargo:rerun-if-changed=macos/Info.plist");
        let plist = format!("{}/macos/Info.plist", std::env::var("CARGO_MANIFEST_DIR").unwrap());
        println!("cargo:rustc-link-arg-bins=-Wl,-sectcreate,__TEXT,__info_plist,{plist}");
        return;
    }
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
