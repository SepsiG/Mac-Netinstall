//! Orchestration for mac-netinstall (being built; see Plan 4a).

pub mod event;
pub mod profile;

pub use event::{DeviceInfoView, DeviceState, EngineEvent, Failure, StartInfo, WaitReason};
pub use profile::{default_profiles_root, ConfigMode, Listing, Profile, ProfileStore};
