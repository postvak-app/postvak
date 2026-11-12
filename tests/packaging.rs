//! Packaging files that must agree with the code.

#[test]
fn the_apps_login_item_opens_the_window_in_the_background() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let plist = std::fs::read_to_string(root.join("packaging/macos/login-item.plist")).unwrap();
    let label = postvak_core::autostart::LOGIN_ITEM_LABEL;
    assert!(plist.contains(&format!("<string>{label}</string>")));
    let arguments: String = postvak_core::autostart::GUI_ARGS
        .iter()
        .map(|arg| format!("\n        <string>{arg}</string>"))
        .collect();
    assert!(plist.contains(&format!("<string>postvak</string>{arguments}")));
    let script = std::fs::read_to_string(root.join("packaging/macos/package.sh")).unwrap();
    assert!(script.contains(&format!("Contents/Library/LaunchAgents/{label}.plist")));
}
