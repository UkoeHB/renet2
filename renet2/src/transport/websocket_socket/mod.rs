#[cfg(all(feature = "ws_client_transport", target_family = "wasm"))]
mod client;

#[cfg(all(feature = "ws_server_transport", not(target_family = "wasm")))]
mod server;

#[cfg(all(feature = "ws_client_transport", target_family = "wasm"))]
pub use client::*;

#[cfg(all(feature = "ws_server_transport", not(target_family = "wasm")))]
pub use server::*;
