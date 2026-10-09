use crate::channel::reliable::{ReceiveChannelReliable, SendChannelReliable};
use crate::channel::unreliable::{ReceiveChannelUnreliable, SendChannelUnreliable};
use crate::channel::{ChannelConfig, DefaultChannel, SendType};
use crate::connection_stats::ConnectionStats;
use crate::error::DisconnectReason;
use crate::packet::{Packet, PacketPartialDeser, Payload};
use crate::zero_reinit_buffer;
use bytes::Bytes;
use octets::OctetsMut;

use std::collections::BTreeMap;
use std::ops::Range;
use std::time::Duration;

/// Configuration for a renet connection and its channels.
#[derive(Debug, Clone)]
pub struct ConnectionConfig {
    /// The number of bytes that is available per update tick to send messages.
    /// Default: 60_000, at 60hz this is becomes 28.8 Mbps
    pub available_bytes_per_tick: u64,
    /// The channels that the server sends to the client.
    /// The order of the channels in this Vec determines which channel has priority when generating packets.
    /// Each tick, the first channel can consume up to `available_bytes_per_tick`,
    /// used bytes are removed from it and passed to the next channel
    pub server_channels_config: Vec<ChannelConfig>,
    /// The channels that the client sends to the server.
    /// The order of the channels in this Vec determines which channel has priority when generating packets.
    /// Each tick, the first channel can consume up to `available_bytes_per_tick`,
    /// used bytes are removed from it and passed to the next channel
    pub client_channels_config: Vec<ChannelConfig>,
}

impl ConnectionConfig {
    /// Makes a new config with default `available_bytes_per_tick`.
    pub fn from_channels(server: Vec<ChannelConfig>, client: Vec<ChannelConfig>) -> Self {
        Self {
            // At 60hz this is becomes 28.8 Mbps
            available_bytes_per_tick: 60_000,
            server_channels_config: server,
            client_channels_config: client,
        }
    }

    /// Makes a new config with default `available_bytes_per_tick` and the same server and client channels.
    pub fn from_shared_channels(channels: Vec<ChannelConfig>) -> Self {
        Self::from_channels(channels.clone(), channels)
    }

    /// Makes a new config for testing purposes.
    pub fn test() -> Self {
        Self::from_shared_channels(DefaultChannel::config())
    }

    /// Downgrades all reliable channels to [`SendType::Unreliable`] with `ordered_reliable_substrate = true`.
    ///
    /// Used when setting up a client that has a socket with built-in reliability (such as WebSockets).
    pub fn downgrade_to_unreliable(&mut self) {
        self.server_channels_config.iter_mut().for_each(|c| match c.send_type {
            SendType::Unreliable { .. } => (),
            _ => {
                c.send_type = SendType::Unreliable {
                    ordered_reliable_substrate: true,
                };
            }
        });
        self.client_channels_config.iter_mut().for_each(|c| match c.send_type {
            SendType::Unreliable { .. } => (),
            _ => {
                c.send_type = SendType::Unreliable {
                    ordered_reliable_substrate: true,
                };
            }
        });
    }
}

#[derive(Debug, Clone)]
struct PacketSent {
    sent_at: Duration,
    info: PacketSentInfo,
}

#[derive(Debug, Clone)]
enum PacketSentInfo {
    // No need to track info for unreliable messages
    None,
    ReliableMessages {
        channel_id: u8,
        message_ids: Vec<u64>,
    },
    ReliableSliceMessage {
        channel_id: u8,
        message_id: u64,
        slice_index: usize,
    },
    // When an ack packet is acknowledged,
    // We remove all Ack ranges below the largest_acked sent by it
    Ack {
        largest_acked_packet: u64,
    },
}

#[derive(Debug)]
enum ChannelOrder {
    Reliable(u8),
    Unreliable(u8),
}

#[derive(Debug)]
enum SendChannel {
    Empty,
    Unreliable(SendChannelUnreliable),
    Reliable(SendChannelReliable),
}

#[derive(Debug)]
enum ReceiveChannel {
    Empty,
    Unreliable(ReceiveChannelUnreliable),
    Reliable(ReceiveChannelReliable),
}

