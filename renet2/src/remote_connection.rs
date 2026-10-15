use crate::channel::reliable::{ReceiveChannelReliable, SendChannelReliable};
use crate::channel::unreliable::{ReceiveChannelUnreliable, SendChannelUnreliable};
use crate::channel::{ChannelConfig, DefaultChannel, SendType};
use crate::connection_stats::ConnectionStats;
use crate::error::DisconnectReason;
use crate::packet::{AckRanges, AckRangesIterator, Packet, PacketPartialDeser, Payload, SerializationError};
use crate::zero_reinit_buffer;
use bytes::Bytes;
use octets::OctetsMut;

use std::collections::BTreeMap;
use std::ops::Range;
use std::time::Duration;

/// Limit to 63 pending ranges
/// - This keeps ack packets under 1 kB and limits acks map iter cost.
/// - The number of ranges can be stored with 6 bits.
pub(crate) const MAX_ACK_RANGES: u64 = 63;

/// Matches `NETCODE_MAX_PAYLOAD_BYTES`.
pub(crate) const MAX_PAYLOAD_BYTES: usize = 1300;

/// For stale sent packets that weren't acked.
const DISCARD_AFTER: Duration = Duration::from_secs(3);

/// Configuration for a renet connection and its channels.
#[derive(Debug, Clone)]
pub struct ConnectionConfig {
    /// The number of bytes that is available per update tick to send messages.
    ///
    /// Default: 60_000, at 60hz this is 28.8 Mbps
    pub available_bytes_per_tick: u64,
    /// Controls when acks are sent.
    pub ack_config: AckConfig,
    /// The channels that the server sends to the client.
    /// The order of the channels in this Vec determines which channel has priority when generating packets.
    /// Each tick, the first channel can consume up to `available_bytes_per_tick`.
    /// Used bytes are removed from it and passed to the next channel.
    ///
    /// Max: 256 channels
    pub server_channels_config: Vec<ChannelConfig>,
    /// The channels that the client sends to the server.
    /// The order of the channels in this Vec determines which channel has priority when generating packets.
    /// Each tick, the first channel can consume up to `available_bytes_per_tick`.
    /// Used bytes are removed from it and passed to the next channel.
    ///
    /// Max: 256 channels
    pub client_channels_config: Vec<ChannelConfig>,
}

impl ConnectionConfig {
    /// Makes a new config with default client configs.
    pub fn from_channels(server: Vec<ChannelConfig>, client: Vec<ChannelConfig>) -> Self {
        Self {
            // At 60hz this is 28.8 Mbps
            available_bytes_per_tick: 60_000,
            ack_config: AckConfig::default(),
            server_channels_config: server,
            client_channels_config: client,
        }
    }

