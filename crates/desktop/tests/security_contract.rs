use serde_json::Value;
use std::collections::BTreeSet;

const CAPABILITY: &str = include_str!("../capabilities/main.json");
const CONFIG: &str = include_str!("../tauri.conf.json");
const UI_MAIN: &str = include_str!("../../../ui/src/main.tsx");
const UI_CONFLICTS: &str = include_str!("../../../ui/src/conflicts.tsx");

#[test]
fn main_window_capability_is_local_exact_and_event_only() {
    let capability: Value = serde_json::from_str(CAPABILITY).unwrap();
    assert_eq!(capability["identifier"], "main-window-events");
    assert_eq!(capability["local"], true);
    assert!(capability.get("remote").is_none());
    assert_eq!(capability["windows"], serde_json::json!(["main"]));

    let actual = capability["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|permission| permission.as_str().unwrap())
        .collect::<BTreeSet<_>>();
    let expected = BTreeSet::from(["core:event:allow-listen", "core:event:allow-unlisten"]);
    assert_eq!(actual, expected);
    for forbidden in [
        "core:default",
        "core:event:default",
        "dialog:default",
        "dialog:allow-open",
        "fs:default",
        "shell:default",
        "shell:allow-open",
    ] {
        assert!(
            !actual.contains(forbidden),
            "forbidden permission: {forbidden}"
        );
    }
}

#[test]
fn frontend_api_usage_and_production_csp_match_the_allowlist() {
    assert!(UI_MAIN.contains("listen<Progress>"));
    assert!(UI_MAIN.contains("listen<ScanStatus>"));
    assert!(UI_MAIN.contains("invoke<"));
    assert!(UI_CONFLICTS.contains("invoke"));
    assert!(!UI_MAIN.contains("@tauri-apps/plugin-dialog"));
    assert!(!UI_MAIN.contains("@tauri-apps/plugin-fs"));
    assert!(!UI_MAIN.contains("@tauri-apps/plugin-shell"));

    let config: Value = serde_json::from_str(CONFIG).unwrap();
    assert_eq!(
        config["app"]["security"]["capabilities"],
        serde_json::json!(["main-window-events"]),
        "only the reviewed capability file may be activated",
    );
    let csp = config["app"]["security"]["csp"].as_str().unwrap();
    assert!(csp.contains("script-src 'self'"));
    assert!(csp.contains("style-src 'self'"));
    assert!(!csp.contains("'unsafe-inline'"));
    assert!(!csp.contains("'unsafe-eval'"));
    assert!(!csp.contains("https:"));
    assert!(!csp.contains("wss:"));
}
