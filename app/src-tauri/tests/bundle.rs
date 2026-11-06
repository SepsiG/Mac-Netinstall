//! The bundle settings and the window's permissions are part of the
//! design (bundle id, macOS 13+, Local Network text, minimal
//! capabilities, a CSP): pin them.

use serde_json::Value;
use std::path::Path;

fn read(name: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(name)).unwrap()
}

fn json(name: &str) -> Value {
    serde_json::from_str(&read(name)).unwrap()
}

#[test]
fn bundle_identity_and_platform() {
    let conf = json("tauri.conf.json");
    assert_eq!(conf["identifier"], "io.github.sepsig.mac-netinstall");
    assert_eq!(conf["identifier"], ni_assets::BUNDLE_ID);
    assert_eq!(conf["productName"], "Mac Netinstall");
    assert_eq!(conf["bundle"]["macOS"]["minimumSystemVersion"], "13.0");
    assert_eq!(conf["bundle"]["targets"], serde_json::json!(["app", "dmg"]));
    assert_eq!(conf["version"], env!("CARGO_PKG_VERSION"));
    // Ad-hoc signing with the bundle id: macOS then reports real MAC
    // addresses to the app (a linker-signed bundle gets 02:00:00:00:00:00).
    assert_eq!(conf["bundle"]["macOS"]["signingIdentity"], "-");
}

#[test]
fn info_plist_explains_local_network_use() {
    let plist = read("Info.plist");
    let key = plist
        .find("<key>NSLocalNetworkUsageDescription</key>")
        .unwrap();
    let text = &plist[key..];
    let start = text.find("<string>").unwrap() + "<string>".len();
    let end = text.find("</string>").unwrap();
    let description = &text[start..end];
    assert!(description.contains("MikroTik"), "{description}");
    assert!(description.contains("local network"), "{description}");
}

#[test]
fn the_web_view_is_locked_down() {
    let conf = json("tauri.conf.json");
    let csp = conf["app"]["security"]["csp"].as_str().unwrap();
    for part in [
        "default-src 'self'",
        "script-src 'self'",
        "object-src 'none'",
    ] {
        assert!(csp.contains(part), "CSP lacks {part}: {csp}");
    }
    assert!(!csp.contains("unsafe-eval"));
    let cap = json("capabilities/default.json");
    assert_eq!(cap["windows"], serde_json::json!(["main"]));
    let perms: Vec<&str> = cap["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect();
    for p in &perms {
        let ok = p.starts_with("allow-") || p.starts_with("core:event:allow-");
        assert!(ok, "unexpected permission {p}");
    }
    // Every app command is allowed; nothing else from the core or plugins.
    let build = read("build.rs");
    let listed = build.matches("    \"").count();
    assert_eq!(
        perms.iter().filter(|p| p.starts_with("allow-")).count(),
        listed
    );
}
