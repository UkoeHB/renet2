use std::{io, net::SocketAddr, time::Duration};

use renetcode2::{ClientAuthentication, DisconnectReason, NetcodeClient, NetcodeError, NETCODE_MAX_PACKET_BYTES};

use renet2::{ClientId, RenetClient};

use super::{ClientSocket, NetcodeTransportError};

#[derive(Debug)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::resource::Resource))]
pub struct NetcodeClientTransport {
    socket: Box<dyn ClientSocket>,
    netcode_client: NetcodeClient,
    buffer: [u8; NETCODE_MAX_PACKET_BYTES],
}

impl NetcodeClientTransport {
    /// Makes a new client transport with the given [`ClientSocket`].
    pub fn new(current_time: Duration, authentication: ClientAuthentication, socket: impl ClientSocket) -> Result<Self, NetcodeError> {
        let netcode_client = NetcodeClient::new(current_time, authentication)?.set_encryption_policy(!socket.is_encrypted());

        Ok(Self {
            socket: Box::new(socket),
            netcode_client,
            buffer: [0u8; NETCODE_MAX_PACKET_BYTES],
        })
    }

    /// Gets the internal socket's [`ClientSocket::is_reliable`] value.
    pub fn is_reliable(&self) -> bool {
        self.socket.is_reliable()
    }

    /// Gets the client's [`SocketAddr`].
    ///
    /// Will return an error if the client doesn't have an address (e.g. a WebTransport client).
    pub fn addr(&self) -> io::Result<SocketAddr> {
        self.socket.addr()
    }

    /// Returns the client's id.
    pub fn client_id(&self) -> ClientId {
        self.netcode_client.client_id()
    }

    /// Returns `true` if the netcode client is connected.
    pub fn is_connected(&self) -> bool {
        self.netcode_client.is_connected()
    }

    /// Returns `true` if the netcode client is connecting.
    pub fn is_connecting(&self) -> bool {
        self.netcode_client.is_connecting()
    }

    /// Returns `true` if the netcode client is disconnected.
    pub fn is_disconnected(&self) -> bool {
        self.netcode_client.is_disconnected()
    }

    /// Returns the duration since the client last received a packet.
    ///
    /// Useful to detect timeouts.
    pub fn time_since_last_received_packet(&self) -> Duration {
        self.netcode_client.time_since_last_received_packet()
    }

    /// Disconnects the client from the transport layer.
    ///
    /// This sends the disconnect packet instantly, use this when closing/exiting games,
    /// should use [RenetClient::disconnect] otherwise.
    pub fn disconnect(&mut self) {
        if self.netcode_client.is_disconnected() {
            return;
        }

        match self.netcode_client.disconnect() {
            Ok((addr, packet)) => {
                if let Err(e) = self.socket.send(addr, packet) {
                    log::error!("Failed to send disconnect packet: {e}");
                }
            }
            Err(e) => log::error!("Failed to generate disconnect packet: {e}"),
        }
    }

    /// If the client is disconnected, returns the reason.
    pub fn disconnect_reason(&self) -> Option<DisconnectReason> {
        self.netcode_client.disconnect_reason()
    }

    /// Sends packets to the server.
    ///
    /// Should be called every tick.
    pub fn send_packets(&mut self, connection: &mut RenetClient) -> Result<(), NetcodeTransportError> {
        if let Some(reason) = self.netcode_client.disconnect_reason() {
            return Err(NetcodeError::Disconnected(reason).into());
        }

        let packets = connection.get_packets_to_send();
        for packet in packets {
            let (addr, payload) = self.netcode_client.generate_payload_packet(&packet)?;
            self.socket.send(addr, payload)?;
        }

        Ok(())
    }

    /// Advances the transport by the duration, and receive packets from the network.
    pub fn update(&mut self, duration: Duration, client: &mut RenetClient) -> Result<(), NetcodeTransportError> {
        if let Some(reason) = self.netcode_client.disconnect_reason() {
            // Mark the client as disconnected if an error occured in the transport layer
            client.disconnect_due_to_transport();
            self.socket.close();

            return Err(NetcodeError::Disconnected(reason).into());
        }

        if self.socket.is_closed() {
            client.disconnect_due_to_transport();
        }

        if let Some(error) = client.disconnect_reason() {
            let (addr, disconnect_packet) = self.netcode_client.disconnect()?;
            if !self.socket.is_closed() {
                self.socket.send(addr, disconnect_packet)?;
                self.socket.close();
            }
            return Err(error.into());
        }

        if self.netcode_client.is_connected() {
            client.set_connected();
        } else if self.netcode_client.is_connecting() {
            client.set_connecting();
        }

        self.socket.preupdate();

        loop {
            let packet = match self.socket.try_recv(&mut self.buffer) {
                Ok((len, addr)) => {
                    if addr != self.netcode_client.server_addr() {
                        log::debug!("Discarded packet from unknown server {:?}", addr);
                        continue;
                    }

                    &mut self.buffer[..len]
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => break,
                Err(e) => return Err(NetcodeTransportError::IO(e)),
            };

            if let Some(payload) = self.netcode_client.process_packet(packet) {
                client.process_packet(payload);
            }
        }

        if let Some((packet, addr)) = self.netcode_client.update(duration) {
            self.socket.send(addr, packet)?;
        }

        self.socket.postupdate();

        Ok(())
    }
}