/// Describes the stats of a connection.
pub struct NetworkInfo {
    /// Round-trip Time
    pub rtt: f64,
    pub packet_loss: f64,
    pub bytes_sent_per_second: f64,
    pub bytes_received_per_second: f64,
}

/// The connection status of a [`RenetClient`].
#[derive(Debug)]
pub enum RenetConnectionStatus {
    Connected,
    Connecting,
    Disconnected { reason: DisconnectReason },
}

#[derive(Debug)]
#[cfg_attr(feature = "bevy", derive(bevy_ecs::resource::Resource))]
pub struct RenetClient {
    has_reliable_socket: bool,
    packet_sequence: u64,
    current_time: Duration,
    sent_packets: BTreeMap<u64, PacketSent>,
    pending_acks: Vec<Range<u64>>,
    channel_send_order: Vec<ChannelOrder>,
    send_channels: Vec<SendChannel>,
    receive_channels: Vec<ReceiveChannel>,
    stats: ConnectionStats,
    available_bytes_per_tick: u64,
    connection_status: RenetConnectionStatus,
    rtt: f64,
    // ------ Scratch space -------
    // Cleared before use
    u64_scratch: Vec<u64>,
    // Drained after use
    payloads_ser: Vec<Payload>,
    // Payloads are popped and pushed and cleared after pop
    payloads_cache: Vec<Payload>,
    // Cleared before reassignment
    ack_ranges_reuse: Vec<Range<u64>>,
    // Message id vecs are popped and pushed and cleared before push
    message_ids_cache: Vec<Vec<u64>>,
}

impl RenetClient {
    /// The `has_reliable_socket` argument must match with the underlying socket.
    ///
    /// It will be false for sockets like UDP or WebTransport, and true for in-memory sockets and WebSockets.
    ///
    /// See `ClientSocket::is_reliable` in `renet2_netcode`.
    pub fn new(mut config: ConnectionConfig, has_reliable_socket: bool) -> Self {
        if has_reliable_socket {
            config.downgrade_to_unreliable();
        }

        Self::from_channels(
            has_reliable_socket,
            config.available_bytes_per_tick,
            config.client_channels_config,
            config.server_channels_config,
        )
    }

    // When creating a client from the server, the server_channels_config are used as send channels,
    // and the client_channels_config is used as recv channels.
    pub(crate) fn new_from_server(mut config: ConnectionConfig, has_reliable_socket: bool) -> Self {
        if has_reliable_socket {
            config.downgrade_to_unreliable();
        }

        Self::from_channels(
            has_reliable_socket,
            config.available_bytes_per_tick,
            config.server_channels_config,
            config.client_channels_config,
        )
    }

