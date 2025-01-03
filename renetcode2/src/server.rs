use std::{collections::HashMap, net::SocketAddr, time::Duration};

use crate::{
    crypto::generate_random_bytes,
    packet::{ChallengeToken, Packet},
    replay_protection::ReplayProtection,
    token::PrivateConnectToken,
    NetcodeError, NETCODE_CONNECT_TOKEN_PRIVATE_BYTES, NETCODE_CONNECT_TOKEN_XNONCE_BYTES, NETCODE_KEY_BYTES, NETCODE_MAC_BYTES,
    NETCODE_MAX_CLIENTS, NETCODE_MAX_PACKET_BYTES, NETCODE_MAX_PAYLOAD_BYTES, NETCODE_MAX_PENDING_CLIENTS, NETCODE_SEND_RATE,
    NETCODE_USER_DATA_BYTES, NETCODE_VERSION_INFO,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionState {
    Disconnected,
    PendingResponse,
    Connected,
}

#[derive(Debug, Clone)]
struct Connection {
    confirmed: bool,
    client_id: u64,
    state: ConnectionState,
    send_key: [u8; NETCODE_KEY_BYTES],
    receive_key: [u8; NETCODE_KEY_BYTES],
    user_data: [u8; NETCODE_USER_DATA_BYTES],
    socket_id: usize,
    addr: SocketAddr,
    last_packet_received_time: Duration,
    last_packet_send_time: Duration,
    timeout_seconds: i32,
    sequence: u64,
    expire_timestamp: u64,
    replay_protection: ReplayProtection,
}

#[derive(Debug, Copy, Clone)]
struct ConnectTokenEntry {
    time: Duration,
    socket_id: usize,
    address: SocketAddr,
    mac: [u8; NETCODE_MAC_BYTES],
}

/// A server that can generate packets from connect clients, that are encrypted, or process
/// incoming encrypted packets from clients. The server is agnostic from the transport layer, only
/// consuming and generating bytes that can be transported in any way desired.
#[derive(Debug)]
pub struct NetcodeServer {
    sockets: Vec<ServerSocketConfig>,
    clients: Box<[Option<Connection>]>,
    pending_clients: HashMap<(usize, SocketAddr), Connection>,
    connect_token_entries: Box<[Option<ConnectTokenEntry>; NETCODE_MAX_CLIENTS * 2]>,
    protocol_id: u64,
    connect_key: [u8; NETCODE_KEY_BYTES],
    max_clients: usize,
    challenge_sequence: u64,
    challenge_key: [u8; NETCODE_KEY_BYTES],
    current_time: Duration,
    global_sequence: u64,
    secure: bool,
    out: [u8; NETCODE_MAX_PACKET_BYTES],
}

/// Result from processing an packet in the server
#[derive(Debug, PartialEq, Eq)]
pub enum ServerResult<'a, 's> {
    /// Nothing needs to be done.
    None,
    /// An error occurred while processing the packet, the address should be rejected.
    Error { socket_id: usize, addr: SocketAddr },
    /// A connection request was valid but denied because of connection limits or a token already in use.
    ///
    /// If there is a payload it should be sent to the address.
    ConnectionDenied {
        socket_id: usize,
        addr: SocketAddr,
        payload: Option<&'s mut [u8]>,
    },
    /// A connection request was accepted.
    ///
    /// The payload should be sent to the address.
    ConnectionAccepted {
        client_id: u64,
        socket_id: usize,
        addr: SocketAddr,
        payload: &'s mut [u8],
    },
    /// A packet to be sent back to the processed address.
    PacketToSend {
        socket_id: usize,
        addr: SocketAddr,
        payload: &'s mut [u8],
    },
    /// A payload received from the client.
    Payload { client_id: u64, payload: &'a [u8] },
    /// A new client has connected
    ClientConnected {
        client_id: u64,
        socket_id: usize,
        addr: SocketAddr,
        user_data: Box<[u8; NETCODE_USER_DATA_BYTES]>,
        payload: &'s mut [u8],
    },
    /// The client connection has been terminated.
    ClientDisconnected {
        client_id: u64,
        socket_id: usize,
        addr: SocketAddr,
        payload: Option<&'s mut [u8]>,
    },
}

/// Configuration details for a socket associated with a netcode server.
#[derive(Debug)]
pub struct ServerSocketConfig {
    /// If `true` then netcode packets sent/received to/from this socket will be encrypted/decrypted.
    ///
    /// `true` by default.
    pub needs_encryption: bool,
    /// Publicly available addresses to which clients will attempt to connect.
    pub public_addresses: Vec<SocketAddr>,
}

impl ServerSocketConfig {
    /// Makes a new config with default settings.
    pub fn new(public_addresses: Vec<SocketAddr>) -> Self {
        Self {
            needs_encryption: true,
            public_addresses,
        }
    }
}

/// Configuration to establish a secure or unsecure connection with the server.
pub enum ServerAuthentication {
    /// Establishes a safe connection using a private key for encryption. The private key cannot be
    /// shared with the client. Connections are stablished using [crate::token::ConnectToken].
    ///
    /// See also [ClientAuthentication::Secure][crate::ClientAuthentication::Secure]
    Secure { private_key: [u8; NETCODE_KEY_BYTES] },
    /// Establishes unsafe connections with clients, useful for testing and prototyping.
    ///
    /// See also [ClientAuthentication::Unsecure][crate::ClientAuthentication::Unsecure]
    Unsecure,
}

pub struct ServerConfig {
    pub current_time: Duration,
    /// Maximum numbers of clients that can be connected at a time
    pub max_clients: usize,
    /// Unique identifier for this particular game/application.
    /// You can use a hash function with the current version of the game to generate this value
    /// so that older versions cannot connect to newer versions.
    pub protocol_id: u64,
    /// Settings for sockets associated with this server.
    pub sockets: Vec<ServerSocketConfig>,
    /// Authentication configuration for the server
    pub authentication: ServerAuthentication,
}

impl NetcodeServer {
    pub fn new(config: ServerConfig) -> Self {
        if config.sockets.is_empty() {
            panic!("Cannot make a server with no sockets.");
        }
        if config.max_clients > NETCODE_MAX_CLIENTS {
            // TODO: do we really need to set a max?
            //       only using for token entries
            panic!("The max clients allowed is {}", NETCODE_MAX_CLIENTS);
        }
        let challenge_key = generate_random_bytes();
        let clients = vec![None; config.max_clients].into_boxed_slice();

        let connect_key = match config.authentication {
            ServerAuthentication::Unsecure => [0; NETCODE_KEY_BYTES],
            ServerAuthentication::Secure { private_key } => private_key,
        };

        let secure = match config.authentication {
            ServerAuthentication::Unsecure => false,
            ServerAuthentication::Secure { .. } => true,
        };

        Self {
            sockets: config.sockets,
            clients,
            connect_token_entries: Box::new([None; NETCODE_MAX_CLIENTS * 2]),
            pending_clients: HashMap::new(),
            protocol_id: config.protocol_id,
            connect_key,
            max_clients: config.max_clients,
            challenge_sequence: 0,
            global_sequence: 0,
            challenge_key,
            current_time: config.current_time,
            secure,
            out: [0u8; NETCODE_MAX_PACKET_BYTES],
        }
    }

    #[doc(hidden)]
    pub fn __test() -> Self {
        let config = ServerConfig {
            current_time: Duration::ZERO,
            max_clients: 32,
            protocol_id: 0,
            sockets: vec![ServerSocketConfig::new(vec!["127.0.0.1:0".parse().unwrap()])],
            authentication: ServerAuthentication::Unsecure,
        };
        Self::new(config)
    }

    /// Gets the public addresses of a specific socket.
    ///
    /// Panics if `socket_id` is out of range.
    pub fn addresses(&self, socket_id: usize) -> Vec<SocketAddr> {
        self.sockets[socket_id].public_addresses.clone()
    }

    pub fn current_time(&self) -> Duration {
        self.current_time
    }

    fn find_or_add_connect_token_entry(&mut self, new_entry: ConnectTokenEntry) -> bool {
        let mut min = Duration::MAX;
        let mut oldest_entry = 0;
        let mut empty_entry = false;
        let mut matching_entry = None;
        for (i, entry) in self.connect_token_entries.iter().enumerate() {
            match entry {
                Some(e) => {
                    if e.mac == new_entry.mac {
                        matching_entry = Some(e);
                    }
                    if !empty_entry && e.time < min {
                        oldest_entry = i;
                        min = e.time;
                    }
                }
                None => {
                    if !empty_entry {
                        empty_entry = true;
                        oldest_entry = i;
                    }
                }
            }
        }

        if let Some(entry) = matching_entry {
            return (entry.socket_id == new_entry.socket_id) && (entry.address == new_entry.address);
        }

        self.connect_token_entries[oldest_entry] = Some(new_entry);

        true
    }

    /// Returns the user data from the connected client.
    pub fn user_data(&self, client_id: u64) -> Option<[u8; NETCODE_USER_DATA_BYTES]> {
        if let Some(client) = find_client_by_id(&self.clients, client_id) {
            return Some(client.user_data);
        }

        None
    }

    /// Returns the duration since the connected client last received a packet.
    /// Usefull to detect users that are timing out.
    pub fn time_since_last_received_packet(&self, client_id: u64) -> Option<Duration> {
        if let Some(client) = find_client_by_id(&self.clients, client_id) {
            let time = self.current_time - client.last_packet_received_time;
            return Some(time);
        }

        None
    }

    /// Returns the client socket id and address if connected.
    pub fn client_addr(&self, client_id: u64) -> Option<(usize, SocketAddr)> {
        if let Some(client) = find_client_by_id(&self.clients, client_id) {
            return Some((client.socket_id, client.addr));
        }

        None
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_connection_request<'a>(
        &mut self,
        socket_id: usize,
        addr: SocketAddr,
        version_info: [u8; 13],
        protocol_id: u64,
        expire_timestamp: u64,
        xnonce: [u8; NETCODE_CONNECT_TOKEN_XNONCE_BYTES],
        data: [u8; NETCODE_CONNECT_TOKEN_PRIVATE_BYTES],
    ) -> Result<ServerResult<'a, '_>, NetcodeError> {
        if version_info != *NETCODE_VERSION_INFO {
            return Err(NetcodeError::InvalidVersion);
        }

        if protocol_id != self.protocol_id {
            return Err(NetcodeError::InvalidProtocolID);
        }

        if self.current_time.as_secs() >= expire_timestamp {
            return Err(NetcodeError::Expired);
        }

        let connect_token = PrivateConnectToken::decode(&data, self.protocol_id, expire_timestamp, &xnonce, &self.connect_key)?;

        if socket_id >= self.sockets.len() {
            return Err(NetcodeError::InvalidSocketId);
        }
        if socket_id != connect_token.socket_id as usize {
            return Err(NetcodeError::InvalidSocketId);
        }

        // Skip host list check when unsecure
        if self.secure {
            let in_host_list = connect_token
                .server_addresses
                .iter()
                .filter_map(|host| *host)
                .any(|addr| self.sockets[socket_id].public_addresses.contains(&addr));

            if !in_host_list {
                return Err(NetcodeError::NotInHostList);
            }
        }

        if let Some((_, connection)) = find_client_mut_by_addr(&mut self.clients, socket_id, addr) {
            // This branch should be unreachable since connection requests are ignored for already-connected addresses.

            if connection.client_id == connect_token.client_id {
                log::debug!(
                    "Connection request ignored: client {} already connected (socket id: {}, address: {}).",
                    connection.client_id,
                    socket_id,
                    addr
                );

                return Ok(ServerResult::None);
            } else {
                log::debug!(
                    "Connection request denied: (socket id: {}, address: {}) tried connecting as client {} but is already \
                    client {}.",
                    socket_id,
                    addr,
                    connect_token.client_id,
                    connection.client_id,
                );

                return Ok(ServerResult::ConnectionDenied {
                    addr,
                    socket_id,
                    payload: None,
                });
            }
        } else if let Some(connection) = find_client_mut_by_id(&mut self.clients, connect_token.client_id) {
            log::debug!(
                "Connection request denied: (socket id: {}, address: {}) tried connecting as client {} but a different \
                address (socket id: {}, address: {}) is connected as that client.",
                socket_id,
                addr,
                connect_token.client_id,
                connection.socket_id,
                connection.addr,
            );
            return Ok(ServerResult::ConnectionDenied {
                addr,
                socket_id,
                payload: None,
            });
        }

        if !self.pending_clients.contains_key(&(socket_id, addr)) && self.pending_clients.len() >= NETCODE_MAX_PENDING_CLIENTS {
            log::warn!(
                "Connection request denied: reached max amount allowed of pending clients ({}).",
                NETCODE_MAX_PENDING_CLIENTS
            );
            return Ok(ServerResult::ConnectionDenied {
                addr,
                socket_id,
                payload: None,
            });
        }

        let mut mac = [0u8; NETCODE_MAC_BYTES];
        mac.copy_from_slice(&data[NETCODE_CONNECT_TOKEN_PRIVATE_BYTES - NETCODE_MAC_BYTES..]);
        let connect_token_entry = ConnectTokenEntry {
            socket_id,
            address: addr,
            time: self.current_time,
            mac,
        };

        if !self.find_or_add_connect_token_entry(connect_token_entry) {
            log::warn!("Connection request denied: connect token already has an entry for a different address");
            return Ok(ServerResult::ConnectionDenied {
                addr,
                socket_id,
                payload: None,
            });
        }

        if self.clients.iter().flatten().count() >= self.max_clients {
            self.pending_clients.remove(&(socket_id, addr));
            let packet = Packet::ConnectionDenied;
            let len = packet.encode(
                &mut self.out,
                self.protocol_id,
                Some((self.global_sequence, &connect_token.server_to_client_key)),
                self.sockets[socket_id].needs_encryption,
            )?;
            self.global_sequence += 1;
            return Ok(ServerResult::ConnectionDenied {
                socket_id,
                addr,
                payload: Some(&mut self.out[..len]),
            });
        }

        self.challenge_sequence += 1;
        let packet = Packet::generate_challenge(
            connect_token.client_id,
            &connect_token.user_data,
            self.challenge_sequence,
            &self.challenge_key,
        )?;

        let len = packet.encode(
            &mut self.out,
            self.protocol_id,
            Some((self.global_sequence, &connect_token.server_to_client_key)),
            self.sockets[socket_id].needs_encryption,
        )?;
        self.global_sequence += 1;

        log::trace!("Connection request from Client {}", connect_token.client_id);

        let pending = self.pending_clients.entry((socket_id, addr)).or_insert_with(|| Connection {
            confirmed: false,
            sequence: 0,
            client_id: connect_token.client_id,
            last_packet_received_time: self.current_time,
            last_packet_send_time: self.current_time,
            socket_id,
            addr,
            state: ConnectionState::PendingResponse,
            send_key: connect_token.server_to_client_key,
            receive_key: connect_token.client_to_server_key,
            timeout_seconds: connect_token.timeout_seconds,
            expire_timestamp,
            user_data: connect_token.user_data,
            replay_protection: ReplayProtection::new(),
        });
        pending.last_packet_received_time = self.current_time;
        pending.last_packet_send_time = self.current_time;

        Ok(ServerResult::ConnectionAccepted {
            client_id: connect_token.client_id,
            socket_id,
            addr,
            payload: &mut self.out[..len],
        })
    }

    /// Returns an encoded packet payload to be sent to the client.
    pub fn generate_payload_packet<'s>(
        &'s mut self,
        client_id: u64,
        payload: &[u8],
    ) -> Result<(usize, SocketAddr, &'s mut [u8]), NetcodeError> {
        if payload.len() > NETCODE_MAX_PAYLOAD_BYTES {
            return Err(NetcodeError::PayloadAboveLimit);
        }

        if let Some(client) = find_client_mut_by_id(&mut self.clients, client_id) {
            let packet = Packet::Payload(payload);
            let len = packet.encode(
                &mut self.out,
                self.protocol_id,
                Some((client.sequence, &client.send_key)),
                self.sockets[client.socket_id].needs_encryption,
            )?;
            client.sequence += 1;
            client.last_packet_send_time = self.current_time;

            return Ok((client.socket_id, client.addr, &mut self.out[..len]));
        }

        Err(NetcodeError::ClientNotFound)
    }

    /// Process an packet from the especifed address. Returns a server result, check out
    /// [ServerResult].
    pub fn process_packet<'a, 's>(&'s mut self, socket_id: usize, addr: SocketAddr, buffer: &'a mut [u8]) -> ServerResult<'a, 's> {
        match self.process_packet_internal(socket_id, addr, buffer) {
            Err(e) => {
                log::error!("Failed to process packet: {}", e);
                ServerResult::Error { socket_id, addr }
            }
            Ok(r) => r,
        }
    }

    fn process_packet_internal<'a, 's>(
        &'s mut self,
        socket_id: usize,
        addr: SocketAddr,
        buffer: &'a mut [u8],
    ) -> Result<ServerResult<'a, 's>, NetcodeError> {
        if buffer.len() < 2 + NETCODE_MAC_BYTES {
            return Err(NetcodeError::PacketTooSmall);
        }

        // Handle connected client
        if let Some((slot, client)) = find_client_mut_by_addr(&mut self.clients, socket_id, addr) {
            let (_, packet) = Packet::decode(
                buffer,
                self.protocol_id,
                Some(&client.receive_key),
                Some(&mut client.replay_protection),
                self.sockets[socket_id].needs_encryption,
            )?;
            log::trace!(
                "Received packet from connected client ({}): {:?}",
                client.client_id,
                packet.packet_type()
            );

            client.last_packet_received_time = self.current_time;
            match client.state {
                ConnectionState::Connected => match packet {
                    Packet::Disconnect => {
                        client.state = ConnectionState::Disconnected;
                        let client_id = client.client_id;
                        self.clients[slot] = None;
                        log::trace!("Client {} requested to disconnect", client_id);
                        return Ok(ServerResult::ClientDisconnected {
                            client_id,
                            socket_id,
                            addr,
                            payload: None,
                        });
                    }
                    Packet::Payload(payload) => {
                        if !client.confirmed {
                            log::trace!("Confirmed connection for Client {}", client.client_id);
                            client.confirmed = true;
                        }
                        return Ok(ServerResult::Payload {
                            client_id: client.client_id,
                            payload,
                        });
                    }
                    Packet::KeepAlive { .. } => {
                        if !client.confirmed {
                            log::trace!("Confirmed connection for Client {}", client.client_id);
                            client.confirmed = true;
                        }
                        return Ok(ServerResult::None);
                    }
                    _ => return Ok(ServerResult::None),
                },
                _ => return Ok(ServerResult::None),
            }
        }

        // Handle pending client
        if let Some(pending) = self.pending_clients.get_mut(&(socket_id, addr)) {
            let (_, packet) = Packet::decode(
                buffer,
                self.protocol_id,
                Some(&pending.receive_key),
                Some(&mut pending.replay_protection),
                self.sockets[socket_id].needs_encryption,
            )?;
            pending.last_packet_received_time = self.current_time;
            log::trace!("Received packet from pending client ({}): {:?}", addr, packet.packet_type());
            match packet {
                Packet::ConnectionRequest {
                    protocol_id,
                    expire_timestamp,
                    data,
                    xnonce,
                    version_info,
                } => {
                    return self.handle_connection_request(socket_id, addr, version_info, protocol_id, expire_timestamp, xnonce, data);
                }
                Packet::Response {
                    token_data,
                    token_sequence,
                } => {
                    let challenge_token = ChallengeToken::decode(token_data, token_sequence, &self.challenge_key)?;
                    let mut pending = self.pending_clients.remove(&(socket_id, addr)).unwrap();
                    if find_client_slot_by_id(&self.clients, challenge_token.client_id).is_some() {
                        log::debug!(
                            "Ignored connection response for Client {}, already connected.",
                            challenge_token.client_id
                        );
                        return Ok(ServerResult::None);
                    }
                    match self.clients.iter().position(|c| c.is_none()) {
                        None => {
                            let packet = Packet::ConnectionDenied;
                            let len = packet.encode(
                                &mut self.out,
                                self.protocol_id,
                                Some((self.global_sequence, &pending.send_key)),
                                self.sockets[socket_id].needs_encryption,
                            )?;
                            pending.state = ConnectionState::Disconnected;
                            self.global_sequence += 1;
                            pending.last_packet_send_time = self.current_time;
                            return Ok(ServerResult::ConnectionDenied {
                                socket_id,
                                addr,
                                payload: Some(&mut self.out[..len]),
                            });
                        }
                        Some(client_index) => {
                            pending.state = ConnectionState::Connected;
                            pending.user_data = challenge_token.user_data;
                            pending.last_packet_send_time = self.current_time;

                            let packet = Packet::KeepAlive {
                                max_clients: self.max_clients as u32,
                                client_index: client_index as u32,
                            };
                            let len = packet.encode(
                                &mut self.out,
                                self.protocol_id,
                                Some((pending.sequence, &pending.send_key)),
                                self.sockets[socket_id].needs_encryption,
                            )?;
                            pending.sequence += 1;

                            let client_id: u64 = pending.client_id;
                            let user_data: [u8; NETCODE_USER_DATA_BYTES] = pending.user_data;
                            self.clients[client_index] = Some(pending);

                            return Ok(ServerResult::ClientConnected {
                                client_id,
                                socket_id,
                                addr,
                                user_data: Box::new(user_data),
                                payload: &mut self.out[..len],
                            });
                        }
                    }
                }
                _ => return Ok(ServerResult::None),
            }
        }

        // Handle new client
        let (_, packet) = Packet::decode(buffer, self.protocol_id, None, None, self.sockets[socket_id].needs_encryption)?;
        match packet {
            Packet::ConnectionRequest {
                data,
                protocol_id,
                expire_timestamp,
                xnonce,
                version_info,
            } => self.handle_connection_request(socket_id, addr, version_info, protocol_id, expire_timestamp, xnonce, data),
            _ => unreachable!("Decoding packet without key can only return ConnectionRequest packets"),
        }
    }

    pub fn clients_slot(&self) -> Vec<usize> {
        self.clients
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| if slot.is_some() { Some(index) } else { None })
            .collect()
    }

    /// Returns the ids from the connected clients (iterator).
    pub fn clients_id_iter(&self) -> impl Iterator<Item = u64> + '_ {
        self.clients.iter().filter_map(|slot| slot.as_ref().map(|client| client.client_id))
    }

    /// Returns the ids from the connected clients.
    pub fn clients_id(&self) -> Vec<u64> {
        self.clients_id_iter().collect()
    }

    /// Returns the maximum number of clients that can be connected.
    pub fn max_clients(&self) -> usize {
        self.max_clients
    }

    /// Update the maximum numbers of clients that can be connected
    ///
    /// Changing the `max_clients` to a lower value than the current number of connect clients
    /// does not disconnect clients. So [`NetcodeServer::connected_clients()`] can return a
    /// higher value than [`NetcodeServer::max_clients()`].
    pub fn set_max_clients(&mut self, max_clients: usize) {
        let max_clients = max_clients.min(NETCODE_MAX_CLIENTS);
        log::debug!("Netcode max_clients set to {}", max_clients);

        self.max_clients = max_clients;
    }

    /// Returns current number of clients connected.
    pub fn connected_clients(&self) -> usize {
        self.clients.iter().filter(|slot| slot.is_some()).count()
    }

    /// Advance the server current time, and remove any pending connections that have expired.
    pub fn update(&mut self, duration: Duration) {
        self.current_time += duration;

        for client in self.pending_clients.values_mut() {
            if self.current_time.as_secs() > client.expire_timestamp {
                log::debug!("Pending Client {} disconnected, connection token expired.", client.client_id);
                client.state = ConnectionState::Disconnected;
            }
        }

        self.pending_clients.retain(|_, c| c.state != ConnectionState::Disconnected);
    }

    /// Updates the client, returns a ServerResult.
    ///
    /// # Example
    /// ```
    /// # use renetcode2::ServerResult;
    /// # let mut server = renetcode2::NetcodeServer::__test();
    /// for client_id in server.clients_id().into_iter() {
    ///     match server.update_client(client_id) {
    ///         ServerResult::PacketToSend { payload, socket_id, addr } => send_to(payload, socket_id, addr),
    ///         _ => { /* ... */ }
    ///     }
    /// }
    /// # fn send_to(p: &[u8], socket_id: usize, addr: std::net::SocketAddr) {}
    /// ```
    pub fn update_client(&mut self, client_id: u64) -> ServerResult<'_, '_> {
        let slot = match find_client_slot_by_id(&self.clients, client_id) {
            None => return ServerResult::None,
            Some(slot) => slot,
        };

        if let Some(client) = &mut self.clients[slot] {
            let connection_timed_out = client.timeout_seconds > 0
                && (client.last_packet_received_time + Duration::from_secs(client.timeout_seconds as u64) < self.current_time);
            if connection_timed_out {
                log::debug!("Client {} disconnected, connection timed out", client.client_id);
                client.state = ConnectionState::Disconnected;
            }
            let socket_id = client.socket_id;

            if client.state == ConnectionState::Disconnected {
                let packet = Packet::Disconnect;
                let sequence = client.sequence;
                let send_key = client.send_key;
                let addr = client.addr;
                self.clients[slot] = None;

                let len = match packet.encode(
                    &mut self.out,
                    self.protocol_id,
                    Some((sequence, &send_key)),
                    self.sockets[socket_id].needs_encryption,
                ) {
                    Err(e) => {
                        log::error!("Failed to encode disconnect packet: {}", e);
                        return ServerResult::ClientDisconnected {
                            client_id,
                            socket_id,
                            addr,
                            payload: None,
                        };
                    }
                    Ok(len) => len,
                };

                return ServerResult::ClientDisconnected {
                    client_id,
                    socket_id,
                    addr,
                    payload: Some(&mut self.out[..len]),
                };
            }

            if client.last_packet_send_time + NETCODE_SEND_RATE <= self.current_time {
                let packet = Packet::KeepAlive {
                    client_index: slot as u32,
                    max_clients: self.max_clients as u32,
                };

                let len = match packet.encode(
                    &mut self.out,
                    self.protocol_id,
                    Some((client.sequence, &client.send_key)),
                    self.sockets[socket_id].needs_encryption,
                ) {
                    Err(e) => {
                        log::error!("Failed to encode keep alive packet: {}", e);
                        return ServerResult::None;
                    }
                    Ok(len) => len,
                };
                client.sequence += 1;
                client.last_packet_send_time = self.current_time;
                return ServerResult::PacketToSend {
                    socket_id,
                    addr: client.addr,
                    payload: &mut self.out[..len],
                };
            }
        }

        ServerResult::None
    }

    pub fn is_client_connected(&self, client_id: u64) -> bool {
        find_client_slot_by_id(&self.clients, client_id).is_some()
    }

    /// Disconnect an client and returns its address and a disconnect packet to be sent to them.
    // TODO: we can return Result<PacketToSend, NetcodeError>
    //       but the library user would need to be aware that he has to run
    //       the same code as Result::ClientDisconnected
    pub fn disconnect(&mut self, client_id: u64) -> ServerResult<'_, '_> {
        if let Some(slot) = find_client_slot_by_id(&self.clients, client_id) {
            let client = self.clients[slot].take().unwrap();
            let packet = Packet::Disconnect;

            let len = match packet.encode(
                &mut self.out,
                self.protocol_id,
                Some((client.sequence, &client.send_key)),
                self.sockets[client.socket_id].needs_encryption,
            ) {
                Err(e) => {
                    log::error!("Failed to encode disconnect packet: {}", e);
                    return ServerResult::ClientDisconnected {
                        client_id,
                        socket_id: client.socket_id,
                        addr: client.addr,
                        payload: None,
                    };
                }
                Ok(len) => len,
            };
            return ServerResult::ClientDisconnected {
                client_id,
                socket_id: client.socket_id,
                addr: client.addr,
                payload: Some(&mut self.out[..len]),
            };
        }

        ServerResult::None
    }
}

