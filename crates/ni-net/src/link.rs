//! MAC addresses that survive macOS's address masking.
//!
//! macOS reports `02:00:00:00:00:00` as the link-layer address of every
//! interface to some processes — observed for ad-hoc, linker-signed
//! binaries such as anything `cargo build` produces — and it does so for
//! `getifaddrs(3)` and the `NET_RT_IFLIST` routing sysctl alike (child
//! processes such as `ifconfig` inherit the masking). IOKit's
//! `IOMACAddress` property of `IOEthernetController` is not masked, but it
//! is the *hardware* address: for Wi-Fi with a private (rotating) address it
//! is not the address in use.
//!
//! [`known_mac`] turns placeholders into "unknown"; [`hardware_addresses`]
//! reads IOKit.

use core_foundation::base::{CFType, TCFType};
use core_foundation::data::CFData;
use core_foundation::string::CFString;
use core_foundation_sys::base::{CFAllocatorRef, CFTypeRef};
use core_foundation_sys::dictionary::CFMutableDictionaryRef;
use core_foundation_sys::string::CFStringRef;
use ni_proto::Mac;
use std::collections::HashMap;
use std::ffi::{c_char, c_int, CStr};

/// The masked address macOS hands out instead of the real one.
pub const MASKED_MAC: Mac = Mac([0x02, 0, 0, 0, 0, 0]);

/// `mac`, unless it is a placeholder ([`MASKED_MAC`] or all-zero).
pub fn known_mac(mac: Mac) -> Option<Mac> {
    (mac != MASKED_MAC && mac != Mac::ZERO).then_some(mac)
}

/// A known MAC from exactly six bytes; `None` for any other length or a
/// placeholder.
pub fn mac_from_bytes(bytes: &[u8]) -> Option<Mac> {
    known_mac(Mac(bytes.try_into().ok()?))
}

type IoObject = u32;
type KernReturn = c_int;

/// `kIORegistryIterateRecursively | kIORegistryIterateParents`.
const ITERATE_PARENTS_RECURSIVELY: u32 = 0x1 | 0x2;

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOServiceMatching(name: *const c_char) -> CFMutableDictionaryRef;
    fn IOServiceGetMatchingServices(
        main_port: u32,
        matching: CFMutableDictionaryRef,
        existing: *mut IoObject,
    ) -> KernReturn;
    fn IOIteratorNext(iterator: IoObject) -> IoObject;
    fn IOObjectRelease(object: IoObject) -> KernReturn;
    fn IORegistryEntryCreateCFProperty(
        entry: IoObject,
        key: CFStringRef,
        allocator: CFAllocatorRef,
        options: u32,
    ) -> CFTypeRef;
    fn IORegistryEntrySearchCFProperty(
        entry: IoObject,
        plane: *const c_char,
        key: CFStringRef,
        allocator: CFAllocatorRef,
        options: u32,
    ) -> CFTypeRef;
}

/// An owned IOKit object reference.
struct Object(IoObject);

impl Drop for Object {
    fn drop(&mut self) {
        // SAFETY: we own one reference to a non-null object.
        unsafe { IOObjectRelease(self.0) };
    }
}

/// Wrap a +1 CF reference (or null).
fn owned_cf(r: CFTypeRef) -> Option<CFType> {
    // SAFETY: `r` is null or a reference we own (a Create/Copy result).
    (!r.is_null()).then(|| unsafe { CFType::wrap_under_create_rule(r) })
}

/// BSD name -> hardware MAC of every `IOEthernetInterface` (Ethernet,
/// Wi-Fi, USB/Thunderbolt adapters), from its controller's `IOMACAddress`.
/// Empty if IOKit cannot be queried. Placeholders are left out.
pub fn hardware_addresses() -> HashMap<String, Mac> {
    const CLASS: &CStr = c"IOEthernetInterface";
    const PLANE: &CStr = c"IOService";
    let mut out = HashMap::new();
    let bsd_key = CFString::from_static_string("BSD Name");
    let mac_key = CFString::from_static_string("IOMACAddress");
    let mut iter: IoObject = 0;
    // SAFETY: `IOServiceMatching` returns a +1 dictionary (or null) that
    // `IOServiceGetMatchingServices` consumes; port 0 is the default main
    // port.
    let kr = unsafe {
        let matching = IOServiceMatching(CLASS.as_ptr());
        if matching.is_null() {
            return out;
        }
        IOServiceGetMatchingServices(0, matching, &mut iter)
    };
    if kr != 0 || iter == 0 {
        return out;
    }
    let iter = Object(iter);
    loop {
        // SAFETY: `iter` is a live iterator; the result is +1 or 0.
        let entry = unsafe { IOIteratorNext(iter.0) };
        if entry == 0 {
            break;
        }
        let entry = Object(entry);
        // SAFETY: valid entry and key; both calls return +1 references.
        let (name, mac) = unsafe {
            (
                owned_cf(IORegistryEntryCreateCFProperty(
                    entry.0,
                    bsd_key.as_concrete_TypeRef(),
                    std::ptr::null(),
                    0,
                )),
                owned_cf(IORegistryEntrySearchCFProperty(
                    entry.0,
                    PLANE.as_ptr(),
                    mac_key.as_concrete_TypeRef(),
                    std::ptr::null(),
                    ITERATE_PARENTS_RECURSIVELY,
                )),
            )
        };
        let name = name.and_then(|n| n.downcast_into::<CFString>());
        let mac = mac.and_then(|m| m.downcast_into::<CFData>());
        if let (Some(name), Some(mac)) = (name, mac) {
            if let Some(mac) = mac_from_bytes(mac.bytes()) {
                out.insert(name.to_string(), mac);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const EN7: [u8; 6] = [0x48, 0xa9, 0x8a, 0x11, 0x22, 0x33];

    #[test]
    fn placeholders_are_unknown() {
        assert_eq!(known_mac(MASKED_MAC), None);
        assert_eq!(known_mac(Mac::ZERO), None);
        assert_eq!(known_mac(Mac(EN7)), Some(Mac(EN7)));
    }

    #[test]
    fn mac_from_bytes_needs_six_known_bytes() {
        assert_eq!(mac_from_bytes(&EN7), Some(Mac(EN7)));
        assert_eq!(mac_from_bytes(&MASKED_MAC.0), None);
        assert_eq!(mac_from_bytes(&[0; 6]), None);
        assert_eq!(mac_from_bytes(&EN7[..5]), None);
        assert_eq!(mac_from_bytes(&[1, 2, 3, 4, 5, 6, 7, 8]), None);
        assert_eq!(mac_from_bytes(&[]), None);
    }

    #[test]
    fn iokit_lists_real_hardware_addresses() {
        // Every Mac has at least one Ethernet-like interface (en0).
        let all = hardware_addresses();
        assert!(!all.is_empty(), "IOKit listed no IOEthernetInterface");
        assert!(all.keys().all(|n| !n.is_empty()));
        assert!(!all.contains_key("lo0"));
        assert!(all.values().all(|m| known_mac(*m).is_some()));
    }

    proptest! {
        #[test]
        fn mac_from_bytes_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..16)) {
            let got = mac_from_bytes(&bytes);
            prop_assert_eq!(got.is_some(), bytes.len() == 6 && known_mac(Mac(bytes[..].try_into().unwrap())).is_some());
        }
    }
}
