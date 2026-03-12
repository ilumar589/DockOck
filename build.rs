fn main() {
    // Re-run if the file changes (or appears/disappears).
    println!("cargo:rerun-if-changed=custom_providers.json");

    let src = std::path::Path::new("custom_providers.json");
    if !src.exists() {
        return;
    }

    // OUT_DIR is  target/<profile>/build/<crate>-<hash>/out
    // Walk up 3 levels to reach target/<profile>/
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let profile_dir = std::path::Path::new(&out_dir)
        .ancestors()
        .nth(3)
        .unwrap()
        .to_path_buf();

    std::fs::copy(src, profile_dir.join("custom_providers.json")).unwrap();
}
