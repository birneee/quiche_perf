#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![warn(unused_extern_crates)]

#[allow(unused_imports)]
// suppress warning of unused creates
use env_logger as _;

pub mod server;
pub mod client;
pub mod args;
mod cert;
mod h3;

/// code is submitted to close the connection after all transfers where successful
const ERROR_CODE_SUCCESS: u64 = 0x100;