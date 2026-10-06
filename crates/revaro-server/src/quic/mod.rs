//! Optional standard HTTP/3 endpoint and bounded sender-side congestion policy.
//! HTTP framing, loss detection and retransmission remain upstream h3/Quinn.

mod bandwidth;
mod config;
mod controller;
mod pacing;
mod server;

pub use config::{CongestionMode, QuicConfig, TlsConfig, TlsIdentity};
pub use server::NativeTransport;
