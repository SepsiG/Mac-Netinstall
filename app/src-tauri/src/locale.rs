//! The user's preferred languages, from macOS.
//!
//! `navigator.languages` in the web view reflects only the localizations
//! the app bundle declares, so the UI asks Rust instead.

use core_foundation::array::CFArray;
use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use core_foundation_sys::locale::CFLocaleCopyPreferredLanguages;

/// BCP 47 tags in order of preference, e.g. `["hu-HU", "en-US"]`.
pub fn preferred_languages() -> Vec<String> {
    // SAFETY: CFLocaleCopyPreferredLanguages follows the Create rule and
    // returns an array of CFStrings (or null).
    let array = unsafe { CFLocaleCopyPreferredLanguages() };
    if array.is_null() {
        return Vec::new();
    }
    // SAFETY: we own the reference returned above.
    let array: CFArray<CFString> = unsafe { CFArray::wrap_under_create_rule(array) };
    array.iter().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_is_at_least_one_preferred_language() {
        let langs = preferred_languages();
        assert!(!langs.is_empty());
        assert!(langs.iter().all(|l| !l.is_empty()));
    }
}