    fn from_channels(
        has_reliable_socket: bool,
        available_bytes_per_tick: u64,
        send_channels_config: Vec<ChannelConfig>,
        receive_channels_config: Vec<ChannelConfig>,
    ) -> Self {
        let max_send_channel = send_channels_config.iter().map(|c| c.channel_id).max().unwrap_or_default();
        let max_receive_channel = receive_channels_config.iter().map(|c| c.channel_id).max().unwrap_or_default();

        let mut send_channels = Vec::new();
        send_channels.resize_with(max_send_channel as usize + 1, || SendChannel::Empty);
        let mut channel_send_order: Vec<ChannelOrder> = Vec::with_capacity(send_channels_config.len());
        for channel_config in send_channels_config.iter() {
            let send_channel = &mut send_channels[channel_config.channel_id as usize];
            assert!(
                matches!(send_channel, SendChannel::Empty),
                "already exists send channel {}",
                channel_config.channel_id
            );

            match channel_config.send_type {
                SendType::Unreliable {
                    ordered_reliable_substrate,
                } => {
                    channel_send_order.push(ChannelOrder::Unreliable(channel_config.channel_id));
                    let channel = SendChannelUnreliable::new(
                        channel_config.channel_id,
                        channel_config.max_memory_usage_bytes,
                        ordered_reliable_substrate,
                    );
                    *send_channel = SendChannel::Unreliable(channel);
                }
                SendType::ReliableOrdered { resend_time } | SendType::ReliableUnordered { resend_time } => {
                    channel_send_order.push(ChannelOrder::Reliable(channel_config.channel_id));
                    let channel = SendChannelReliable::new(channel_config.channel_id, resend_time, channel_config.max_memory_usage_bytes);
                    *send_channel = SendChannel::Reliable(channel);
                }
            }
        }

        let mut receive_channels = Vec::new();
        receive_channels.resize_with(max_receive_channel as usize + 1, || ReceiveChannel::Empty);
        for channel_config in receive_channels_config.iter() {
            let receive_channel = &mut receive_channels[channel_config.channel_id as usize];
            assert!(
                matches!(receive_channel, ReceiveChannel::Empty),
                "already exists receive channel {}",
                channel_config.channel_id
            );

            match channel_config.send_type {
                SendType::Unreliable { .. } => {
                    let channel = ReceiveChannelUnreliable::new(channel_config.channel_id, channel_config.max_memory_usage_bytes);
                    *receive_channel = ReceiveChannel::Unreliable(channel);
                }
                SendType::ReliableOrdered { .. } => {
                    let channel = ReceiveChannelReliable::new(channel_config.max_memory_usage_bytes, true);
                    *receive_channel = ReceiveChannel::Reliable(channel);
                }
                SendType::ReliableUnordered { .. } => {
                    let channel = ReceiveChannelReliable::new(channel_config.max_memory_usage_bytes, false);
                    *receive_channel = ReceiveChannel::Reliable(channel);
                }
            }
        }

        Self {
            has_reliable_socket,
            packet_sequence: 0,
            current_time: Duration::ZERO,
            sent_packets: BTreeMap::new(),
            pending_acks: Vec::new(),
            channel_send_order,
            send_channels,
            receive_channels,
            stats: ConnectionStats::new(),
            rtt: 0.0,
            available_bytes_per_tick,
            connection_status: RenetConnectionStatus::Connecting,
            u64_scratch: Vec::new(),
            payloads_ser: Vec::new(),
            payloads_cache: Vec::new(),
            ack_ranges_reuse: Vec::new(),
            message_ids_cache: Vec::new(),
        }
    }

    /// Returns whether this client uses a reliable underlying socket.
    pub fn has_reliable_socket(&self) -> bool {
        self.has_reliable_socket
    }

    /// Returns the round-time trip for the connection.
    pub fn rtt(&self) -> f64 {
        self.rtt
    }

    /// Returns the packet loss for the connection.
    pub fn packet_loss(&self) -> f64 {
        self.stats.packet_loss()
    }

    /// Returns the bytes sent per second in the connection.
    pub fn bytes_sent_per_sec(&self) -> f64 {
        self.stats.bytes_sent_per_second(self.current_time)
    }

    /// Returns the bytes received per second in the connection.
    pub fn bytes_received_per_sec(&self) -> f64 {
        self.stats.bytes_received_per_second(self.current_time)
    }

    /// Returns all network information for the connection.
    pub fn network_info(&self) -> NetworkInfo {
        NetworkInfo {
            rtt: self.rtt,
            packet_loss: self.stats.packet_loss(),
            bytes_sent_per_second: self.stats.bytes_sent_per_second(self.current_time),
            bytes_received_per_second: self.stats.bytes_received_per_second(self.current_time),
        }
    }

    /// Returns whether the client is connected.
    #[inline]
    pub fn is_connected(&self) -> bool {
        matches!(self.connection_status, RenetConnectionStatus::Connected)
    }

    /// Returns whether the client is connecting.
    #[inline]
    pub fn is_connecting(&self) -> bool {
        matches!(self.connection_status, RenetConnectionStatus::Connecting)
    }

    /// Returns whether the client is disconnected.
    #[inline]
    pub fn is_disconnected(&self) -> bool {
        matches!(self.connection_status, RenetConnectionStatus::Disconnected { .. })
    }

