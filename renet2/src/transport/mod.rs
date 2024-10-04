use std::{error::Error, fmt};

mod client;
#[cfg(feature = "memory_transport")]
mod memory_socket;
#[cfg(all(feature = "native_transport", not(target_family = "wasm")))]
mod native_socket;
mod server;
mod transport_socket;
mod webtransport_socket;

// #[cfg(feature = "ws_server_transport")]
mod websocket;

pub use client::*;
pub use server::*;
pub use transport_socket::*;

#[cfg(feature = "memory_transport")]
pub use memory_socket::*;
#[cfg(all(feature = "native_transport", not(target_family = "wasm")))]
pub use native_socket::*;
pub use webtransport_socket::*;

// #[cfg(feature = "ws_server_transport")]
pub use websocket::*;

pub use renetcode2::{
    generate_random_bytes, ClientAuthentication, ConnectToken, DisconnectReason as NetcodeDisconnectReason, NetcodeError,
    ServerAuthentication, ServerConfig, ServerSocketConfig, TokenGenerationError, NETCODE_KEY_BYTES, NETCODE_USER_DATA_BYTES,
};

#[derive(Debug)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::prelude::Event))]
pub enum NetcodeTransportError {
    Netcode(NetcodeError),
    Renet(crate::DisconnectReason),
    IO(std::io::Error),
}

impl Error for NetcodeTransportError {}

impl fmt::Display for NetcodeTransportError {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            NetcodeTransportError::Netcode(ref err) => err.fmt(fmt),
            NetcodeTransportError::Renet(ref err) => err.fmt(fmt),
            NetcodeTransportError::IO(ref err) => err.fmt(fmt),
        }
    }
}

impl From<renetcode2::NetcodeError> for NetcodeTransportError {
    fn from(inner: renetcode2::NetcodeError) -> Self {
        NetcodeTransportError::Netcode(inner)
    }
}

impl From<renetcode2::TokenGenerationError> for NetcodeTransportError {
    fn from(inner: renetcode2::TokenGenerationError) -> Self {
        NetcodeTransportError::Netcode(renetcode2::NetcodeError::TokenGenerationError(inner))
    }
}

impl From<crate::DisconnectReason> for NetcodeTransportError {
    fn from(inner: crate::DisconnectReason) -> Self {
        NetcodeTransportError::Renet(inner)
    }
}

impl From<std::io::Error> for NetcodeTransportError {
    fn from(inner: std::io::Error) -> Self {
        NetcodeTransportError::IO(inner)
    }
}
