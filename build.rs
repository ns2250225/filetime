fn main() {
    slint_build::compile("ui/app.slint").unwrap();
    #[cfg(windows)]
    winresource::WindowsResource::new()
        .set_icon("assets/filetime.ico")
        .compile()
        .expect("failed to embed FileTime icon");
    println!("cargo:rerun-if-changed=assets/filetime.ico");
    println!("cargo:rerun-if-changed=assets/filetime-icon.png");
    if std::env::var_os("CARGO_FEATURE_BUNDLED_SHELL").is_some() {
        let root = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        for (key, file) in [
            ("FILETIME_SHELL_DLL", "target/bundle/FileTimeShell.dll"),
            ("FILETIME_SHELL_MSIX", "target/bundle/FileTime.msix"),
            ("FILETIME_SHELL_CERT", "target/bundle/FileTime.cer"),
        ] {
            let path = std::path::Path::new(&root).join(file);
            assert!(path.exists(), "missing bundled asset: {}", path.display());
            println!("cargo:rustc-env={key}={}", path.display());
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}
