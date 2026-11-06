//! Orchestration for mac-netinstall (being built; see Plan 4a).

pub mod event;

pub use event::{DeviceInfoView, DeviceState, EngineEvent, Failure, StartInfo, WaitReason};