    /// Makes a new config with default client configs and the same server and client channels.
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
pub struct AckConfig {
    /// The timeout for ack packets.
    /// If no acks were sent in the last `last_sent_timeout`, then a
    /// packet will be sent with acks. This is primarily needed to maintain rtt calculations, as a fallback if
    /// ack burst fails, and for cleanup of unacked sends.
    /// - Acks may be sent even if there were recent packets sent by us (this is not a keepalive condition).
    /// - Set it to a high number if you don't care about rtt or want to use reliable messages. Note that unacked sends
    ///   clean themselves up after a delay, but this timeout can reduce how much is tracked locally.
    /// - Uses two timers, one for 'any sequenced packet' and one for 'any acks'. A sequenced ack will be sent if either
    ///   timer exceeds `ack_send_timeout`. Note that ack bursts aren't automatically sequenced. Sequences are required
    ///   for rtt calculations.
    ///
    /// Default: 250ms (this is `NETCODE_SEND_RATE` which means acks will be sent instead of `netcode` keepalive packets)
    pub ack_send_timeout: Duration,
    /// The number of times acks should be sent after receiving a reliable message from the paired client before
    /// falling back to the base ack send cadence. Tradeoff between packet
    /// loss and bandwidth. Note if the server tick rate is fairly slow (e.g. 10hz) then even with a small ack burst
    /// the paired client will resend reliable messages before you get through the burst count (assuming 1 ack sent
    /// per tick, which will be the case if no messages are sent out that acks can piggy-back on). Which will reset
    /// the burst count if packets aren't lost. So a high `ack_burst` won't be that useful.
    ///
    /// Default: 5
    pub ack_burst: u16,
}

impl Default for AckConfig {
    fn default() -> Self {
        Self {
            // See `NETCODE_SEND_RATE`. Matching the keepalive timer in renetcode2 reduces redundant keepalive packets from
            // the netcode layer.
            ack_send_timeout: Duration::from_millis(250),
            ack_burst: 5,
        }
    }
}

/// When a sent packet is acknowledged, we remove all pending ack ranges for received packets below the highest ack
/// that we sent in that packet. This removes all successfully-acked received packets *and* packets that arrived late
/// with a lower sequence while the ack for our sent packet was in transit. Reliable packets always trigger an
/// ack burst, so the described ack discarding mechanism will at most truncate the burst (from the point of view
/// of the discarded sequence) after at least one ack was sent. If a reliable packet is both late in arriving and
/// all its burst acks are lost, then it will ultimately be resent by the paired client after the resend timer.
/// All packets may contain ack ranges.
#[derive(Debug, Clone)]
struct PacketSent {
    sent_at: Duration,
    info: PacketSentInfo,
    largest_acked_packet: Option<u64>,
    /// Used to end ack bursts early if our acks for it are acked.
    acked_burst_trigger: Option<u64>,
}

#[derive(Debug, Clone)]
enum PacketSentInfo {
    None,
    Ack,
    ReliableMessages {
        channel_id: u8,
        message_ids: Vec<u64>,
    },
    ReliableSliceMessage {
        channel_id: u8,
        message_id: u64,
        slice_index: usize,
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
    // ------ Configs -------
    has_reliable_socket: bool,
    available_bytes_per_tick: u64,
    ack_config: AckConfig,
    // ------ State -------
    packet_sequence: u64,
    // An approximation of current time, it will drift. Should not be exposed to the public API.
    current_time: Duration,
    next_sequenced_ack: Duration,
    next_any_ack: Duration,
    ack_burst_count: u16,
    ack_burst_trigger: u64,
    rtt_delay_ref: Option<Duration>,
    sent_packets: BTreeMap<u64, PacketSent>,
    pending_acks: Vec<Range<u64>>,
    channel_send_order: Vec<ChannelOrder>,
    send_channels: Vec<SendChannel>,
    receive_channels: Vec<ReceiveChannel>,
    stats: ConnectionStats,
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
            config.ack_config,
            config.client_channels_config,
            config.server_channels_config,
        )
    }

    // When creating a client from within the server, the server_channels_config are used as send channels,
    // and the client_channels_config is used as recv channels.
    pub(crate) fn new_from_server(mut config: ConnectionConfig, has_reliable_socket: bool) -> Self {
        if has_reliable_socket {
            config.downgrade_to_unreliable();
        }

        Self::from_channels(
            has_reliable_socket,
            config.available_bytes_per_tick,
            config.ack_config,
            config.server_channels_config,
            config.client_channels_config,
        )
    }

    fn from_channels(
        has_reliable_socket: bool,
        available_bytes_per_tick: u64,
        ack_config: AckConfig,
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
            available_bytes_per_tick,
            ack_config,
            packet_sequence: 0,
            current_time: Duration::ZERO,
            next_sequenced_ack: Duration::ZERO,
            next_any_ack: Duration::ZERO,
            ack_burst_count: 0,
            ack_burst_trigger: 0,
            rtt_delay_ref: None,
            sent_packets: BTreeMap::new(),
            pending_acks: Vec::new(),
            channel_send_order,
            send_channels,
            receive_channels,
            stats: ConnectionStats::new(),
            rtt: 0.0,
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
    ///
    /// `rtt` will be stale if packet loss is high, if no message has ever been sent,
    /// or if an invalid sequence is used in packets due to a bug.
    pub fn rtt(&self) -> f64 {
        self.rtt
    }

    /// Returns the packet loss for the connection.
    ///
    /// Only computed for sequenced packets (reliable packets and occasional ack packets).
    /// Send acks more often with [`AckConfig::ack_send_timeout`] if you want this to be a higher-fidelity
    /// signal.
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

    /// Send a message to the paired client over a channel.
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

    /// Receive a message from the paired client over a channel.
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
            if self.current_time - sent_packet.sent_at >= DISCARD_AFTER {
                self.u64_scratch.push(sequence);
            } else {
                // If the current packet is not lost, the next ones will not be lost
                // since all the next packets were sent after this one.
                break;
            }
        }

        for sequence in self.u64_scratch.iter() {
            if let Some(removed) = self.sent_packets.remove(sequence)
                && let PacketSentInfo::ReliableMessages { mut message_ids, .. } = removed.info
            {
                message_ids.clear();
                self.message_ids_cache.push(message_ids);
            }
        }

