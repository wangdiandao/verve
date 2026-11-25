//! Mihomo kernel adapter over Named Pipe / Unix Domain Socket and CLI preflight.

mod client;
mod discovery;
mod preflight;
mod schema;
mod transport;

pub use client::{MihomoKernel, MihomoKernelConfig};
pub use discovery::discover_mihomo_binary;
