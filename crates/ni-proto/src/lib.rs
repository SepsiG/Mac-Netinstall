//! Sans-IO implementation of the MikroTik Netinstall UDP protocol (server side).
//!
//! Protocol knowledge is derived from pyNetinstall
//! (<https://github.com/rfdrake/pynetinstall>, GPL-3.0).

pub mod device_info;
pub mod frame;
pub mod message;
pub mod session;

pub use device_info::DeviceInfo;
pub use frame::{DecodeError, Frame, Mac, ParseMacError, HEADER_LEN, MAX_CHUNK, PORT};
pub use session::{Event, FailReason, InstallPlan, Phase, PlanFile, ServerSession, SessionConfig};
