fn main() {
    cc::Build::new()
        .file("native/menu.m")
        .flag("-fobjc-arc")
        .compile("seemenu");
    println!("cargo:rustc-link-lib=framework=AppKit");
    println!("cargo:rerun-if-changed=native/menu.m");
    tauri_build::build();
}