        // Reset rtt delay
        if self.pending_acks.is_empty() {
            self.rtt_delay_ref = None;
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

        // Extract packet
        self.stats.received_packet(packet.len() as u64);
        let partial = PacketPartialDeser::from_raw_bytes(packet).map_err(DisconnectReason::PacketDeserialization)?;

        // Handle messages
        let ack_ranges = match partial {
            PacketPartialDeser::SmallReliable {
                sequence,
                channel_id,
                mut messages,
                ..
            } => {
                let Some(ReceiveChannel::Reliable(channel)) = self.receive_channels.get_mut(channel_id as usize) else {
                    return Err(DisconnectReason::ReceivedInvalidChannelId(channel_id));
                };

                while let Some((message_id, message)) = messages.next().map_err(DisconnectReason::PacketDeserialization)? {
                    channel
                        .process_message(message, message_id)
                        .map_err(|error| DisconnectReason::ReceiveChannelError { channel_id, error })?;
                }

                // Reset ack burst
                self.ack_burst_count = self.ack_config.ack_burst;
                self.ack_burst_trigger = sequence;

                // Use ack ranges as sequence shuttle.
                let mut ack_ranges = messages
                    .into_acks_iter()
                    .map_err(DisconnectReason::PacketDeserialization)?
                    .unwrap_or_default();
                if ack_ranges.sequence.is_some() {
                    return Err(DisconnectReason::PacketDeserialization(SerializationError::InvalidAckRange));
                }
                ack_ranges.sequence = Some(sequence);
                Some(ack_ranges)
            }
            PacketPartialDeser::SmallUnreliable {
                channel_id, mut messages, ..
            } => {
                let Some(ReceiveChannel::Unreliable(channel)) = self.receive_channels.get_mut(channel_id as usize) else {
                    return Err(DisconnectReason::ReceivedInvalidChannelId(channel_id));
                };

                while let Some(message) = messages.next().map_err(DisconnectReason::PacketDeserialization)? {
                    channel.process_message(message);
                }

                messages.into_acks_iter().map_err(DisconnectReason::PacketDeserialization)?
            }
            PacketPartialDeser::ReliableSlice {
                sequence,
                channel_id,
                slice,
                ack_ranges,
                ..
            } => {
                let Some(ReceiveChannel::Reliable(channel)) = self.receive_channels.get_mut(channel_id as usize) else {
                    return Err(DisconnectReason::ReceivedInvalidChannelId(channel_id));
                };

                channel
                    .process_slice(slice)
                    .map_err(|error| DisconnectReason::ReceiveChannelError { channel_id, error })?;

                // Reset ack burst
                self.ack_burst_count = self.ack_config.ack_burst;
                self.ack_burst_trigger = sequence;

                // Use ack ranges as sequence shuttle.
                let mut ack_ranges = ack_ranges.unwrap_or_default();
                if ack_ranges.sequence.is_some() {
                    return Err(DisconnectReason::PacketDeserialization(SerializationError::InvalidAckRange));
                }
                ack_ranges.sequence = Some(sequence);
                Some(ack_ranges)
            }
            PacketPartialDeser::UnreliableSlice {
                channel_id,
                slice,
                ack_ranges,
                ..
            } => {
                let Some(ReceiveChannel::Unreliable(channel)) = self.receive_channels.get_mut(channel_id as usize) else {
                    return Err(DisconnectReason::ReceivedInvalidChannelId(channel_id));
                };

                channel
                    .process_slice(slice, self.current_time)
                    .map_err(|error| DisconnectReason::ReceiveChannelError { channel_id, error })?;

                ack_ranges
            }
            PacketPartialDeser::Ack { ack_ranges, .. } => ack_ranges,
            PacketPartialDeser::Sequence { sequence } => {
                // Use ack ranges as sequence shuttle.
                let mut ack_ranges = AckRangesIterator::default();
                ack_ranges.sequence = Some(sequence);
                Some(ack_ranges)
            }
        };

        // Handle acks
        if let Some(ack_ranges) = ack_ranges {
            // Do this before handling ack ranges in case the paired client duplicates a sequence that we already acked
            // or uses sequences out of order. If `ack_ranges` points to our packets that acked paired client
            // packets *ahead* of this packet's sequence, then this sequence will be cleared from `self.pending_acks`.
            if let Some(sequence) = ack_ranges.sequence {
                self.add_pending_ack(sequence);
            }

            self.handle_incoming_ack_ranges(ack_ranges)?;
        }

        Ok(())
    }

