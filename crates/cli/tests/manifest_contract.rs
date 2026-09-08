const WINDOWS_MANIFEST: &str = include_str!("../windows-app-manifest.xml");
const WINDOWS_RESOURCE: &str = include_str!("../windows-resource.rc");
const BUILD_SCRIPT: &str = include_str!("../build.rs");

#[test]
fn cli_windows_manifest_keeps_common_controls_and_enables_long_paths() {
    assert!(WINDOWS_MANIFEST.contains("Microsoft.Windows.Common-Controls"));
    assert!(WINDOWS_MANIFEST.contains("version=\"6.0.0.0\""));
    assert!(WINDOWS_MANIFEST.contains("publicKeyToken=\"6595b64144ccf1df\""));
    assert!(WINDOWS_MANIFEST.contains("<ws2:longPathAware>true</ws2:longPathAware>"));
    assert!(WINDOWS_MANIFEST.contains("http://schemas.microsoft.com/SMI/2016/WindowsSettings"));
}

#[test]
fn cli_build_requires_resource_id_one_manifest_for_the_release_binary() {
    assert!(WINDOWS_RESOURCE.contains("1 RT_MANIFEST \"windows-app-manifest.xml\""));
    assert!(BUILD_SCRIPT.contains("compile_for"));
    assert!(BUILD_SCRIPT.contains("\"music-folder\""));
    assert!(BUILD_SCRIPT.contains("manifest_required"));
    assert!(BUILD_SCRIPT.contains("CARGO_CFG_WINDOWS"));
}