    /// Returns the disconnect reason if the client is disconnected.
    pub fn disconnect_reason(&self) -> Option<DisconnectReason> {
        if let RenetConnectionStatus::Disconnected { reason } = self.connection_status { Some(reason) } else { None }
    }

    /// Set the client connection status to connected.
    ///
    /// Does nothing if the client is disconnected. A disconnected client must be reconstructed.
    ///
    /// <p style="background:rgba(77,220,255,0.16);padding:0.5em;">
    /// <strong>Note:</strong> This should only be called by the transport layer.
    /// </p>
    pub fn set_connected(&mut self) {
        if !self.is_disconnected() {
            self.connection_status = RenetConnectionStatus::Connected;
        }
    }

    /// Set the client connection status to connecting.
    ///
    /// Does nothing if the client is disconnected. A disconnected client must be reconstructed.
    ///
    /// <p style="background:rgba(77,220,255,0.16);padding:0.5em;">
    /// <strong>Note:</strong> This should only be called by the transport layer.
    /// </p>
    pub fn set_connecting(&mut self) {
        if !self.is_disconnected() {
            self.connection_status = RenetConnectionStatus::Connecting;
        }
    }

    /// Disconnect the client.
    ///
    /// If the client is already disconnected, it does nothing.
    pub fn disconnect(&mut self) {
        self.disconnect_with_reason(DisconnectReason::DisconnectedByClient);
    }

    /// Disconnect the client because an error occurred in the transport layer.
    ///
    /// If the client is already disconnected, it does nothing.
    /// <p style="background:rgba(77,220,255,0.16);padding:0.5em;">
    /// <strong>Note:</strong> This should only be called by the transport layer.
    /// </p>
    pub fn disconnect_due_to_transport(&mut self) {
        self.disconnect_with_reason(DisconnectReason::Transport);
    }

    /// Returns the available memory in bytes for the given channel.
    pub fn channel_available_memory<I: Into<u8>>(&self, channel_id: I) -> usize {
        let channel_id = channel_id.into();
        match self.send_channels.get(channel_id as usize) {
            None | Some(SendChannel::Empty) => {
                panic!("Called 'channel_available_memory' with invalid channel {channel_id}");
            }
            Some(SendChannel::Reliable(reliable_channel)) => reliable_channel.available_memory(),
            Some(SendChannel::Unreliable(unreliable_channel)) => unreliable_channel.available_memory(),
        }
    }

    /// Checks if the channel can send a message with the given size in bytes.
    pub fn can_send_message<I: Into<u8>>(&self, channel_id: I, size_bytes: usize) -> bool {
        let channel_id = channel_id.into();
        match self.send_channels.get(channel_id as usize) {
            None | Some(SendChannel::Empty) => {
                panic!("Called 'can_send_message' with invalid channel {channel_id}");
            }
            Some(SendChannel::Reliable(reliable_channel)) => reliable_channel.can_send_message(size_bytes),
            Some(SendChannel::Unreliable(unreliable_channel)) => unreliable_channel.can_send_message(size_bytes),
        }
    }

    /// Send a message to the server over a channel.
    pub fn send_message<I: Into<u8>, B: Into<Bytes>>(&mut self, channel_id: I, message: B) {
        if self.is_disconnected() {
            return;
        }

        let channel_id = channel_id.into();
        match self.send_channels.get_mut(channel_id as usize) {
            None | Some(SendChannel::Empty) => {
                panic!("Called 'send_message' with invalid channel {channel_id}");
            }
            Some(SendChannel::Reliable(reliable_channel)) => {
                if let Err(error) = reliable_channel.send_message(message.into()) {
                    self.disconnect_with_reason(DisconnectReason::SendChannelError { channel_id, error });
                }
            }
            Some(SendChannel::Unreliable(unreliable_channel)) => {
                unreliable_channel.send_message(message.into());
            }
        }
    }