fn find_client_mut_by_id(clients: &mut [Option<Connection>], client_id: u64) -> Option<&mut Connection> {
    clients.iter_mut().flatten().find(|c| c.client_id == client_id)
}

fn find_client_by_id(clients: &[Option<Connection>], client_id: u64) -> Option<&Connection> {
    clients.iter().flatten().find(|c| c.client_id == client_id)
}

fn find_client_slot_by_id(clients: &[Option<Connection>], client_id: u64) -> Option<usize> {
    clients.iter().enumerate().find_map(|(i, c)| match c {
        Some(c) if c.client_id == client_id => Some(i),
        _ => None,
    })
}

fn find_client_mut_by_addr(clients: &mut [Option<Connection>], socket_id: usize, addr: SocketAddr) -> Option<(usize, &mut Connection)> {
    clients.iter_mut().enumerate().find_map(|(i, c)| match c {
        Some(c) if (c.socket_id == socket_id) && (c.addr == addr) => Some((i, c)),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use crate::{client::NetcodeClient, token::ConnectToken, ClientAuthentication};

    use super::*;

    const TEST_KEY: &[u8; NETCODE_KEY_BYTES] = b"an example very very secret key."; // 32-bytes
    const TEST_PROTOCOL_ID: u64 = 7;

    fn new_server() -> NetcodeServer {
        let config = ServerConfig {
            current_time: Duration::ZERO,
            max_clients: 16,
            protocol_id: TEST_PROTOCOL_ID,
            sockets: vec![ServerSocketConfig::new(vec!["127.0.0.1:5000".parse().unwrap()])],
            authentication: ServerAuthentication::Secure { private_key: *TEST_KEY },
        };
        NetcodeServer::new(config)
    }

    #[test]
    fn server_connection() {
        let mut server = new_server();
        let server_addresses: Vec<SocketAddr> = server.addresses(0);
        let user_data = generate_random_bytes();
        let expire_seconds = 3;
        let client_id = 4;
        let timeout_seconds = 5;
        let client_addr: SocketAddr = "127.0.0.1:3000".parse().unwrap();
        let connect_token = ConnectToken::generate(
            Duration::ZERO,
            TEST_PROTOCOL_ID,
            expire_seconds,
            client_id,
            timeout_seconds,
            0,
            server_addresses,
            Some(&user_data),
            TEST_KEY,
        )
        .unwrap();
        let client_auth = ClientAuthentication::Secure { connect_token };
        let mut client = NetcodeClient::new(Duration::ZERO, client_auth).unwrap();
        let (client_packet, _) = client.update(Duration::ZERO).unwrap();

        let result = server.process_packet(0, client_addr, client_packet);
        assert!(matches!(result, ServerResult::ConnectionAccepted { .. }));
        match result {
            ServerResult::ConnectionAccepted { payload, .. } => client.process_packet(payload),
            _ => unreachable!(),
        };

        assert!(!client.is_connected());
        let (client_packet, _) = client.update(Duration::ZERO).unwrap();
        let result = server.process_packet(0, client_addr, client_packet);

        match result {
            ServerResult::ClientConnected {
                socket_id,
                client_id: r_id,
                user_data: r_data,
                payload,
                ..
            } => {
                assert_eq!(socket_id, 0);
                assert_eq!(client_id, r_id);
                assert_eq!(user_data, *r_data);
                client.process_packet(payload)
            }
            _ => unreachable!(),
        };

        assert!(client.is_connected());

        for _ in 0..3 {
            let payload = [7u8; 300];
            let (_, _, packet) = server.generate_payload_packet(client_id, &payload).unwrap();
            let result_payload = client.process_packet(packet).unwrap();
            assert_eq!(payload, result_payload);
        }

        let result = server.update_client(client_id);
        assert_eq!(result, ServerResult::None);
        server.update(NETCODE_SEND_RATE);

        let result = server.update_client(client_id);
        match result {
            ServerResult::PacketToSend { payload, .. } => {
                assert!(client.process_packet(payload).is_none());
            }
            _ => unreachable!(),
        }

        let client_payload = [2u8; 300];
        let (_, packet) = client.generate_payload_packet(&client_payload).unwrap();

        match server.process_packet(0, client_addr, packet) {
            ServerResult::Payload { client_id: id, payload } => {
                assert_eq!(id, client_id);
                assert_eq!(client_payload, payload);
            }
            _ => unreachable!(),
        }

        assert!(server.is_client_connected(client_id));
        let result = server.disconnect(client_id);
        match result {
            ServerResult::ClientDisconnected {
                payload: Some(payload), ..
            } => {
                assert!(client.is_connected());
                assert!(client.process_packet(payload).is_none());
                assert!(!client.is_connected());
            }
            _ => unreachable!(),
        }

        assert!(!server.is_client_connected(client_id));
    }

    #[test]
    fn connect_token_already_used() {
        let mut server = new_server();

        let client_addr: SocketAddr = "127.0.0.1:3000".parse().unwrap();
        let mut connect_token = ConnectTokenEntry {
            time: Duration::ZERO,
            socket_id: 0,
            address: client_addr,
            mac: generate_random_bytes(),
        };
        // Allow first entry
        assert!(server.find_or_add_connect_token_entry(connect_token));
        // Allow same token with the same address
        assert!(server.find_or_add_connect_token_entry(connect_token));

        // Don't allow same token with different socket id
        connect_token.socket_id = 1;
        assert!(!server.find_or_add_connect_token_entry(connect_token));
        connect_token.socket_id = 0;

        // Don't allow same token with different address
        connect_token.address = "127.0.0.1:3001".parse().unwrap();
        assert!(!server.find_or_add_connect_token_entry(connect_token));
    }
}
