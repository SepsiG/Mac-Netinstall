//! Sans-IO implementation of the MikroTik Netinstall UDP protocol (server side).
//!
//! Protocol knowledge is derived from pyNetinstall
//! (<https://github.com/rfdrake/pynetinstall>, GPL-3.0) and from captures of
//! the official `netinstall-cli` 7.25.1 talking to a simulated device
//! (`docs/superpowers/research/2026-11-06-official-netinstall-wire-spec.md`),
//! which is what the session follows where the two differ.

pub mod device_info;
pub mod frame;
pub mod message;
pub mod session;

pub use device_info::{DeviceInfo, HELLO_TAG};
pub use frame::{DecodeError, Frame, Mac, ParseMacError, HEADER_LEN, MAX_CHUNK, PORT};
pub use message::{AfterInstall, Offer};
pub use session::{
    Event, FailReason, InstallPlan, Phase, PlanError, PlanFile, ServerSession, SessionConfig,
};
