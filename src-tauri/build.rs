fn main() {
    // Embed Info.plist directly into the executable on macOS.
    //
    // `tauri dev` (and a bare `cargo run`) produce a plain executable with no
    // .app bundle, so the bundle's Info.plist — and its mic/camera usage
    // strings — never apply, and macOS silently denies audio/video capture.
    // Linking the plist into the binary's __TEXT,__info_plist section makes the
    // usage strings present even for the unbundled dev binary, so calls work in
    // `npm run dev` using the same WebKit data store (i.e. the same paired
    // session) as before — no need to switch to the bundled app.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let plist = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Info.plist");
        println!("cargo:rerun-if-changed=Info.plist");
        println!(
            "cargo:rustc-link-arg-bins=-Wl,-sectcreate,__TEXT,__info_plist,{}",
            plist.display()
        );
    }

    tauri_build::build()
}
