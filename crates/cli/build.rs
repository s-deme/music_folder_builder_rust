fn main() {
    println!("cargo:rerun-if-changed=windows-resource.rc");
    println!("cargo:rerun-if-changed=windows-app-manifest.xml");

    if std::env::var_os("CARGO_CFG_WINDOWS").is_none() {
        return;
    }

    embed_resource::compile_for(
        "windows-resource.rc",
        ["music-folder"],
        embed_resource::NONE,
    )
    .manifest_required()
    .expect("the CLI Windows application manifest must be embedded");
}
