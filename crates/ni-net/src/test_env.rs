//! Tests that need the development Mac's own network hardware.
//!
//! A few tests compare against real NICs (IOKit's Ethernet controllers,
//! `ifconfig`'s addresses, "every Mac has en0"). CI runners are virtual
//! machines with other (virtual) NICs and may mask MAC addresses
//! differently, so these tests are skipped where the `CI` environment
//! variable is set (GitHub Actions sets `CI=true`), unless
//! `NI_HARDWARE_TESTS=1` asks for them. On a development Mac they run in
//! full; nothing is weakened.

/// Whether to skip the hardware test `name` (and say so on stderr).
pub(crate) fn skip_hardware_test(name: &str) -> bool {
    let skip = skip(
        std::env::var("CI").ok().as_deref(),
        std::env::var("NI_HARDWARE_TESTS").ok().as_deref(),
    );
    if skip {
        eprintln!(
            "skipped on CI: {name} needs the development Mac's NICs (NI_HARDWARE_TESTS=1 runs it)"
        );
    }
    skip
}

fn skip(ci: Option<&str>, force: Option<&str>) -> bool {
    let ci = ci.is_some_and(|v| !v.is_empty() && v != "false" && v != "0");
    ci && force != Some("1")
}

#[cfg(test)]
mod tests {
    use super::skip;

    #[test]
    fn hardware_tests_are_skipped_only_on_ci_unless_forced() {
        assert!(!skip(None, None));
        assert!(!skip(Some(""), None));
        assert!(!skip(Some("false"), None));
        assert!(!skip(Some("0"), None));
        assert!(skip(Some("true"), None));
        assert!(skip(Some("1"), Some("0")));
        assert!(!skip(Some("true"), Some("1")));
    }
}
