const WINDOWS_MANIFEST: &str = include_str!("../windows-app-manifest.xml");

#[test]
fn windows_manifest_keeps_common_controls_and_enables_long_paths() {
    assert!(WINDOWS_MANIFEST.contains("Microsoft.Windows.Common-Controls"));
    assert!(WINDOWS_MANIFEST.contains("<ws2:longPathAware>true</ws2:longPathAware>"));
    assert!(WINDOWS_MANIFEST.contains("http://schemas.microsoft.com/SMI/2016/WindowsSettings"));
}
