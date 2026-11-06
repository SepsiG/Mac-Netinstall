//! Test fixtures: synthetic ELF kernels, archives, NPKs, a tiny HTTP
//! server and a fake MikroTik mirror. No MikroTik bytes are used anywhere
//! in the tests.

mod elf;
mod files;
mod mirror;
mod server;

pub use elf::*;
pub use files::*;
pub use mirror::*;
pub use server::*;