    /// Receive a message from the server over a channel.
    pub fn receive_message<I: Into<u8>>(&mut self, channel_id: I) -> Option<Bytes> {
        if self.is_disconnected() {
            return None;
        }

        let channel_id = channel_id.into();
        match self.receive_channels.get_mut(channel_id as usize) {
            None | Some(ReceiveChannel::Empty) => {
                panic!("Called 'receive_message' with invalid channel {channel_id}");
            }
            Some(ReceiveChannel::Reliable(reliable_channel)) => reliable_channel.receive_message(),
            Some(ReceiveChannel::Unreliable(unreliable_channel)) => unreliable_channel.receive_message(),
        }
    }

    /// Advances the client by the duration.
    /// Should be called every tick
    pub fn update(&mut self, duration: Duration) {
        self.current_time += duration;
        self.stats.update(self.current_time);

        for unreliable_channel in self.receive_channels.iter_mut() {
            let ReceiveChannel::Unreliable(unreliable_channel) = unreliable_channel else {
                continue;
            };
            unreliable_channel.discard_incomplete_old_slices(self.current_time);
        }

        // Discard lost packets
        self.u64_scratch.clear();
        for (&sequence, sent_packet) in self.sent_packets.iter() {
            const DISCARD_AFTER: Duration = Duration::from_secs(3);
            if self.current_time - sent_packet.sent_at >= DISCARD_AFTER {
                self.u64_scratch.push(sequence);
            } else {
                // If the current packet is not lost, the next ones will not be lost
                // since all the next packets were sent after this one.
                break;
            }
        }

        for sequence in self.u64_scratch.iter() {
            if let Some(removed) = self.sent_packets.remove(sequence) {
                if let PacketSentInfo::ReliableMessages { mut message_ids, .. } = removed.info {
                    message_ids.clear();
                    self.message_ids_cache.push(message_ids);
                }
            }
        }
    }

    /// Process a packet received from the connected client or server.
    /// <p style="background:rgba(77,220,255,0.16);padding:0.5em;">
    /// <strong>Note:</strong> This should only be called by the transport layer.
    /// </p>
    pub fn process_packet(&mut self, packet: &[u8]) {
        if let Err(reason) = self.process_packet_impl(packet) {
            self.disconnect_with_reason(reason);
        }
    }

