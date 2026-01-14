use std::{io::ErrorKind, net::SocketAddr};

use crate::{ClientSocket, NetcodeTransportError};
use renetcode2::NETCODE_MAX_PACKET_BYTES;

use super::*;

/// Implementation of [`ClientSocket`] for clients of an in-memory channel server socket.
///
/// In-memory sockets are treated as encrypted and reliable by default for efficiency. Use [`Self::new_with`] to use
/// a different policy (this is useful for performance tests).
#[derive(Debug, Clone)]
pub struct MemorySocketClient {
    client_id: u16,
    channels: MemorySocketChannels,
    encrypted: bool,
    reliable: bool,
}

impl MemorySocketClient {
    /// Makes a new in-memory client socket.
    pub fn new(client_id: u16, channels: MemorySocketChannels) -> Self {
        Self::new_with(client_id, channels, true, true)
    }

    /// Makes a new in-memory client socket with a specific encryption and reliability policy.
    pub fn new_with(client_id: u16, channels: MemorySocketChannels, encrypted: bool, reliable: bool) -> Self {
        assert!(client_id != IN_MEMORY_SERVER_ID);
        Self {
            client_id,
            channels,
            encrypted,
            reliable,
        }
    }

    /// Gets the inner client id that is used to make client addresses.
    ///
    /// This may not equal the `client_id` used in `netcode` unless you intentionally make them the same.
    pub fn id(&self) -> u64 {
        self.client_id as u64
    }
}

impl ClientSocket for MemorySocketClient {
    fn is_encrypted(&self) -> bool {
        self.encrypted
    }
    fn is_reliable(&self) -> bool {
        self.reliable
    }

    fn addr(&self) -> std::io::Result<SocketAddr> {
        Ok(in_memory_client_addr(self.client_id))
    }

    fn is_closed(&mut self) -> bool {
        false
    }

    fn close(&mut self) {}
    fn preupdate(&mut self) {}

    fn try_recv(&mut self, buffer: &mut [u8]) -> std::io::Result<(usize, SocketAddr)> {
        assert!(buffer.len() >= NETCODE_MAX_PACKET_BYTES);

        let packet = self
            .channels
            .receiver
            .try_recv()
            .map_err(|_| std::io::Error::from(ErrorKind::WouldBlock))?;
        buffer[..packet.len].copy_from_slice(&packet.bytes[..packet.len]);

        Ok((packet.len, in_memory_server_addr()))
    }

    fn postupdate(&mut self) {}

    fn send(&mut self, addr: SocketAddr, packet: &[u8]) -> Result<(), NetcodeTransportError> {
        assert_eq!(addr, in_memory_server_addr());
        assert!(packet.len() <= NETCODE_MAX_PACKET_BYTES);

        let mut mem_packet = InMemoryPacket {
            len: packet.len(),
            ..Default::default()
        };
        mem_packet.bytes[..packet.len()].copy_from_slice(packet);
        self.channels
            .sender
            .send(mem_packet)
            .map_err(|_| std::io::Error::from(ErrorKind::ConnectionAborted))?;

        Ok(())
    }
}
