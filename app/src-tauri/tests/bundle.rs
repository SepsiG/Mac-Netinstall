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
fn the_main_window_is_resizable_down_to_a_usable_size() {
    let conf = json("tauri.conf.json");
    let windows = conf["app"]["windows"].as_array().unwrap();
    assert_eq!(windows.len(), 1);
    let w = &windows[0];
    assert_eq!(w["label"], "main");
    assert_eq!(w["resizable"], true);
    // The minimum is what the settings accept for a remembered window.
    use mac_netinstall_app::settings::{MIN_WINDOW_HEIGHT, MIN_WINDOW_WIDTH};
    assert_eq!(w["minWidth"], MIN_WINDOW_WIDTH);
    assert_eq!(w["minHeight"], MIN_WINDOW_HEIGHT);
    assert!(w["width"].as_u64().unwrap() >= u64::from(MIN_WINDOW_WIDTH));
    assert!(w["height"].as_u64().unwrap() >= u64::from(MIN_WINDOW_HEIGHT));
    assert!(w.get("maxWidth").is_none() && w.get("maxHeight").is_none());
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
            Some((
                k.trim_matches('"').to_string(),
                v.trim_matches('"').to_string(),
            ))
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

/// Width and height from a PNG's IHDR chunk.
fn png_size(path: &Path) -> (u32, u32) {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "{}", path.display());
    assert_eq!(&bytes[12..16], b"IHDR", "{}", path.display());
    let be = |i: usize| u32::from_be_bytes(bytes[i..i + 4].try_into().unwrap());
    (be(16), be(20))
}

#[test]
fn the_bundle_icons_are_a_complete_macos_icon_set() {
    // The bundler builds the .icns from these PNGs; "@2x" marks the
    // Retina variant (twice the pixels). They are rendered from
    // app/icon/*.svg by app/scripts/make-icon.mjs (the frontend tests check
    // they are up to date).
    let conf = json("tauri.conf.json");
    let icons: Vec<&str> = conf["bundle"]["icon"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    // The first PNG is also the window icon (tauri-codegen).
    assert_eq!(icons.first(), Some(&"icons/icon.png"));
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut slots = BTreeSet::new();
    for icon in &icons {
        let (w, h) = png_size(&dir.join(icon));
        assert_eq!(w, h, "{icon} is square");
        let name = icon.strip_prefix("icons/").unwrap();
        let (points, scale) = match name {
            "icon.png" => (512, 1),
            _ => {
                let (base, scale) = match name.strip_suffix("@2x.png") {
                    Some(base) => (base, 2),
                    None => (name.strip_suffix(".png").unwrap(), 1),
                };
                let (a, b) = base.split_once('x').unwrap();
                assert_eq!(a, b, "{icon}");
                (a.parse::<u32>().unwrap(), scale)
            }
        };
        assert_eq!(w, points * scale, "{icon}: {w} px");
        assert!(slots.insert((points, scale)), "{icon} duplicates a size");
    }
    let expected: BTreeSet<(u32, u32)> = [16, 32, 128, 256, 512]
        .into_iter()
        .flat_map(|p| [(p, 1), (p, 2)])
        .collect();
    assert_eq!(slots, expected);
    // Nothing else lingers in icons/ (e.g. an old .icns would be used as is).
    let on_disk: BTreeSet<String> = std::fs::read_dir(dir.join("icons"))
        .unwrap()
        .map(|e| format!("icons/{}", e.unwrap().file_name().to_string_lossy()))
        .collect();
    let listed: BTreeSet<String> = icons.iter().map(|s| s.to_string()).collect();
    assert_eq!(on_disk, listed);
}

#[test]
fn the_app_is_a_regular_dock_app() {
    // It shows in the Dock and the app switcher while running: no
    // agent/background-only keys (lib.rs also sets the Regular activation
    // policy explicitly).
    let plist = read("Info.plist");
    for key in ["LSUIElement", "LSBackgroundOnly"] {
        assert!(!plist.contains(key), "Info.plist sets {key}");
    }
    let lib = read("src/lib.rs");
    assert!(lib.contains("set_activation_policy(tauri::ActivationPolicy::Regular)"));
    assert!(
        !lib.contains("ActivationPolicy::Accessory")
            && !lib.contains("ActivationPolicy::Prohibited")
    );
}

#[test]
fn the_dmg_offers_drag_to_applications() {
    // The app on the left, the Applications link on the right, in a
    // window that fits both. Finder applies the layout through AppleScript,
    // which `CI=true` builds skip: they get the same two items without
    // the layout (see README, "Building a release").
    let conf = json("tauri.conf.json");
    let dmg = &conf["bundle"]["macOS"]["dmg"];
    let (w, h) = (
        dmg["windowSize"]["width"].as_u64().unwrap(),
        dmg["windowSize"]["height"].as_u64().unwrap(),
    );
    assert!(
        (500..=900).contains(&w) && (300..=600).contains(&h),
        "{w}x{h}"
    );
    let app = &dmg["appPosition"];
    let apps = &dmg["applicationFolderPosition"];
    assert_eq!(app["y"], apps["y"], "side by side");
    let (ax, bx) = (app["x"].as_u64().unwrap(), apps["x"].as_u64().unwrap());
    assert!(
        ax < bx && bx < w,
        "app left of Applications, inside the window"
    );
}