    fn process_packet_impl(&mut self, packet: &[u8]) -> Result<(), DisconnectReason> {
        if self.is_disconnected() {
            return Ok(());
        }

        self.stats.received_packet(packet.len() as u64);
        let partial = PacketPartialDeser::from_bytes(packet).map_err(|e| DisconnectReason::PacketDeserialization(e))?;

        self.add_pending_ack(partial.sequence());

        match partial {
            PacketPartialDeser::SmallReliable {
                channel_id, mut messages, ..
            } => {
                let Some(ReceiveChannel::Reliable(channel)) = self.receive_channels.get_mut(channel_id as usize) else {
                    return Err(DisconnectReason::ReceivedInvalidChannelId(channel_id));
                };

                while let Some((message_id, message)) = messages.next().map_err(|e| DisconnectReason::PacketDeserialization(e))? {
                    channel
                        .process_message(message, message_id)
                        .map_err(|error| DisconnectReason::ReceiveChannelError { channel_id, error })?;
                }
            }
            PacketPartialDeser::SmallUnreliable {
                channel_id, mut messages, ..
            } => {
                let Some(ReceiveChannel::Unreliable(channel)) = self.receive_channels.get_mut(channel_id as usize) else {
                    return Err(DisconnectReason::ReceivedInvalidChannelId(channel_id));
                };

                while let Some(message) = messages.next().map_err(|e| DisconnectReason::PacketDeserialization(e))? {
                    channel.process_message(message);
                }
            }
            PacketPartialDeser::ReliableSlice { channel_id, slice, .. } => {
                let Some(ReceiveChannel::Reliable(channel)) = self.receive_channels.get_mut(channel_id as usize) else {
                    return Err(DisconnectReason::ReceivedInvalidChannelId(channel_id));
                };

                channel
                    .process_slice(slice)
                    .map_err(|error| DisconnectReason::ReceiveChannelError { channel_id, error })?;
            }
            PacketPartialDeser::UnreliableSlice { channel_id, slice, .. } => {
                let Some(ReceiveChannel::Unreliable(channel)) = self.receive_channels.get_mut(channel_id as usize) else {
                    return Err(DisconnectReason::ReceivedInvalidChannelId(channel_id));
                };

                channel
                    .process_slice(slice, self.current_time)
                    .map_err(|error| DisconnectReason::ReceiveChannelError { channel_id, error })?;
            }
            PacketPartialDeser::Ack { ack_ranges, .. } => {
                // Create list with just new acks
                // This prevents DoS from huge ack ranges
                self.u64_scratch.clear();
                for range in ack_ranges {
                    for (&sequence, _) in self.sent_packets.range(range) {
                        self.u64_scratch.push(sequence)
                    }
                }

                for packet_sequence in self.u64_scratch.iter().copied() {
                    let sent_packet = self.sent_packets.remove(&packet_sequence).unwrap();
                    self.stats.acked_packet(sent_packet.sent_at, self.current_time);

                    // Update rtt
                    let rtt = (self.current_time - sent_packet.sent_at).as_secs_f64();
                    if self.rtt < f64::EPSILON {
                        self.rtt = rtt;
                    } else {
                        self.rtt = self.rtt * 0.875 + rtt * 0.125;
                    }

                    match sent_packet.info {
                        PacketSentInfo::ReliableMessages {
                            channel_id,
                            mut message_ids,
                        } => {
                            let SendChannel::Reliable(channel) = self.send_channels.get_mut(channel_id as usize).unwrap() else {
                                panic!("Acked packet has invalid channel {channel_id}");
                            };
                            for message_id in message_ids.iter() {
                                channel.process_message_ack(*message_id);
                            }
                            message_ids.clear();
                            self.message_ids_cache.push(message_ids);
                        }
                        PacketSentInfo::ReliableSliceMessage {
                            channel_id,
                            message_id,
                            slice_index,
                        } => {
                            let SendChannel::Reliable(channel) = self.send_channels.get_mut(channel_id as usize).unwrap() else {
                                panic!("Acked packet has invalid channel {channel_id}");
                            };
                            channel.process_slice_message_ack(message_id, slice_index);
                        }
                        PacketSentInfo::Ack { largest_acked_packet } => {
                            Self::acked_largest(&mut self.pending_acks, largest_acked_packet);
                        }
                        PacketSentInfo::None => {}
                    }
                }
            }
        }

        Ok(())
    }

