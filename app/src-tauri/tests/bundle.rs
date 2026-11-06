//! The bundle settings and the window's permissions are part of the
//! design (bundle id, macOS 13+, Local Network text, minimal
//! capabilities, a CSP): pin them.

use serde_json::Value;
use std::collections::BTreeSet;
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

/// `"key" = "value";` lines of an `InfoPlist.strings`.
fn strings_file(path: &str) -> std::collections::BTreeMap<String, String> {
    read(path)
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            let (k, v) = l.strip_suffix(';')?.split_once(" = ")?;
            Some((k.trim_matches('"').to_string(), v.trim_matches('"').to_string()))
        })
        .collect()
}

#[test]
fn the_local_network_text_is_localized_and_bundled() {
    let conf = json("tauri.conf.json");
    let files = &conf["bundle"]["macOS"]["files"];
    for lang in ["en", "hu"] {
        let src = files[format!("Resources/{lang}.lproj/InfoPlist.strings")]
            .as_str()
            .unwrap_or_else(|| panic!("{lang}.lproj not bundled: {files}"));
        let strings = strings_file(src.trim_start_matches("./"));
        let text = &strings["NSLocalNetworkUsageDescription"];
        assert!(text.contains("MikroTik"), "{lang}: {text}");
    }
    let hu = strings_file("locales/hu.lproj/InfoPlist.strings");
    assert!(hu["NSLocalNetworkUsageDescription"].contains("helyi hálózat"));
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
    // Every app command is allowed (and nothing else from the core or
    // plugins): the capability, build.rs's command list, the
    // `#[tauri::command]`s and the invoke handler all agree.
    let allowed: BTreeSet<String> = perms
        .iter()
        .filter_map(|p| p.strip_prefix("allow-"))
        .map(|p| p.replace('-', "_"))
        .collect();
    let listed = build_commands();
    assert_eq!(allowed, listed, "capabilities vs build.rs");
    assert_eq!(command_fns(), listed, "commands.rs vs build.rs");
    assert_eq!(handled_commands(), listed, "invoke handler vs build.rs");
}

/// The `COMMANDS` array of build.rs.
fn build_commands() -> BTreeSet<String> {
    let build = read("build.rs");
    let start = build.find("const COMMANDS: &[&str] = &[").unwrap();
    let body = &build[start..];
    let body = &body[body.find('[').unwrap() + 1..];
    let body = &body[body.find('[').unwrap() + 1..body.find("];").unwrap()];
    body.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.trim_matches('"').to_string())
        .collect()
}

/// The functions marked `#[tauri::command…]` in commands.rs.
fn command_fns() -> BTreeSet<String> {
    let src = read("src/commands.rs");
    src.split("\n#[tauri::command")
        .skip(1)
        .map(|rest| {
            let f = &rest[rest.find("fn ").unwrap() + 3..];
            f[..f.find(['(', '<']).unwrap()].to_string()
        })
        .collect()
}

/// The `commands::…` entries of `generate_handler!` in lib.rs.
fn handled_commands() -> BTreeSet<String> {
    let src = read("src/lib.rs");
    let start = src.find("generate_handler![").unwrap();
    let body = &src[start + "generate_handler![".len()..];
    let body = &body[..body.find(']').unwrap()];
    body.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.trim_start_matches("commands::").to_string())
        .collect()
}
