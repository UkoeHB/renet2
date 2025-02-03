use std::fmt::Debug;
use std::net::SocketAddr;

use super::NetcodeTransportError;

/// Unreliable data source for use in [`NetcodeServerTransport`](super::NetcodeServerTransport).
///
/// Note that while `netcode` uses `SocketAddr` everywhere, if your transport uses a different 'connection URL'
/// scheme then you can layer the bytes into the [`ConnectToken`](renetcode2::ConnectToken) server address list.
/// Just keep in mind that when a client disconnects, the client will traverse the server address list to find
/// an address to reconnect with. If that isn't supported by your scheme, then when [`ServerSocket::send`] is
/// called with an invalid/unexpected server address you should return an error. If you want to support
/// multiple server addresses but your URLs exceed 16 bytes (IPV6 addresses are 16 bytes), then you should pre-parse
/// the server list from the connect token, and then map that list to the 16-byte IPV6 segments that will be produced
/// by the client when it tries to reconnect to different servers.
pub trait ServerSocket: Debug + Send + Sync + 'static {
    /// Gets the encryption behavior of the socket.
    ///
    /// If the socket internally encrypts packets before sending them, then this should return `true`.
    /// In that case, `renetcode` will not pre-encrypt packets before [`Self::send`] is called.
    fn is_encrypted(&self) -> bool;
    /// Gets the reliability of the socket.
    ///
    /// If this is true, then [`RenetServer`](renet2::RenetServer) will 'downgrade' all channels to
    /// [`SendType::Unreliable`](renet2::SendType::Unreliable) so there is not a redundant reliability layer.
    fn is_reliable(&self) -> bool;

    /// Gets the data source's `SocketAddr`.
    ///
    /// Returns an error if there is no meaningful address. Server sockets should always have an address.
    fn addr(&self) -> std::io::Result<SocketAddr>;

    /// Checks if the data source is closed.
    fn is_closed(&mut self) -> bool;

    /// Closes the data source.
    ///
    /// This should disconnect any remote connections that are being tracked.
    fn close(&mut self);

    /// Notifies the data source that an address's connection request was denied.
    ///
    /// This can be used to manage fresh connections if the internal socket needs to assign resources to
    /// clients. Only connections that are not denied should be assigned resources.
    fn connection_denied(&mut self, addr: SocketAddr);

    /// Notifies the data source that an address's connection request was accepted.
    ///
    /// This can be used to clean up older sessions tied to the same client id.
    fn connection_accepted(&mut self, client_id: u64, addr: SocketAddr);

    /// Disconnects a remote connection with the given address.
    fn disconnect(&mut self, addr: SocketAddr);

    /// Handles data-source-specific logic that must run before receiving packets.
    fn preupdate(&mut self);

    /// Tries to receive the next packet sent to this data source.
    ///
    /// Returns the number of bytes written to the buffer, and the source address of the bytes.
    ///
    /// Should return [`std::io::ErrorKind::WouldBlock`] when no packets are available.
    fn try_recv(&mut self, buffer: &mut [u8]) -> std::io::Result<(usize, SocketAddr)>;

    /// Handles data-source-specific logic that must run after sending packets.
    fn postupdate(&mut self);

    /// Sends a packet to the designated address.
    ///
    /// Should return [`std::io::ErrorKind::ConnectionAborted`] if the destination's connection was closed internally.
    fn send(&mut self, addr: SocketAddr, packet: &[u8]) -> Result<(), NetcodeTransportError>;
}

/// Unreliable data source for use in [`NetcodeClientTransport`](super::NetcodeClientTransport).
///
/// See [`ServerSocket`] also.
pub trait ClientSocket: Debug + Send + Sync + 'static {
    /// Gets the encryption behavior of the socket.
    ///
    /// If the socket internally encrypts packets before sending them, then this should return `true`.
    /// In that case, `renetcode` will not pre-encrypt packets before [`Self::send`] is called.
    ///
    /// Should match the encryption policy of the server socket you will connect to.
    fn is_encrypted(&self) -> bool;
    /// Gets the reliability of the socket.
    ///
    /// If this is true, then [`RenetClient`](renet2::RenetClient) will 'downgrade' all channels to
    /// [`SendType::Unreliable`](renet2::SendType::Unreliable) so there is not a redundant reliability layer.
    ///
    /// Should match the reliability of the server socket you will connect to.
    fn is_reliable(&self) -> bool;

    /// Gets the data source's `SocketAddr`.
    ///
    /// Returns an error if there is no meaningful address. Server sockets should always have an address.
    fn addr(&self) -> std::io::Result<SocketAddr>;

    /// Checks if the data source is closed.
    fn is_closed(&mut self) -> bool;

    /// Closes the data source.
    ///
    /// This should disconnect any remote connections that are being tracked.
    fn close(&mut self);

    /// Handles data-source-specific logic that must run before receiving packets.
    fn preupdate(&mut self);

    /// Tries to receive the next packet sent to this data source.
    ///
    /// Returns the number of bytes written to the buffer, and the source address of the bytes.
    ///
    /// Should return [`std::io::ErrorKind::WouldBlock`] when no packets are available.
    fn try_recv(&mut self, buffer: &mut [u8]) -> std::io::Result<(usize, SocketAddr)>;

    /// Handles data-source-specific logic that must run after sending packets.
    fn postupdate(&mut self);

    /// Sends a packet to the designated address.
    ///
    /// Should return [`std::io::ErrorKind::ConnectionAborted`] if the destination's connection was closed internally.
    fn send(&mut self, addr: SocketAddr, packet: &[u8]) -> Result<(), NetcodeTransportError>;
}