    /// Returns a list of packets to be sent to the server.
    /// <p style="background:rgba(77,220,255,0.16);padding:0.5em;">
    /// <strong>Note:</strong> This should only be called by the transport layer.
    /// </p>
    pub fn get_packets_to_send(&mut self) -> &[Payload] {
        if self.is_disconnected() {
            return &[];
        }

        let sent_at = self.current_time;
        self.payloads_cache.extend(self.payloads_ser.drain(..));
        let mut bytes_sent: u64 = 0;

        let mut apply_packet = |packet: &Packet| -> Result<(), DisconnectReason> {
            match packet {
                Packet::SmallReliable {
                    sequence,
                    channel_id,
                    messages,
                } => {
                    let mut message_ids = self.message_ids_cache.pop().unwrap_or_default();
                    message_ids.extend(messages.iter().map(|(id, _)| *id));
                    self.sent_packets.insert(
                        *sequence,
                        PacketSent {
                            sent_at,
                            info: PacketSentInfo::ReliableMessages {
                                channel_id: *channel_id,
                                message_ids,
                            },
                        },
                    );
                }
                Packet::ReliableSlice {
                    sequence,
                    channel_id,
                    slice,
                } => {
                    self.sent_packets.insert(
                        *sequence,
                        PacketSent {
                            sent_at,
                            info: PacketSentInfo::ReliableSliceMessage {
                                channel_id: *channel_id,
                                message_id: slice.message_id,
                                slice_index: slice.slice_index,
                            },
                        },
                    );
                }
                Packet::SmallUnreliable { sequence, .. } => {
                    self.sent_packets.insert(
                        *sequence,
                        PacketSent {
                            sent_at,
                            info: PacketSentInfo::None,
                        },
                    );
                }
                Packet::UnreliableSlice { sequence, .. } => {
                    self.sent_packets.insert(
                        *sequence,
                        PacketSent {
                            sent_at,
                            info: PacketSentInfo::None,
                        },
                    );
                }
                Packet::Ack { sequence, ack_ranges } => {
                    let last_range = ack_ranges.last().unwrap();
                    let largest_acked_packet = last_range.end - 1;
                    self.sent_packets.insert(
                        *sequence,
                        PacketSent {
                            sent_at,
                            info: PacketSentInfo::Ack { largest_acked_packet },
                        },
                    );
                }
            }

            let mut buffer = self.payloads_cache.pop().unwrap_or_default();
            const PACKET_SIZE: usize = 1400;
            zero_reinit_buffer(&mut buffer, PACKET_SIZE);
            let mut oct = OctetsMut::with_slice(buffer.as_mut_slice());
            let len = match packet.to_bytes(&mut oct) {
                Err(err) => {
                    return Err(DisconnectReason::PacketSerialization(err));
                }
                Ok(len) => len,
            };

            bytes_sent += len as u64;
            buffer.resize(len, 0);
            self.payloads_ser.push(buffer);

            Ok(())
        };

        let mut available_bytes = self.available_bytes_per_tick;
        for order in self.channel_send_order.iter() {
            match order {
                ChannelOrder::Reliable(channel_id) => {
                    let SendChannel::Reliable(channel) = self.send_channels.get_mut(*channel_id as usize).unwrap() else {
                        panic!("Packet to send has invalid channel {channel_id}");
                    };
                    for packet in channel.get_packets_to_send(&mut self.packet_sequence, &mut available_bytes, self.current_time) {
                        if let Err(err) = apply_packet(packet) {
                            self.disconnect_with_reason(err);
                            return &[];
                        }
                    }
                }
                ChannelOrder::Unreliable(channel_id) => {
                    let SendChannel::Unreliable(channel) = self.send_channels.get_mut(*channel_id as usize).unwrap() else {
                        panic!("Packet to send has invalid channel {channel_id}");
                    };
                    for packet in channel.get_packets_to_send(&mut self.packet_sequence, &mut available_bytes) {
                        if let Err(err) = apply_packet(packet) {
                            self.disconnect_with_reason(err);
                            return &[];
                        }
                    }
                }
            }
        }

        if !self.pending_acks.is_empty() {
            let mut ack_ranges = std::mem::take(&mut self.ack_ranges_reuse);
            ack_ranges.extend(self.pending_acks.iter().cloned());
            let ack_packet = Packet::Ack {
                sequence: self.packet_sequence,
                ack_ranges,
            };
            self.packet_sequence += 1;
            if let Err(err) = apply_packet(&ack_packet) {
                let Packet::Ack { mut ack_ranges, .. } = ack_packet else {
                    unreachable!();
                };
                ack_ranges.clear();
                self.ack_ranges_reuse = ack_ranges;
                self.disconnect_with_reason(err);
                return &[];
            }
            let Packet::Ack { mut ack_ranges, .. } = ack_packet else {
                unreachable!();
            };
            ack_ranges.clear();
            self.ack_ranges_reuse = ack_ranges;
        }

        self.stats.sent_packets(self.payloads_ser.len() as u64, bytes_sent);

        &self.payloads_ser
    }

    fn add_pending_ack(&mut self, sequence: u64) {
        if self.pending_acks.is_empty() {
            self.pending_acks.push(sequence..sequence + 1);
            return;
        }

        // Try to fit the sequence in an existing range
        for index in 0..self.pending_acks.len() {
            let range = &mut self.pending_acks[index];
            if range.contains(&sequence) {
                // Sequence already contained in this range
                return;
            }

            if range.start == sequence + 1 {
                // New sequence is just before this range
                range.start = sequence;
                return;
            } else if range.end == sequence {
                // New sequence is just after this range
                range.end = sequence + 1;

                // Check if we can merge with the range just after it
                let next_index = index + 1;
                if next_index < self.pending_acks.len() && self.pending_acks[index].end == self.pending_acks[next_index].start {
                    self.pending_acks[index].end = self.pending_acks[next_index].end;
                    self.pending_acks.remove(next_index);
                }

                return;
            } else if self.pending_acks[index].start > sequence + 1 {
                // New sequence is before this range and not extensible to it
                // Add new range to the left
                self.pending_acks.insert(index, sequence..sequence + 1);
                return;
            }
        }

        // New sequence was not before or adjacent to any range
        // Add new range with only this sequence at the end
        self.pending_acks.push(sequence..sequence + 1);

        // Limit to 64 pending ranges
        if self.pending_acks.len() > 64 {
            self.pending_acks.remove(0);
        }
    }

