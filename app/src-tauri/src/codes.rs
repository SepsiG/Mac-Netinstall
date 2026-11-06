//! The app's own failure codes, and the full code catalog for the UI's
//! translation check (`app/src/lib/codes.json`, see `tests/codes.rs`).

use serde::Serialize;

/// Codes the bridge reports itself (besides the engine's).
pub const APP_FAILURES: &[&str] = &[
    "asset_in_use",
    "cache_failed",
    "dialog_failed",
    "invalid_mac",
    "invalid_setting",
    "log_io",
    "no_home",
    "not_a_script",
    "packages_unavailable",
    "profile_exists",
    "script_failed",
    "settings_io",
];

/// The engine's catalog plus [`APP_FAILURES`].
#[derive(Debug, Clone, Serialize)]
pub struct AppCatalog {
    #[serde(flatten)]
    pub engine: ni_engine::codes::Catalog,
    pub app_failures: Vec<&'static str>,
}

pub fn catalog() -> AppCatalog {
    AppCatalog {
        engine: ni_engine::codes::catalog(),
        app_failures: APP_FAILURES.to_vec(),
    }
}

/// The catalog as the JSON file the frontend tests read.
pub fn catalog_json() -> String {
    let mut s = serde_json::to_string_pretty(&catalog()).expect("catalog serializes");
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bridge's sources, without their test modules.
    const SOURCES: &[&str] = &[
        include_str!("core.rs"),
        include_str!("commands.rs"),
        include_str!("settings.rs"),
        include_str!("lib.rs"),
    ];

    #[test]
    fn every_app_failure_is_listed_and_new() {
        let engine = ni_engine::codes::FAILURES;
        for src in SOURCES {
            let src = src.split("#[cfg(test)]\nmod tests").next().unwrap();
            for prefix in ["Failure::new(\"", "asset_cause(\""] {
                for (i, _) in src.match_indices(prefix) {
                    let rest = &src[i + prefix.len()..];
                    let code = &rest[..rest.find('"').unwrap()];
                    assert!(
                        APP_FAILURES.contains(&code) || engine.contains(&code),
                        "{code} is not listed"
                    );
                }
            }
        }
        for code in APP_FAILURES {
            assert!(!engine.contains(code), "{code} is an engine code already");
        }
    }
}
