fn main() {
    println!("cargo:rerun-if-changed=assets/orbit.rc");
    println!("cargo:rerun-if-changed=assets/orbit.ico");
    println!("cargo:rerun-if-changed=assets/orbit.manifest");
    embed_resource::compile("assets/orbit.rc", embed_resource::NONE);
}