    fn acked_largest(pending_acks: &mut Vec<Range<u64>>, largest_ack: u64) {
        while !pending_acks.is_empty() {
            let range: &mut Range<u64> = &mut pending_acks[0];

            // Largest ack is below the range, stop checking
            if largest_ack < range.start {
                return;
            }

            // Largest ack is above the range, remove it
            if range.end <= largest_ack {
                pending_acks.remove(0);
                continue;
            }

            // Largest ack is contained in the range
            // Update start
            range.start = largest_ack + 1;
            if range.is_empty() {
                pending_acks.remove(0);
            }

            return;
        }
    }

    pub(crate) fn disconnect_with_reason(&mut self, reason: DisconnectReason) {
        if !self.is_disconnected() {
            self.connection_status = RenetConnectionStatus::Disconnected { reason };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_acks() {
        let mut connection = RenetClient::new(ConnectionConfig::test(), false);
        connection.add_pending_ack(3);
        assert_eq!(connection.pending_acks, vec![3..4]);

        connection.add_pending_ack(4);
        assert_eq!(connection.pending_acks, vec![3..5]);

        connection.add_pending_ack(2);
        assert_eq!(connection.pending_acks, vec![2..5]);

        connection.add_pending_ack(0);
        assert_eq!(connection.pending_acks, vec![0..1, 2..5]);

        connection.add_pending_ack(7);
        assert_eq!(connection.pending_acks, vec![0..1, 2..5, 7..8]);

        connection.add_pending_ack(1);
        assert_eq!(connection.pending_acks, vec![0..5, 7..8]);

        connection.add_pending_ack(5);
        assert_eq!(connection.pending_acks, vec![0..6, 7..8]);

        connection.add_pending_ack(6);
        assert_eq!(connection.pending_acks, vec![0..8]);
    }

    #[test]
    fn ack_pending_acks() {
        let mut connection = RenetClient::new(ConnectionConfig::test(), false);
        for i in 0..10 {
            connection.add_pending_ack(i);
        }

        assert_eq!(connection.pending_acks, vec![0..10]);

        RenetClient::acked_largest(&mut connection.pending_acks, 0);
        assert_eq!(connection.pending_acks, vec![1..10]);

        RenetClient::acked_largest(&mut connection.pending_acks, 3);
        assert_eq!(connection.pending_acks, vec![4..10]);

        connection.add_pending_ack(0);
        assert_eq!(connection.pending_acks, vec![0..1, 4..10]);
        RenetClient::acked_largest(&mut connection.pending_acks, 5);
        assert_eq!(connection.pending_acks, vec![6..10]);

        connection.add_pending_ack(0);
        assert_eq!(connection.pending_acks, vec![0..1, 6..10]);
        RenetClient::acked_largest(&mut connection.pending_acks, 10);
        assert_eq!(connection.pending_acks, vec![]);
    }

    #[test]
    fn discard_old_packets() {
        let mut connection = RenetClient::new(ConnectionConfig::test(), false);
        let message: Bytes = vec![5; 5].into();
        connection.send_message(0, message);

        connection.get_packets_to_send();
        assert_eq!(connection.sent_packets.len(), 1);

        connection.update(Duration::from_secs(1));
        assert_eq!(connection.sent_packets.len(), 1);

        connection.update(Duration::from_secs(4));
        assert_eq!(connection.sent_packets.len(), 0);
    }
}
