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
//! reads IOKit. [`controller_classes`] (also IOKit) gives the classes of
//! each interface's controller and its providers, to tell real Ethernet
//! NICs from look-alikes.

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
    fn IORegistryEntryGetParentEntry(
        entry: IoObject,
        plane: *const c_char,
        parent: *mut IoObject,
    ) -> KernReturn;
    fn IOObjectGetClass(object: IoObject, class_name: *mut c_char) -> KernReturn;
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
    ethernet_interfaces()
        .into_iter()
        .filter_map(|(name, i)| Some((name, i.mac?)))
        .collect()
}

/// BSD name -> IOKit class names of the providers of every
/// `IOEthernetInterface`, nearest first (the controller, then its
/// providers; up to [`PROVIDER_DEPTH`]). They tell a real Ethernet NIC
/// (whatever the bus: PCIe, USB, Thunderbolt) from a Thunderbolt IP port
/// (`AppleThunderboltIPPort`), Wi-Fi (`IOUserNetworkWLAN`, ...) or the
/// Mac's own USB-C ports in device mode (`IOUSBDeviceInterface`). Empty if
/// IOKit cannot be queried.
pub fn controller_classes() -> HashMap<String, Vec<String>> {
    ethernet_interfaces()
        .into_iter()
        .map(|(name, i)| (name, i.providers))
        .collect()
}

/// What IOKit says about one `IOEthernetInterface`.
struct IoInterface {
    mac: Option<Mac>,
    providers: Vec<String>,
}

const PLANE: &CStr = c"IOService";
/// How many providers [`controller_classes`] lists per interface.
pub const PROVIDER_DEPTH: usize = 5;

/// The class names of `entry`'s providers in the service plane, nearest
/// first.
fn provider_classes(entry: &Object) -> Vec<String> {
    let mut out = Vec::new();
    let mut current: IoObject = entry.0;
    let mut owned: Option<Object> = None;
    for _ in 0..PROVIDER_DEPTH {
        let mut parent: IoObject = 0;
        // SAFETY: valid entry and plane name; on success `parent` is +1.
        let kr = unsafe { IORegistryEntryGetParentEntry(current, PLANE.as_ptr(), &mut parent) };
        if kr != 0 || parent == 0 {
            break;
        }
        let parent = Object(parent);
        let mut name = [0 as c_char; 128];
        // SAFETY: valid object; `io_name_t` is 128 bytes.
        if unsafe { IOObjectGetClass(parent.0, name.as_mut_ptr()) } == 0 {
            // SAFETY: IOKit wrote a NUL-terminated name.
            let class = unsafe { CStr::from_ptr(name.as_ptr()) };
            out.push(class.to_string_lossy().into_owned());
        }
        current = parent.0;
        owned = Some(parent); // keeps `current` alive; the previous one is released
    }
    drop(owned);
    out
}

/// Every `IOEthernetInterface` by BSD name.
fn ethernet_interfaces() -> HashMap<String, IoInterface> {
    const CLASS: &CStr = c"IOEthernetInterface";
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
        let Some(name) = name.and_then(|n| n.downcast_into::<CFString>()) else {
            continue;
        };
        let mac = mac
            .and_then(|m| m.downcast_into::<CFData>())
            .and_then(|m| mac_from_bytes(m.bytes()));
        let providers = provider_classes(&entry);
        out.insert(name.to_string(), IoInterface { mac, providers });
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
    fn iokit_names_the_controllers() {
        let all = controller_classes();
        assert!(!all.is_empty(), "IOKit listed no IOEthernetInterface");
        assert!(all.values().all(|chain| !chain.is_empty()));
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
