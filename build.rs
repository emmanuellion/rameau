// Windows: embeds packaging/windows/rameau.rc (the app icon) into rameau.exe.
fn main() {
    println!("cargo:rerun-if-changed=packaging/windows/rameau.rc");
    println!("cargo:rerun-if-changed=packaging/windows/rameau.ico");
    #[cfg(windows)]
    embed_resource::compile("packaging/windows/rameau.rc", embed_resource::NONE).manifest_optional().unwrap();
}
