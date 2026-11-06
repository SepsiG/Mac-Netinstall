//! `app/src/lib/codes.json` (read by the UI's translation tests) must be
//! the current code catalog. Regenerate it with
//! `UPDATE_CODES=1 cargo test -p mac-netinstall --test codes`.

use mac_netinstall_app::codes::catalog_json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[test]
fn the_ui_code_list_is_up_to_date() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/lib/codes.json");
    let want = catalog_json();
    if std::env::var_os("UPDATE_CODES").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &want).unwrap();
    }
    let have = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        have == want,
        "{} is out of date: run UPDATE_CODES=1 cargo test -p mac-netinstall --test codes",
        path.display()
    );
}

/// Rust sources that build failures (engine and app), without their test
/// modules.
fn failure_sources() -> Vec<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    let mut stack = vec![root.join("../../crates/ni-engine/src"), root.join("src")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                files.push(path);
            }
        }
    }
    files
        .into_iter()
        .map(|p| {
            let src = std::fs::read_to_string(&p).unwrap();
            src.split("#[cfg(test)]\nmod tests")
                .next()
                .unwrap()
                .to_string()
        })
        .collect()
}

/// The `.with("key", …)` keys chained onto the expression that starts at
/// `src` (up to the end of the expression: a `;`, or a `,` / `)` that
/// closes an enclosing call).
fn chained_keys(src: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut depth = 0i32;
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth < 0 {
                    break;
                }
            }
            b';' | b',' if depth == 0 => break,
            b'"' => {
                // Skip string literals.
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'.' if depth == 0 && src[i..].starts_with(".with(\"") => {
                let rest = &src[i + ".with(\"".len()..];
                keys.push(rest[..rest.find('"').unwrap()].to_string());
            }
            _ => {}
        }
        i += 1;
    }
    keys
}

/// Every code with the parameter names the Rust side can send with it.
fn params_by_code() -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    // Constructors that add parameters themselves.
    let helpers: &[(&str, &[&str])] = &[
        ("Failure::new(\"", &[]),
        ("asset_failure(\"", &["cause", "detail"]),
        ("asset_cause(\"", &["cause"]),
    ];
    for src in failure_sources() {
        for (prefix, implied) in helpers {
            for (i, _) in src.match_indices(prefix) {
                let rest = &src[i + prefix.len()..];
                let code = &rest[..rest.find('"').unwrap()];
                // The constructor's own closing parenthesis comes first.
                let after = &rest[rest.find(')').unwrap() + 1..];
                let set = out.entry(code.to_string()).or_default();
                set.extend(implied.iter().map(|s| s.to_string()));
                set.extend(chained_keys(after));
            }
        }
        // `invalid("field")` in settings.rs: `invalid_setting` (`field`, ...).
        for (i, _) in src.match_indices("invalid(\"") {
            let rest = &src[i..];
            let after = &rest[rest.find(')').unwrap() + 1..];
            let set = out.entry("invalid_setting".into()).or_default();
            set.insert("field".into());
            set.extend(chained_keys(after));
        }
    }
    // Built from a parameter list rather than a literal chain: the
    // engine's `link_down` callers and failure helpers.
    for (code, keys) in [
        ("kernel_version_unknown", &["mac"][..]),
        ("interface_gone", &["interface", "ip", "reason"][..]),
    ] {
        out.entry(code.into())
            .or_default()
            .extend(keys.iter().map(|k| k.to_string()));
    }
    out
}

/// `{name}` / `{name, plural, …}` placeholders of an ICU message.
fn placeholders(message: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (i, _) in message.match_indices('{') {
        // `one {…}`, `other {…}`, `=0 {…}`: a plural branch, not a name.
        let before = message[..i].trim_end();
        let word = before
            .rsplit(|c: char| c.is_whitespace() || c == '{' || c == '}')
            .next()
            .unwrap_or("");
        if ["zero", "one", "two", "few", "many", "other"].contains(&word) || word.starts_with('=') {
            continue;
        }
        let rest = &message[i + 1..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        let next = rest[name.len()..].trim_start().chars().next();
        if !name.is_empty()
            && name.chars().next().unwrap().is_ascii_alphabetic()
            && matches!(next, Some('}' | ','))
        {
            out.insert(name);
        }
    }
    out
}

#[test]
fn the_scan_finds_parameters() {
    let params = params_by_code();
    assert!(params["bind_failed"].contains("service"));
    assert!(params["port_in_use"].contains("port"));
    assert!(params["invalid_setting"].contains("field"));
    assert!(params["profile_version_mismatch"].contains("kernel_version"));
    assert_eq!(
        placeholders("{count, plural, one {# device {mac}} other {x}} on {interface}"),
        ["count", "interface", "mac"]
            .into_iter()
            .map(String::from)
            .collect()
    );
}

#[test]
fn translations_only_use_parameters_rust_sends() {
    let params = params_by_code();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/locales");
    for lang in ["en", "hu"] {
        let text = std::fs::read_to_string(root.join(format!("{lang}.json"))).unwrap();
        let messages: BTreeMap<String, String> = serde_json::from_str(&text).unwrap();
        for (key, message) in &messages {
            let Some(code) = key.strip_prefix("code.") else {
                continue;
            };
            if code == "unknown" {
                continue; // the UI fills in `code` itself
            }
            let Some(sent) = params.get(code) else {
                continue; // an asset cause or similar: no parameters
            };
            for name in placeholders(message) {
                assert!(
                    sent.contains(&name),
                    "{lang}: {key} uses {{{name}}}, but Rust sends only {sent:?}"
                );
            }
        }
    }
}
