use std::{io, net::SocketAddr, time::Duration};

use renetcode2::{NetcodeServer, ServerConfig, ServerResult, NETCODE_MAX_PACKET_BYTES, NETCODE_USER_DATA_BYTES};
use renetcode2::{ServerAuthentication, ServerSocketConfig};

use renet2::{ClientId, Payload, RenetServer};

use super::{NetcodeTransportError, ServerSocket};

/// Config for setting up a [`NetcodeServerTransport`].
///
/// Will be converted to a [`ServerConfig`].
pub struct ServerSetupConfig {
    pub current_time: Duration,
    /// Maximum numbers of clients that can be connected at a time
    pub max_clients: usize,
    /// Unique identifier for this particular game/application.
    /// You can use a hash function with the current version of the game to generate this value
    /// so that older versions cannot connect to newer versions.
    pub protocol_id: u64,
    /// Public addresses for each socket associated with this server.
    pub socket_addresses: Vec<Vec<SocketAddr>>,
    /// Authentication configuration for the server
    pub authentication: ServerAuthentication,
}

/// Convenience wrapper for [`ServerSocket`].
///
/// Used in [`NetcodeServerTransport::new_with_sockets`].
pub struct BoxedSocket(Box<dyn ServerSocket>);

impl BoxedSocket {
    /// Makes a new boxed socket.
    pub fn new(socket: impl ServerSocket) -> Self {
        Self(Box::new(socket))
    }
}

#[derive(Debug)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::resource::Resource))]
pub struct NetcodeServerTransport {
    sockets: Vec<Box<dyn ServerSocket>>,
    netcode_server: NetcodeServer,
    buffer: [u8; NETCODE_MAX_PACKET_BYTES],
}

impl NetcodeServerTransport {
    /// Makes a new server transport that uses `netcode` for managing connections and data flow.
    pub fn new(server_config: ServerSetupConfig, socket: impl ServerSocket) -> Result<Self, std::io::Error> {
        Self::new_with_sockets(server_config, vec![BoxedSocket::new(socket)])
    }

    /// Makes a new server transport that uses `netcode` for managing connections and data flow.
    ///
    /// Multiple [`ServerSockets`](ServerSocket) may be inserted. Each socket must line
    /// up 1:1 with socket entries in [`ServerSetupConfig::socket_addresses`].
    pub fn new_with_sockets(mut server_config: ServerSetupConfig, mut boxed: Vec<BoxedSocket>) -> Result<Self, std::io::Error> {
        if server_config.socket_addresses.is_empty() {
            panic!("netcode server transport must have at least 1 socket");
        }
        if server_config.socket_addresses.len() != boxed.len() {
            panic!("server config does not match the number of sockets");
        }

        // Unwrap the boxed sockets so they can be accessed as raw boxes.
        let mut sockets = Vec::with_capacity(boxed.len());
        for socket in boxed.drain(..) {
            sockets.push(socket.0);
        }

        // Transfer config details, use the actual socket impls to determine whether the sockets need netcode encryption.
        let mut socket_configs = Vec::with_capacity(sockets.len());
        let mut socket_addresses = std::mem::take(&mut server_config.socket_addresses);
        for (addrs, socket) in socket_addresses.drain(..).zip(sockets.iter()) {
            socket_configs.push(ServerSocketConfig {
                needs_encryption: !socket.is_encrypted(),
                public_addresses: addrs,
            });
        }

        let server_config = ServerConfig {
            current_time: server_config.current_time,
            max_clients: server_config.max_clients,
            protocol_id: server_config.protocol_id,
            sockets: socket_configs,
            authentication: server_config.authentication,
        };

        Ok(Self {
            sockets,
            netcode_server: NetcodeServer::new(server_config),
            buffer: [0; NETCODE_MAX_PACKET_BYTES],
        })
    }

    /// Returns the server's public addresses for the first transport socket.
    pub fn addresses(&self) -> Vec<SocketAddr> {
        self.get_addresses(0).unwrap()
    }

    /// Returns the server's public addresses for a given `socket_id`.
    pub fn get_addresses(&self, socket_id: usize) -> Option<Vec<SocketAddr>> {
        if socket_id >= self.sockets.len() {
            return None;
        }
        Some(self.netcode_server.addresses(socket_id))
    }

    /// Returns the maximum number of clients that can be connected.
    pub fn max_clients(&self) -> usize {
        self.netcode_server.max_clients()
    }

    /// Update the maximum numbers of clients that can be connected.
    ///
    /// Changing the `max_clients` to a lower value than the current number of connect clients
    /// does not disconnect clients. So [`NetcodeServerTransport::connected_clients()`] can
    /// return a higher value than [`NetcodeServerTransport::max_clients()`].
    pub fn set_max_clients(&mut self, max_clients: usize) {
        self.netcode_server.set_max_clients(max_clients);
    }

    /// Returns current number of clients connected.
    pub fn connected_clients(&self) -> usize {
        self.netcode_server.connected_clients()
    }

    /// Returns the user data for client if connected.
    pub fn user_data(&self, client_id: ClientId) -> Option<[u8; NETCODE_USER_DATA_BYTES]> {
        self.netcode_server.user_data(client_id)
    }

    /// Returns the client socket id and address if connected.
    pub fn client_addr(&self, client_id: ClientId) -> Option<(usize, SocketAddr)> {
        self.netcode_server.client_addr(client_id)
    }

    /// Disconnects all connected clients.
    ///
    /// This sends the disconnect packet instantly, use this when closing/exiting games,
    /// should use [RenetServer::disconnect_all] otherwise.
    pub fn disconnect_all(&mut self, server: &mut RenetServer) {
        for client_id in self.netcode_server.clients_id() {
            let server_result = self.netcode_server.disconnect(client_id);
            handle_server_result(server_result, &mut self.sockets, server);
        }
    }

