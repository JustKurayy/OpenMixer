fn main() {
    println!("cargo:rerun-if-changed=assets/openmixer.ico");
    #[cfg(target_os = "windows")]
    winresource::WindowsResource::new()
        .set_icon("assets/openmixer.ico")
        .set("ProductName", "OpenMixer")
        .set("FileDescription", "OpenMixer line-input audio mixer")
        .compile()
        .expect("failed to embed Windows resources");
}
