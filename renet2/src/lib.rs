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

/// Resizes `buffer` to `size` and sets all bytes to `0`.
pub(crate) fn zero_reinit_buffer(buffer: &mut Vec<u8>, size: usize) {
    buffer.reserve_exact(size.saturating_sub(buffer.capacity()));
    // SAFETY: writing to capacity
    unsafe {
        let ptr = buffer.as_mut_ptr();
        std::ptr::write_bytes(ptr, 0u8, size);
        buffer.set_len(size);
    }
}