    /// Returns the duration since the connected client last received a packet.
    ///
    /// Useful to detect users that are timing out.
    pub fn time_since_last_received_packet(&self, client_id: ClientId) -> Option<Duration> {
        self.netcode_server.time_since_last_received_packet(client_id)
    }

    /// Advances the transport by the duration, and receive packets from the network.
    pub fn update(&mut self, duration: Duration, server: &mut RenetServer) -> Result<(), Vec<NetcodeTransportError>> {
        self.netcode_server.update(duration);

        let mut transport_errors = Vec::default();
        for socket_id in 0..self.sockets.len() {
            self.sockets[socket_id].preupdate();

            loop {
                match self.sockets[socket_id].try_recv(&mut self.buffer) {
                    Ok((len, addr)) => {
                        let server_result = self.netcode_server.process_packet(socket_id, addr, &mut self.buffer[..len]);
                        handle_server_result(server_result, &mut self.sockets, server);
                    }
                    Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(ref e) if e.kind() == io::ErrorKind::Interrupted => break,
                    Err(ref e) if e.kind() == io::ErrorKind::ConnectionReset => continue,
                    Err(e) => {
                        transport_errors.push(e.into());
                    }
                };
            }
        }

        for client_id in self.netcode_server.clients_id() {
            let server_result = self.netcode_server.update_client(client_id);
            handle_server_result(server_result, &mut self.sockets, server);
        }

        for disconnection_id in server.disconnections_id() {
            let server_result = self.netcode_server.disconnect(disconnection_id);
            handle_server_result(server_result, &mut self.sockets, server);
        }

        for socket in self.sockets.iter_mut() {
            socket.postupdate();
        }

        if !transport_errors.is_empty() {
            return Err(transport_errors);
        }

        Ok(())
    }

    /// Sends packets to connected clients.
    pub fn send_packets(&mut self, server: &mut RenetServer) {
        //TODO: it isn't necessary to allocate client ids here, just use one big vec of packets for all clients
        // - also, the vec can be cached in RenetServer for reuse, and likewise with the internal pieces of packets
        for client_id in server.clients_id() {
            let packets = server.get_packets_to_send(client_id).unwrap();
            for packet in packets {
                if !send_packet_to_client(&mut self.sockets, &mut self.netcode_server, server, &packet, client_id) {
                    break;
                }
            }
        }
    }
}

/// Sends a packet to a client.
///
/// Disconnects the client if its address connection is broken.
fn send_packet_to_client(
    sockets: &mut [Box<dyn ServerSocket>],
    netcode_server: &mut NetcodeServer,
    reliable_server: &mut RenetServer,
    packet: &Payload,
    client_id: ClientId,
) -> bool {
    let (send_result, socket_id, addr) = match netcode_server.generate_payload_packet(client_id, packet) {
        Ok((socket_id, addr, payload)) => (sockets[socket_id].send(addr, payload), socket_id, addr),
        Err(e) => {
            log::error!("Failed to encrypt payload packet for client {client_id}: {e}");
            return false;
        }
    };

    match send_result {
        Ok(()) => true,
        Err(NetcodeTransportError::IO(ref e)) if e.kind() == io::ErrorKind::ConnectionAborted => {
            // Manually disconnect the client if the client's address is disconnected.
            reliable_server.remove_connection(client_id);
            // Ignore the server result since this client is not connected.
            let _ = netcode_server.disconnect(client_id);

            false
        }
        Err(e) => {
            log::error!("Failed to send packet to client {client_id} ({socket_id}/{addr}): {e}");
            false
        }
    }
}

fn handle_server_result(server_result: ServerResult, sockets: &mut [Box<dyn ServerSocket>], reliable_server: &mut RenetServer) {
    let send_packet = |sockets: &mut [Box<dyn ServerSocket>], packet: &[u8], socket_id: usize, addr: SocketAddr| {
        if let Err(err) = sockets[socket_id].send(addr, packet) {
            log::trace!("Failed to send packet to {socket_id}/{addr}: {err}");
        }
    };

    match server_result {
        ServerResult::None => {}
        ServerResult::Error { addr, socket_id } => {
            sockets[socket_id].disconnect(addr);
        }
        ServerResult::ConnectionDenied { addr, socket_id, payload } => {
            if let Some(payload) = payload {
                send_packet(sockets, payload, socket_id, addr);
            }
            sockets[socket_id].connection_denied(addr);
        }
        ServerResult::ConnectionAccepted {
            client_id,
            addr,
            socket_id,
            payload,
        } => {
            sockets[socket_id].connection_accepted(client_id, addr);
            send_packet(sockets, payload, socket_id, addr);
        }
        ServerResult::PacketToSend { payload, addr, socket_id } => {
            send_packet(sockets, payload, socket_id, addr);
        }
        ServerResult::Payload { client_id, payload } => {
            if let Err(e) = reliable_server.process_packet_from(payload, client_id) {
                log::error!("Error while processing payload for {}: {}", client_id, e);
            }
        }
        ServerResult::ClientConnected {
            client_id,
            user_data: _,
            addr,
            payload,
            socket_id,
        } => {
            reliable_server.add_connection(client_id, sockets[socket_id].is_reliable());
            send_packet(sockets, payload, socket_id, addr);
        }
        ServerResult::ClientDisconnected {
            client_id,
            addr,
            payload,
            socket_id,
        } => {
            reliable_server.remove_connection(client_id);
            if let Some(payload) = payload {
                send_packet(sockets, payload, socket_id, addr);
            }
            sockets[socket_id].disconnect(addr);
        }
    }
}