    /// Returns a list of packets to be sent to the server.
    /// <p style="background:rgba(77,220,255,0.16);padding:0.5em;">
    /// <strong>Note:</strong> This should only be called by the transport layer.
    /// </p>
    pub fn get_packets_to_send(&mut self) -> &[Payload] {
        match self.get_packets_to_send_impl() {
            Ok(()) => &self.payloads_ser,
            Err(err) => {
                self.disconnect_with_reason(err);
                &[]
            }
        }
    }

    /// On success, `self.payloads_ser` will contain the payloads to send.
    fn get_packets_to_send_impl(&mut self) -> Result<(), DisconnectReason> {
        self.payloads_cache.append(&mut self.payloads_ser);

        if !self.is_connected() {
            return Ok(());
        }

        // Prep acks ranges for pasting into a packet
        let mut acks_buffer = self.payloads_cache.pop().unwrap_or_default();
        zero_reinit_buffer(&mut acks_buffer, MAX_PAYLOAD_BYTES);
        let mut largest_ack = 0;
        let rtt_delay_ms = self.rtt_delay_ref.map(|r| self.current_time.saturating_sub(r).as_millis() as u64);
        let has_pending_acks = !self.pending_acks.is_empty();
        let mut serialized: Option<Option<u64>> = None;
        let mut acks_ser_fn =
            |maybe_maybe_sequence: Option<Option<u64>>, acks_buffer: &mut Vec<u8>| -> Result<Option<u64>, DisconnectReason> {
                if !has_pending_acks {
                    return Ok(None);
                }
                let Some(maybe_sequence) = maybe_maybe_sequence else {
                    return Ok(None);
                };
                // Note: we reserialize if the sequence changes. This can happen during a burst if we serialize
                // one sequence once and then no sequences 1+ times.
                if serialized != Some(maybe_sequence) {
                    serialized = Some(maybe_sequence);
                    let mut ack_ranges = std::mem::take(&mut self.ack_ranges_reuse);
                    ack_ranges.extend(self.pending_acks.iter().cloned());
                    let mut ranges = AckRanges {
                        sequence: maybe_sequence,
                        rtt_delay_ms,
                        ack_ranges,
                    };

                    let last_range = ranges.ack_ranges.last().unwrap();
                    largest_ack = last_range.end - 1;

                    let mut oct = OctetsMut::with_slice(acks_buffer.as_mut_slice());
                    let len = ranges.to_bytes(&mut oct).map_err(DisconnectReason::PacketSerialization)?;
                    ranges.ack_ranges.clear();
                    self.ack_ranges_reuse = ranges.ack_ranges;
                    acks_buffer.resize(len, 0); // should always be <= existing size
                }

                Ok(Some(largest_ack))
            };

        let sent_at = self.current_time;
        let mut bytes_sent: u64 = 0;

        // Callback for applying packets to self.
        // Returns true if acks were written.
        let mut consume_packet_fn = |packet: &Packet, maybe_maybe_sequence: Option<Option<u64>>| -> Result<bool, DisconnectReason> {
            let largest_ack = acks_ser_fn(maybe_maybe_sequence, &mut acks_buffer)?;
            let acks_ser = if largest_ack.is_some() { Some(acks_buffer.as_slice()) } else { None };

            // Serialize packet
            let mut buffer = self.payloads_cache.pop().unwrap_or_default();
            zero_reinit_buffer(&mut buffer, MAX_PAYLOAD_BYTES);
            let mut oct = OctetsMut::with_slice(buffer.as_mut_slice());
            let (len, acks_written) = packet.to_bytes(&mut oct, acks_ser).map_err(DisconnectReason::PacketSerialization)?;

            let largest_acked_packet = if acks_written { largest_ack } else { None };
            // Burst trigger tracking only used to truncate the burst early if our ack of the trigger is acked.
            let acked_burst_trigger = if acks_written && self.ack_burst_count > 0 { Some(self.ack_burst_trigger) } else { None };

            bytes_sent += len as u64;
            buffer.resize(len, 0); // should always be <= existing size
            self.payloads_ser.push(buffer);

            // Preserve packet info to handle future acks
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
                            largest_acked_packet,
                            acked_burst_trigger,
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
                            largest_acked_packet,
                            acked_burst_trigger,
                        },
                    );
                }
                Packet::SmallUnreliable { .. } | Packet::UnreliableSlice { .. } | Packet::Ack => {
                    // Sequences only show up in these packets if we need an ack with sequence for rtt
                    // calculations.
                    if acks_written && let Some(Some(sequence)) = maybe_maybe_sequence {
                        self.sent_packets.insert(
                            sequence,
                            PacketSent {
                                sent_at,
                                info: PacketSentInfo::Ack,
                                largest_acked_packet,
                                acked_burst_trigger,
                            },
                        );
                    }
                }
                Packet::Sequence { sequence } => {
                    self.sent_packets.insert(
                        *sequence,
                        PacketSent {
                            sent_at,
                            info: PacketSentInfo::None,
                            largest_acked_packet: None,
                            acked_burst_trigger: None,
                        },
                    );
                }
            }

            Ok(acks_written)
        };

        // Get number of times to try to append acks to message packets.
        // - If we send acks with a message then we don't send an ack packet even if there is burst remaining.
        //   Burst favors one ack send per tick unless there are multiple messages to piggy-back on in a tick.
        let seq_expired = self.current_time >= self.next_sequenced_ack;
        let any_expired = self.current_time >= self.next_any_ack;
        let ack_expired = seq_expired || any_expired;
        let message_max_acks = self.ack_burst_count.max(if ack_expired { 1 } else { 0 });
        let mut acks_count_sent = 0;
        let mut sequenced_count_sent: u64 = 0;

        // Get packets from each channel
        let mut available_bytes = self.available_bytes_per_tick;
        for order in self.channel_send_order.iter() {
            match order {
                ChannelOrder::Reliable(channel_id) => {
                    let SendChannel::Reliable(channel) = self.send_channels.get_mut(*channel_id as usize).unwrap() else {
                        panic!("Packet to send has invalid channel {channel_id}");
                    };
                    for packet in channel.get_packets_to_send(&mut self.packet_sequence, &mut available_bytes, self.current_time) {
                        let add_acks = if acks_count_sent < message_max_acks { Some(None) } else { None };
                        if consume_packet_fn(packet, add_acks)? {
                            acks_count_sent += 1;
                        }
                        // Reliable packets have a built-in sequence
                        sequenced_count_sent += 1;
                    }
                }
                ChannelOrder::Unreliable(channel_id) => {
                    let SendChannel::Unreliable(channel) = self.send_channels.get_mut(*channel_id as usize).unwrap() else {
                        panic!("Packet to send has invalid channel {channel_id}");
                    };
                    for packet in channel.get_packets_to_send(&mut available_bytes) {
                        let add_acks = match (acks_count_sent < message_max_acks, sequenced_count_sent == 0 && seq_expired) {
                            // No acks
                            (false, _) => None,
                            // Acks without sequence
                            (true, false) => Some(None),
                            // Acks with sequence
                            (true, true) => Some(Some(self.packet_sequence)),
                        };
                        if consume_packet_fn(packet, add_acks)? {
                            acks_count_sent += 1;
                            if let Some(Some(_sequence)) = add_acks {
                                sequenced_count_sent += 1;
                                self.packet_sequence += 1;
                            }
                        }
                    }
                }
            }
        }

        // Send an explicit ack message if acks still need to be sent
        // - We don't add an ack packet if we failed to append acks to a message unless burst count
        //   is nonzero or acks are required.
        {
            let burst_needed = acks_count_sent == 0 && self.ack_burst_count > 0 && has_pending_acks;
            let ack_needed = ack_expired && acks_count_sent == 0 && has_pending_acks;
            let seq_needed = ack_needed && seq_expired && sequenced_count_sent == 0;

            if burst_needed || ack_needed {
                let add_acks = if seq_needed { Some(Some(self.packet_sequence)) } else { Some(None) };
                if consume_packet_fn(&Packet::Ack, add_acks)? {
                    acks_count_sent += 1;
                    if let Some(Some(_sequence)) = add_acks {
                        sequenced_count_sent += 1;
                        self.packet_sequence += 1;
                    }
                } else {
                    log::warn!("failed writing acks to ack packet unexpectedly, trying to recover...");
                }
            }
        }

        // Fall back to a sequence message if our seq timer expired and there are no acks
        if seq_expired && sequenced_count_sent == 0 {
            let packet = Packet::Sequence {
                sequence: self.packet_sequence,
            };
            self.packet_sequence += 1;
            if consume_packet_fn(&packet, None)? {
                log::warn!("wrote acks to sequence packet unexpectedly...");
            }
            sequenced_count_sent += 1;
        }

        self.payloads_cache.push(acks_buffer);

        // Update state
        self.ack_burst_count = self.ack_burst_count.saturating_sub(acks_count_sent);

        if sequenced_count_sent > 0 {
            // Time when we must send an ack with sequence for initiating an rtt calc.
            self.next_sequenced_ack = self.current_time.saturating_add(self.ack_config.ack_send_timeout);
        }
        if acks_count_sent > 0 || !has_pending_acks {
            self.rtt_delay_ref = None;
            // Time when we must send an ack for reliability fallback and sent packet cleanup in the paired client.
            self.next_any_ack = self.current_time.saturating_add(self.ack_config.ack_send_timeout);

            // `!has_pending_acks` check: reset if there are no acks so the timer doesn't accidentally trigger an
            // ack send early after we receive the next sequence.
        }
        if self.rtt_delay_ref.is_none() && has_pending_acks && acks_count_sent == 0 {
            // In this case we are at the *start* of an ack delay. The delay will be from this point in the tick
            // where delay started to the same point where the delay ends (which is when we send a packet with acks).
            // If acks were sent in the previous tick but got lost, we treat the gap between this tick
            // and last tick as part of the rtt to account for that loss.
            self.rtt_delay_ref = Some(self.current_time);
        }
        self.stats.sent_packets(sequenced_count_sent, bytes_sent);

        // &self.payloads_ser has the payloads
        Ok(())
    }

    fn handle_incoming_ack_ranges(&mut self, mut ack_ranges: AckRangesIterator) -> Result<(), DisconnectReason> {
        // Create list with just new acks
        // This prevents DoS from huge ack ranges
        self.u64_scratch.clear();
        while let Some(range) = ack_ranges.next().map_err(DisconnectReason::PacketDeserialization)? {
            for (&sequence, _) in self.sent_packets.range(range) {
                self.u64_scratch.push(sequence)
            }
        }
        if self.u64_scratch.is_empty() {
            return Ok(());
        }

        // Delay refers to artificial delay added by the paired client to avoid a spammy ack loop.
        let mut update_rtt_fn = |sent_packet: &PacketSent, delay_ms: u64| {
            let mut rtt = (self.current_time - sent_packet.sent_at).as_secs_f64();
            rtt -= (delay_ms as f64) * 0.001;
            rtt = rtt.max(0.0);
            if self.rtt < f64::EPSILON {
                self.rtt = rtt;
            } else {
                self.rtt = self.rtt * 0.875 + rtt * 0.125;
            }
        };

        // Update rtt for only the last ack'd sequence if an rtt delay was specified.
        let rtt_delay_ms = ack_ranges.rtt_delay_ms;
        if let Some(rtt_delay_ms) = rtt_delay_ms {
            update_rtt_fn(self.sent_packets.get(self.u64_scratch.last().unwrap()).unwrap(), rtt_delay_ms);
        }

        for packet_sequence in self.u64_scratch.iter() {
            let sent_packet = self.sent_packets.remove(packet_sequence).unwrap();
            self.stats.acked_packet(sent_packet.sent_at, self.current_time);

            // Update rtt
            if rtt_delay_ms.is_none() {
                update_rtt_fn(&sent_packet, 0);
            }

            // Update reliable
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
                PacketSentInfo::Ack | PacketSentInfo::None => (),
            }

            // Update burst count
            // Ack bursts can be sent unsequenced (meaning they won't get acked), so this only gets used if
            // an ack burst piggy-backs on a reliable message that then gets acked or if ack bursts are slow
            // enough for the sequenced ack timeout to fire.
            if sent_packet.acked_burst_trigger == Some(self.ack_burst_trigger) {
                self.ack_burst_count = 0;
            }

            // Update acks
            if let Some(largest_acked_packet) = sent_packet.largest_acked_packet {
                Self::acked_largest(&mut self.pending_acks, largest_acked_packet);
            }
        }

        Ok(())
    }

    /// Invariants:
    /// - All ranges are non-empty
    /// - All ranges are separated by a gap of at least 1
    /// - No sequence equals u64::MAX
    fn add_pending_ack(&mut self, sequence: u64) {
        assert!(sequence != u64::MAX);
        if self.pending_acks.is_empty() {
            self.pending_acks.push(sequence..sequence + 1);
            self.rtt_delay_ref = None;
            return;
        }

        // Update rtt delay reference if we are adding a new highest sequence.
        // We don't set the ref to None since there may be acks that were delayed. We want
        // the paired client to only look at the highest ack it receives after this point for updating rtt.
        // For example, this could be an unreliable message that showed up halfway through the ack timer.
        // When the next ack packet is sent out, only 'half the timeout' delay would have been artificially
        // added to the round trip for this packet. Whereas an ambiguous amount of time may have been delayed for
        // other acks sent at the same time (and it's not efficient to store delays for every single ack).
        if let Some(rtt_delay_ref) = &mut self.rtt_delay_ref {
            let last = self.pending_acks.last().unwrap();
            if sequence >= last.end {
                *rtt_delay_ref = self.current_time;
            }
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

        if self.pending_acks.len() as u64 > MAX_ACK_RANGES {
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
    fn message_lifecycle() {
        let mut c1 = RenetClient::new(ConnectionConfig::test(), false);
        let mut c2 = RenetClient::new(ConnectionConfig::test(), false);
        c1.set_connected();
        c2.set_connected();

        // 0s: startup
        {
            // Send: sequence (timeout)
            {
                let payloads = c1.get_packets_to_send();
                let (recv_packet, recv_acks) = Packet::from_bytes(payloads[0].as_slice()).unwrap();
                assert!(matches!(recv_packet, Packet::Sequence { sequence: 0 }));
                assert!(recv_acks.is_none());

                for payload in payloads {
                    c2.process_packet(payload);
                }

                assert_eq!(c1.sent_packets.len(), 1);
                assert!(matches!(c1.sent_packets[&0].info, PacketSentInfo::None));
                assert_eq!(c1.pending_acks.iter().map(|r| r.end - r.start).sum::<u64>(), 0);

                // Check c2's progress
                assert_eq!(c2.sent_packets.len(), 0); // Nothing yet
                assert_eq!(c2.pending_acks.iter().map(|r| r.end - r.start).sum::<u64>(), 1); // Sequence 0
            }
            // Send: ack (timeout + acks to send) (sequenced)
            {
                let payloads = c2.get_packets_to_send();
                let (recv_packet, recv_acks) = Packet::from_bytes(payloads[0].as_slice()).unwrap();
                assert!(matches!(recv_packet, Packet::Ack));
                assert!(recv_acks.is_some());
                assert!(recv_acks.unwrap().sequence.is_some());

                for payload in payloads {
                    c1.process_packet(payload);
                }

                assert_eq!(c2.sent_packets.len(), 1);
                assert!(matches!(c2.sent_packets[&0].info, PacketSentInfo::Ack));
                assert_eq!(c2.pending_acks.iter().map(|r| r.end - r.start).sum::<u64>(), 1); // Sequence 0

                // Check c1's progress
                assert_eq!(c1.sent_packets.len(), 0); // Sequence 0 was acked
                assert_eq!(c1.pending_acks.iter().map(|r| r.end - r.start).sum::<u64>(), 1); // Ack w/ sequence 0
            }
        }

        // 0s: message c1 -> c2
        {
            // Send: message (no acks)
            {
                let message: Bytes = vec![5; 5].into();
                c1.send_message(DefaultChannel::ReliableUnordered, message);

                let payloads = c1.get_packets_to_send();
                let (recv_packet, recv_acks) = Packet::from_bytes(payloads[0].as_slice()).unwrap();
                assert!(matches!(recv_packet, Packet::SmallReliable { .. }));
                assert!(recv_acks.is_none());

                for payload in payloads {
                    c2.process_packet(payload);
                }

                // First sequence was acked
                assert_eq!(c1.sent_packets.len(), 1);
                assert!(matches!(c1.sent_packets[&1].info, PacketSentInfo::ReliableMessages { .. }));
                assert_eq!(c1.pending_acks.iter().map(|r| r.end - r.start).sum::<u64>(), 1); // Ack w/ sequence 0

                // Check c2's progress
                assert_eq!(c2.sent_packets.len(), 1); // Ack w/ sequence 0 was NOT acked
                assert_eq!(c2.pending_acks.iter().map(|r| r.end - r.start).sum::<u64>(), 2); // Seq 0, msg seq 1
            }
            {
                // c2 do nothing
            }
        }

        // 1s: c1 message resend
        {
            c1.update(Duration::from_secs(1));
            assert_eq!(c1.sent_packets.len(), 1); // No change
            assert_eq!(c1.pending_acks.iter().map(|r| r.end - r.start).sum::<u64>(), 1); // No change

            c2.update(Duration::from_secs(1));
            assert_eq!(c2.sent_packets.len(), 1); // No change
            assert_eq!(c2.pending_acks.iter().map(|r| r.end - r.start).sum::<u64>(), 2); // No Change

            // Send: message again (+ acks for timeout)
            {
                let payloads = c1.get_packets_to_send();
                let (recv_packet, recv_acks) = Packet::from_bytes(payloads[0].as_slice()).unwrap();
                assert!(matches!(recv_packet, Packet::SmallReliable { .. }));
                assert!(recv_acks.is_some());

                for payload in payloads {
                    c2.process_packet(payload);
                }

                assert_eq!(c1.sent_packets.len(), 2);
                assert!(matches!(c1.sent_packets[&1].info, PacketSentInfo::ReliableMessages { .. }));
                assert!(matches!(c1.sent_packets[&2].info, PacketSentInfo::ReliableMessages { .. }));
                assert_eq!(c1.pending_acks.iter().map(|r| r.end - r.start).sum::<u64>(), 1); // Ack w/ sequence 0

                // Check c2's progress
                assert_eq!(c2.sent_packets.len(), 0); // Has now received seq 0 ack
                assert_eq!(c2.pending_acks.iter().map(|r| r.end - r.start).sum::<u64>(), 2); // Messages w/ sequence 1, 2
            }
            {
                let payloads = c2.get_packets_to_send();
                let (recv_packet, recv_acks) = Packet::from_bytes(payloads[0].as_slice()).unwrap();
                assert!(matches!(recv_packet, Packet::Ack));
                assert!(recv_acks.is_some());

                for payload in payloads {
                    c1.process_packet(payload);
                }

                assert_eq!(c2.sent_packets.len(), 1); // Ack sequence 1
                assert!(matches!(c2.sent_packets[&1].info, PacketSentInfo::Ack));
                assert_eq!(c2.pending_acks.iter().map(|r| r.end - r.start).sum::<u64>(), 2); // Messages w/ sequence 1, 2

                // Check c1's progress
                assert_eq!(c1.sent_packets.len(), 0); // Everything acked
                assert_eq!(c1.pending_acks.iter().map(|r| r.end - r.start).sum::<u64>(), 1); // Ack w/ sequence 2
            }
        }

        // - 5s: send acks (timeout)
        {
            c1.update(Duration::from_secs(4));
            assert_eq!(c1.sent_packets.len(), 0); // No change
            assert_eq!(c1.pending_acks.iter().map(|r| r.end - r.start).sum::<u64>(), 1); // No change

            c2.update(Duration::from_secs(4));
            assert_eq!(c2.sent_packets.len(), 0); // Sent acks cleared
            assert_eq!(c2.pending_acks.iter().map(|r| r.end - r.start).sum::<u64>(), 2); // No Change

            // Send: acks for stale receives (timeout)
            {
                let payloads = c1.get_packets_to_send();
                let (recv_packet, recv_acks) = Packet::from_bytes(payloads[0].as_slice()).unwrap();
                assert!(matches!(recv_packet, Packet::Ack));
                assert!(recv_acks.is_some());

                for payload in payloads {
                    c2.process_packet(payload);
                }

                assert_eq!(c1.sent_packets.len(), 1); // Ack w/ sequence 3
                assert!(matches!(c1.sent_packets[&3].info, PacketSentInfo::Ack));
                assert_eq!(c1.pending_acks.iter().map(|r| r.end - r.start).sum::<u64>(), 1); // Ack w/ sequence 1

                // Check c2's progress
                assert_eq!(c2.sent_packets.len(), 0); // Has not sent anything
                assert_eq!(c2.pending_acks.iter().map(|r| r.end - r.start).sum::<u64>(), 3); // msg seq 1, msg seq 2, seq 3
            }
            {
                let payloads = c2.get_packets_to_send();
                let (recv_packet, recv_acks) = Packet::from_bytes(payloads[0].as_slice()).unwrap();
                assert!(matches!(recv_packet, Packet::Ack));
                assert!(recv_acks.is_some());

                for payload in payloads {
                    c1.process_packet(payload);
                }

                assert_eq!(c2.sent_packets.len(), 1); // Ack sequence 2
                assert!(matches!(c2.sent_packets[&2].info, PacketSentInfo::Ack));
                assert_eq!(c2.pending_acks.iter().map(|r| r.end - r.start).sum::<u64>(), 3); // msg seq 1, msg seq 2, seq 3

                // Check c1's progress
                assert_eq!(c1.sent_packets.len(), 0); // Everything acked
                assert_eq!(c1.pending_acks.iter().map(|r| r.end - r.start).sum::<u64>(), 1); // Ack w/ sequence 2
            }
        }
    }
}
