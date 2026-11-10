//! The disclaimer users accept before the first install (the app's
//! settings and the CLI's `--accept-disclaimer`). Its text version is
//! [`DISCLAIMER_VERSION`]: when the text changes in substance, bump it and
//! everybody is asked again.

use crate::event::Failure;

/// Version of the disclaimer text.
pub const DISCLAIMER_VERSION: &str = "1";

/// Where the full text is (README, "Disclaimer").
pub const DISCLAIMER_URL: &str = "https://github.com/SepsiG/Mac-Netinstall#disclaimer";

/// The short disclaimer (English; the app shows its own translations).
pub const DISCLAIMER_TEXT: &str = "\
Mac Netinstall is provided \"as is\", without any warranty, as stated in
sections 15-16 of the GNU General Public License v3.
Netinstall erases the device's storage and reinstalls RouterOS. Use it at
your own risk. The authors and contributors accept no liability for any
damage or loss, including devices becoming unusable (\"bricked\"), lost
configuration or data, or downtime. Old, archived or end-of-life RouterOS
versions, versions below the device's minimum version and wrong packages
are especially risky. Test on a device you can recover first.
Mac Netinstall is not affiliated with, endorsed or supported by MikroTik.
MikroTik, RouterOS and RouterBOARD are trademarks of their owner.";

/// `disclaimer_not_accepted` (`version`): starting or flashing needs the
/// current disclaimer accepted.
pub fn not_accepted() -> Failure {
    Failure::new("disclaimer_not_accepted").with("version", DISCLAIMER_VERSION)
}

/// Whether `accepted` (a stored accepted version) is the current one.
pub fn is_accepted(accepted: Option<&str>) -> bool {
    accepted == Some(DISCLAIMER_VERSION)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_current_version_counts() {
        assert!(is_accepted(Some(DISCLAIMER_VERSION)));
        assert!(!is_accepted(None));
        assert!(!is_accepted(Some("0")));
        assert_eq!(not_accepted().code, "disclaimer_not_accepted");
        assert!(DISCLAIMER_TEXT.contains("GNU General Public License"));
        assert!(DISCLAIMER_TEXT.contains("not affiliated"));
    }
}
