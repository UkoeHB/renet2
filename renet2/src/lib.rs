#![cfg_attr(docsrs, feature(doc_cfg))]

mod channel;
mod connection_stats;
mod error;
mod packet;
mod remote_connection;
mod server;

pub use channel::{ChannelConfig, DefaultChannel, SendType};
pub use error::{ChannelError, ClientNotFound, DisconnectReason};
pub use packet::Payload;
pub use remote_connection::{ConnectionConfig, NetworkInfo, RenetClient, RenetConnectionStatus};
pub use server::{RenetServer, ServerEvent};

pub use bytes::Bytes;

/// Unique identifier for clients.
pub type ClientId = u64;
