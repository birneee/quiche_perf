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

/// No error. This is used when the connection or stream needs to be closed, but there is no error to signal.
/// RFC 99114
const H3_NO_ERROR: u64 = 0x100;
