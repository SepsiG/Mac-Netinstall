//! Test fixtures: synthetic ELF kernels, archives, NPKs, a tiny HTTP
//! server and a fake MikroTik mirror. No MikroTik bytes are used anywhere
//! in the tests.

mod elf;
mod files;

pub use elf::*;
pub use files::*;
