//! Dock Quit, logout and any other `terminate:` must ask first, too.
//!
//! The app menu's Quit is our own item (see `app_menu` in `lib.rs`), but
//! AppKit also terminates the app from the Dock's Quit, at logout,
//! restart or shutdown, and on Apple events, by sending `terminate:` to
//! `NSApplication`. AppKit then asks the application delegate
//! `applicationShouldTerminate:`; tao's delegate class
//! (`TaoAppDelegateParent`, tao 0.37.2, used by Tauri 2.12) does not
//! implement it, so the app terminates at once and Tauri's
//! `RunEvent::ExitRequested` is never sent.
//!
//! [`guard_termination`] therefore adds `applicationShouldTerminate:` to
//! the class of the delegate tao installed (`class_addMethod` at runtime,
//! after the event loop has set the delegate). It answers
//! `NSTerminateCancel` (and asks the window, `close-requested`) while
//! devices are being prepared or flashed, `NSTerminateNow` otherwise —
//! after stopping the engine.
//!
//! **Depends on tao's delegate not implementing the method itself.** If a
//! later tao does, `class_addMethod` fails and the method is replaced
//! instead (with a warning in the log): check then that tao's own
//! handling (likely a `RunEvent::ExitRequested`) is not lost. Manual check
//! (roadmap): with a device flashing on the SimLan, Dock → Quit and
//! logout must show the confirmation and keep the app running.

use objc2::ffi;
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{class, msg_send, sel};
use std::sync::OnceLock;
use tauri::AppHandle;

/// The handle and the decision function the `extern` method calls.
struct Hook {
    app: AppHandle,
    reply: fn(&AppHandle) -> usize,
}

static HOOK: OnceLock<Hook> = OnceLock::new();

/// `- (NSApplicationTerminateReply)applicationShouldTerminate:(NSApplication *)sender`
extern "C-unwind" fn application_should_terminate(
    _this: &AnyObject,
    _cmd: Sel,
    _sender: *mut AnyObject,
) -> usize {
    crate::terminate_reply_guarded(|| match HOOK.get() {
        Some(hook) => (hook.reply)(&hook.app),
        None => crate::NS_TERMINATE_NOW,
    })
}

/// Install `applicationShouldTerminate:` on the app delegate's class; it
/// answers with `reply`. Must run on the main thread, after the event loop
/// exists (Tauri's `setup`).
pub fn guard_termination(
    app: AppHandle,
    reply: fn(&AppHandle) -> usize,
) -> Result<(), Box<dyn std::error::Error>> {
    if HOOK.set(Hook { app, reply }).is_err() {
        return Err("termination guard installed twice".into());
    }
    let sel = sel!(applicationShouldTerminate:);
    // NSUInteger return; self, _cmd, an object.
    let types = c"Q@:@";
    // SAFETY: plain Objective-C runtime calls on the main thread; the
    // function matches the selector's signature (`types`), and an
    // `extern "C-unwind" fn` with that ABI may be stored as an `Imp`.
    unsafe {
        let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        if nsapp.is_null() {
            return Err("no NSApplication".into());
        }
        let delegate: *mut AnyObject = msg_send![nsapp, delegate];
        if delegate.is_null() {
            return Err("NSApplication has no delegate".into());
        }
        let cls: *const AnyClass = (*delegate).class();
        let imp: Imp = std::mem::transmute::<
            extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject) -> usize,
            Imp,
        >(application_should_terminate);
        let added = ffi::class_addMethod(cls as *mut AnyClass, sel, imp, types.as_ptr());
        if !added.as_bool() {
            tracing::warn!(
                "the app delegate implements applicationShouldTerminate: already; replacing it"
            );
            ffi::class_replaceMethod(cls as *mut AnyClass, sel, imp, types.as_ptr());
        }
        let responds: bool = msg_send![delegate, respondsToSelector: sel];
        if !responds {
            return Err("applicationShouldTerminate: could not be installed".into());
        }
    }
    tracing::info!("termination guard installed");
    Ok(())
}
