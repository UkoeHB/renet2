use bytes::Bytes;
use std::{fmt, ops::Range};

use crate::remote_connection::{MAX_ACK_RANGES, MAX_PAYLOAD_BYTES};

pub type Payload = Vec<u8>;

// Sliced messages are split into SLICE_SIZE bytes chunks
pub const SLICE_SIZE: usize = 1200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slice {
    pub message_id: u64,
    pub slice_index: usize,
    pub num_slices: usize,
    pub payload: Bytes,
}

/// All packets *may* be appended with a serialized [`AckRanges`].
/// If there are no ack ranges, nothing extra will be serialized.
#[derive(Debug, PartialEq, Eq)]
pub enum Packet {
    // Small messages in a reliable channel are aggregated and sent in this packet
    SmallReliable {
        sequence: u64,
        channel_id: u8,
        messages: Vec<(u64, Bytes)>,
    },
    // Small messages in a unreliable channel are aggregated and sent in this packet
    SmallUnreliable {
        sequence: u64,
        channel_id: u8,
        messages: Vec<Bytes>,
    },
    // A big unreliable message is sliced in multiples slice packets
    UnreliableSlice {
        sequence: u64,
        channel_id: u8,
        slice: Slice,
    },
    // A big reliable messages is sliced in multiples slice packets
    ReliableSlice {
        sequence: u64,
        channel_id: u8,
        slice: Slice,
    },
    KeepAlive {
        sequence: u64,
    },
}

/// Acks are saved in multiple ranges, all values in the ranges are considered acked.
///
/// Includes an rtt delay if these acks will be sent in a packet that comes after a keepalive timer delay.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct AckRanges {
    pub ack_ranges: Vec<Range<u64>>,
    /// Delay subtracted from rtt to account for forced delay from keepalive timer.
    /// Defined relative to the last sequence in the attached acks.
    pub rtt_delay_ms: Option<u64>,
}

impl AckRanges {
    const RTT_DELAY_FLAG: u8 = 1 << 6;
    //const _UNUSED_FLAG: u8 = 1 << 7;

    /// Consider these ranges:
    /// [20010..20020   ,  20035..20040]
    ///  <----10----><-15-><----5------>
    ///
    /// We can represented more compactly each range if we serialize it as a sequence of
    /// offsets starting with the first 'start', since the difference is usually small.
    /// The ranges would become before serializing:
    /// 20010 10 15 5
    ///   |   |  |  |
    ///   |   |  |  +---->  5: size of 20035..20040
    ///   |   |  +-------> 15: gap between ranges 20010..20020 and 20035..20040
    ///   |   +----------> 10: size of 20010..20020
    ///   +-----------> 20040: start of 20010..20020
    ///
    /// Ranges are prefixed with the number of ranges that are recorded.
    /// - This is *not* -1 because we can append acks to any packet type and need a way to detect if there are
    /// acks recorded.
    /// - Range count is <= 64, which means we have 2 extra bits.
    ///   - Bits 0-5: range count
    ///   - Bit 6: indicates if an rtt delay is appended
    ///   - Bit 7: unused
    ///
    /// Since ranges and gaps should always be at least size 1, we store them with -1.
    ///
    /// We can always reconstruct the ranges using the end of the previous one and the gap.
    ///
    /// Returns the number of bytes written.
    pub fn to_bytes(&self, b: &mut octets::OctetsMut) -> Result<usize, SerializationError> {
        if self.ack_ranges.is_empty() {
            return Err(SerializationError::InvalidAckRange);
        }
        let before = b.cap();

        // Iterate
        let mut it = self.ack_ranges.iter();

        // Write the total number of ranges
        const _: () = assert!(MAX_ACK_RANGES <= 64);
        let mut count = it.len() as u8;
        if count as u64 > MAX_ACK_RANGES {
            return Err(SerializationError::InvalidAckRange);
        }
        if self.rtt_delay_ms.is_some() {
            count |= Self::RTT_DELAY_FLAG;
        }
        b.put_u8(count)?;

        // Extract the first range (first in the iterator)
        let first = it.next().unwrap();
        let first_size = first.end - first.start;
        if first_size == 0 {
            return Err(SerializationError::InvalidAckRange);
        }

        b.put_varint(first.start)?;
        b.put_varint(first_size - 1)?;

        let mut previous_range_end = first.end;
        // For each subsequent range:
        for range in it {
            // Calculate the gap between the end of the previous range and the start of the current range
            let gap = range.start - previous_range_end;
            let range_size = range.end - range.start;
            if range_size == 0 || gap == 0 {
                return Err(SerializationError::InvalidAckRange);
            }

            b.put_varint(gap - 1)?;
            b.put_varint(range_size - 1)?;

            previous_range_end = range.end;
        }

        // Add rtt delay at the end
        if let Some(rtt_delay_ms) = self.rtt_delay_ms {
            b.put_varint(rtt_delay_ms)?;
        }

        Ok(before - b.cap())
    }
}

#[derive(Debug)]
pub struct SmallReliableIterator {
    len: u16,
    current_idx: u16,
    bytes: Bytes,
    bytes_pos: usize,
}

impl SmallReliableIterator {
    pub fn new(len: u16, bytes: Bytes) -> Self {
        Self {
            len,
            current_idx: 0,
            bytes,
            bytes_pos: 0,
        }
    }

    pub fn next(&mut self) -> Result<Option<(u64, Bytes)>, SerializationError> {
        if self.current_idx >= self.len {
            return Ok(None);
        }
        self.current_idx += 1;
        let mut b = octets::Octets::with_slice(&self.bytes[self.bytes_pos..]);
        let message_id = b.get_varint()?;
        let message_len = b.get_varint()? as usize;
        self.bytes_pos += b.off();
        if b.cap() < message_len {
            return Err(SerializationError::BufferTooShort);
        }
        let payload = self.bytes.slice(self.bytes_pos..(self.bytes_pos + message_len));
        self.bytes_pos += message_len;
        Ok(Some((message_id, payload)))
    }

    pub fn into_acks_iter(mut self) -> Result<Option<AckRangesIterator>, SerializationError> {
        // Exhaust remaining messages.
        while self.next()?.is_some() {}
        AckRangesIterator::try_deserialize(self.bytes.slice(self.bytes_pos..))
    }

    pub fn reset(&mut self) {
        self.current_idx = 0;
        self.bytes_pos = 0;
    }
}

#[derive(Debug)]
pub struct SmallUnreliableIterator {
    len: u16,
    current_idx: u16,
    bytes: Bytes,
    bytes_pos: usize,
}

impl SmallUnreliableIterator {
    pub fn new(len: u16, bytes: Bytes) -> Self {
        Self {
            len,
            current_idx: 0,
            bytes,
            bytes_pos: 0,
        }
    }

    pub fn next(&mut self) -> Result<Option<Bytes>, SerializationError> {
        if self.current_idx >= self.len {
            return Ok(None);
        }
        self.current_idx += 1;
        let mut b = octets::Octets::with_slice(&self.bytes[self.bytes_pos..]);
        let message_len = b.get_varint()? as usize;
        self.bytes_pos += b.off();
        if b.cap() < message_len {
            return Err(SerializationError::BufferTooShort);
        }
        let payload = self.bytes.slice(self.bytes_pos..(self.bytes_pos + message_len));
        self.bytes_pos += message_len;
        Ok(Some(payload))
    }

    pub fn into_acks_iter(mut self) -> Result<Option<AckRangesIterator>, SerializationError> {
        // Exhaust remaining messages.
        while self.next()?.is_some() {}
        AckRangesIterator::try_deserialize(self.bytes.slice(self.bytes_pos..))
    }

    pub fn reset(&mut self) {
        self.current_idx = 0;
        self.bytes_pos = 0;
    }
}

#[derive(Debug)]
pub struct AckRangesIterator {
    len: u64,
    has_rtt_delay: bool,
    first_range: Range<u64>,
    current_idx: u64,
    bytes: Bytes,
    bytes_pos: usize,
    prev_end: u64,
}

impl AckRangesIterator {
    pub fn new(len: u64, first_range: Range<u64>, bytes: Bytes, has_rtt_delay: bool) -> Self {
        Self {
            len,
            has_rtt_delay,
            first_range,
            current_idx: 0,
            bytes,
            bytes_pos: 0,
            prev_end: 0,
        }
    }

    pub fn try_deserialize(bytes: Bytes) -> Result<Option<Self>, SerializationError> {
        // Nothing is written if:
        // - bytes.is_empty()
        // - !bytes.is_empty() && bytes[0] == 0 (which will be the case for zero-initialized buffers)
        if bytes.is_empty() {
            return Ok(None);
        }
        let mut b = octets::Octets::with_slice(&bytes);
        let mut num_ranges = b.get_u8()?;
        let mut has_rtt_delay = false;
        if (num_ranges & AckRanges::RTT_DELAY_FLAG) > 0 {
            has_rtt_delay = true;
            num_ranges &= !AckRanges::RTT_DELAY_FLAG;
        }
        if num_ranges == 0 {
            return Ok(None);
        } else if num_ranges as u64 > MAX_ACK_RANGES {
            return Err(SerializationError::InvalidAckRange);
        }
        let first_range_start = b.get_varint()?;
        let first_range_size = b.get_varint()?.checked_add(1).ok_or(SerializationError::InvalidAckRange)?;
        let first_range_end = first_range_start
            .checked_add(first_range_size)
            .ok_or(SerializationError::InvalidAckRange)?;

        Ok(Some(Self::new(
            num_ranges as u64,
            first_range_start..first_range_end,
            bytes.slice(b.off()..),
            has_rtt_delay,
        )))
    }

    pub fn next(&mut self) -> Result<Option<Range<u64>>, SerializationError> {
        if self.current_idx >= self.len {
            return Ok(None);
        }
        if self.current_idx == 0 {
            self.current_idx += 1;
            self.prev_end = self.first_range.end;
            return Ok(Some(self.first_range.clone()));
        }
        self.current_idx += 1;

        // Get the gap between the previous range and the current one
        let mut b = octets::Octets::with_slice(&self.bytes[self.bytes_pos..]);
        let gap = b.get_varint()?.checked_add(1).ok_or(SerializationError::InvalidAckRange)?;

        // Get the end of the current range using the start of the previous one and the gap
        let range_start = self.prev_end.checked_add(gap).ok_or(SerializationError::InvalidAckRange)?;
        let range_size = b.get_varint()?.checked_add(1).ok_or(SerializationError::InvalidAckRange)?;
        let range_end = range_start.checked_add(range_size).ok_or(SerializationError::InvalidAckRange)?;
        self.prev_end = range_end;
        self.bytes_pos += b.off();

        Ok(Some(range_start..range_end))
    }

    pub fn reset(&mut self) {
        self.current_idx = 0;
        self.bytes_pos = 0;
        self.prev_end = 0;
    }

    /// Does not call `self.reset` - will fail if delay was already read.
    pub fn rtt_delay_ms(&mut self) -> Result<Option<u64>, SerializationError> {
        if !self.has_rtt_delay {
            return Ok(None);
        }
        // Skip past ack ranges.
        while self.next()?.is_some() {}

        // Deserialize delay
        let mut b = octets::Octets::with_slice(&self.bytes[self.bytes_pos..]);
        let rtt_delay_ms = b.get_varint()?;
        self.bytes_pos += b.off();

        Ok(Some(rtt_delay_ms))
    }

    pub fn make_acks(&mut self, acks: &mut AckRanges) -> Result<(), SerializationError> {
        self.reset();
        acks.ack_ranges.clear();
        acks.ack_ranges.reserve(self.len as usize);
        while let Some(range) = self.next()? {
            acks.ack_ranges.push(range);
        }
        acks.rtt_delay_ms = self.rtt_delay_ms()?;
        Ok(())
    }
}

/// A partially deserialized packet.
///
/// Use this to process packets without allocating excessively.
#[derive(Debug)]
pub enum PacketPartialDeser {
    SmallReliable {
        sequence: u64,
        channel_id: u8,
        // Can obtain `Option<AckRangesIterator>` after iterating messages.
        messages: SmallReliableIterator,
    },
    SmallUnreliable {
        sequence: u64,
        channel_id: u8,
        // Can obtain `Option<AckRangesIterator>` after iterating messages.
        messages: SmallUnreliableIterator,
    },
    UnreliableSlice {
        sequence: u64,
        channel_id: u8,
        slice: Slice,
        ack_ranges: Option<AckRangesIterator>,
    },
    ReliableSlice {
        sequence: u64,
        channel_id: u8,
        slice: Slice,
        ack_ranges: Option<AckRangesIterator>,
    },
    KeepAlive {
        sequence: u64,
        ack_ranges: Option<AckRangesIterator>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerializationError {
    BufferTooShort,
    InvalidNumSlices,
    SliceSizeAboveLimit,
    EmptySlice,
    InvalidAckRange,
    InvalidPacketType,
    InvalidSequence,
}

impl std::error::Error for SerializationError {}

impl fmt::Display for SerializationError {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        use SerializationError::*;

        match *self {
            BufferTooShort => write!(fmt, "buffer too short"),
            InvalidNumSlices => write!(fmt, "invalid number of slices"),
            InvalidAckRange => write!(fmt, "invalid ack range"),
            InvalidPacketType => write!(fmt, "invalid packet type"),
            SliceSizeAboveLimit => write!(fmt, "invalid slice size, it's above the limit of {} bytes", SLICE_SIZE),
            EmptySlice => write!(fmt, "invalid slice, slices cannot be empty"),
            InvalidSequence => write!(fmt, "invalid sequence"),
        }
    }
}

impl From<octets::BufferTooShortError> for SerializationError {
    fn from(_: octets::BufferTooShortError) -> Self {
        SerializationError::BufferTooShort
    }
}

impl Packet {
    /// Invariant after deserialize: `< u64::MAX`.
    #[allow(unused)]
    pub fn sequence(&self) -> u64 {
        match self {
            Self::SmallReliable { sequence, .. }
            | Self::SmallUnreliable { sequence, .. }
            | Self::UnreliableSlice { sequence, .. }
            | Self::ReliableSlice { sequence, .. }
            | Self::KeepAlive { sequence, .. } => *sequence,
        }
    }

    /// Reliable packets should be acked asap.
    /// Non-reliable packet acks may be delayed until the next keepalive.
    #[allow(unused)]
    pub fn is_reliable(&self) -> bool {
        match self {
            Self::SmallReliable { .. } | Self::ReliableSlice { .. } => true,
            Self::SmallUnreliable { .. } | Self::UnreliableSlice { .. } | Self::KeepAlive { .. } => false,
        }
    }

    /// Returns number of bytes written to `b` and a flag indicating if acks were written.
    ///
    /// `acks` will be appended if it will fit with the message in [`MAX_PAYLOAD_BYTES`].
    pub fn to_bytes(&self, b: &mut octets::OctetsMut, acks: Option<&[u8]>) -> Result<(usize, bool), SerializationError> {
        let before = b.cap();
        let acks_written;

        match self {
            Self::SmallReliable {
                sequence,
                channel_id,
                messages,
            } => {
                b.put_u8(0)?;
                b.put_varint(*sequence)?;
                b.put_u8(*channel_id)?;
                b.put_u16(messages.len() as u16)?;
                for (message_id, message) in messages {
                    b.put_varint(*message_id)?;
                    b.put_varint(message.len() as u64)?;
                    b.put_bytes(message)?;
                }
                acks_written = Self::try_write_acks(b, acks)?;
            }
            Self::SmallUnreliable {
                sequence,
                channel_id,
                messages,
            } => {
                b.put_u8(1)?;
                b.put_varint(*sequence)?;
                b.put_u8(*channel_id)?;
                b.put_u16(messages.len() as u16)?;
                for message in messages {
                    b.put_varint(message.len() as u64)?;
                    b.put_bytes(message)?;
                }
                acks_written = Self::try_write_acks(b, acks)?;
            }
            Self::ReliableSlice {
                sequence,
                channel_id,
                slice,
            } => {
                b.put_u8(2)?;
                b.put_varint(*sequence)?;
                b.put_u8(*channel_id)?;
                b.put_varint(slice.message_id)?;
                b.put_varint(slice.slice_index as u64)?;
                b.put_varint(slice.num_slices as u64)?;
                b.put_varint(slice.payload.len() as u64)?;
                b.put_bytes(&slice.payload)?;
                acks_written = Self::try_write_acks(b, acks)?;
            }
            Self::UnreliableSlice {
                sequence,
                channel_id,
                slice,
            } => {
                b.put_u8(3)?;
                b.put_varint(*sequence)?;
                b.put_u8(*channel_id)?;
                b.put_varint(slice.message_id)?;
                b.put_varint(slice.slice_index as u64)?;
                b.put_varint(slice.num_slices as u64)?;
                b.put_varint(slice.payload.len() as u64)?;
                b.put_bytes(&slice.payload)?;
                acks_written = Self::try_write_acks(b, acks)?;
            }
            Self::KeepAlive { sequence } => {
                b.put_u8(4)?;
                b.put_varint(*sequence)?;
                acks_written = Self::try_write_acks(b, acks)?;
            }
        }

        Ok((before - b.cap(), acks_written))
    }

    /// See [`PacketPartialDeser`] for the non-allocating version if you just want to process packets
    /// and discard them.
    #[allow(unused)]
    pub fn from_bytes(data: &[u8]) -> Result<(Self, Option<AckRanges>), SerializationError> {
        PacketPartialDeser::from_raw_bytes(data)?.into_packet()
    }

    fn try_write_acks(b: &mut octets::OctetsMut, acks: Option<&[u8]>) -> Result<bool, SerializationError> {
        if let Some(acks) = acks
            && b.off() <= MAX_PAYLOAD_BYTES
            && acks.len() <= MAX_PAYLOAD_BYTES - b.off()
        {
            b.put_bytes(acks)?;
            return Ok(true);
        }
        Ok(false)
    }
}

impl PacketPartialDeser {
    /// Invariant after deserialize: `< u64::MAX`.
    pub fn sequence(&self) -> u64 {
        match self {
            Self::SmallReliable { sequence, .. }
            | Self::SmallUnreliable { sequence, .. }
            | Self::UnreliableSlice { sequence, .. }
            | Self::ReliableSlice { sequence, .. }
            | Self::KeepAlive { sequence, .. } => *sequence,
        }
    }

    /// Reliable packets should be acked asap.
    /// Non-reliable packet acks may be delayed until the next keepalive.
    #[allow(unused)]
    pub fn is_reliable(&self) -> bool {
        match self {
            Self::SmallReliable { .. } | Self::ReliableSlice { .. } => true,
            Self::SmallUnreliable { .. } | Self::UnreliableSlice { .. } | Self::KeepAlive { .. } => false,
        }
    }

    /// Allocates once
    pub fn from_raw_bytes(data: &[u8]) -> Result<Self, SerializationError> {
        Self::from_bytes(Bytes::copy_from_slice(data))
    }

    pub fn from_bytes(bytes: Bytes) -> Result<Self, SerializationError> {
        let mut b = octets::Octets::with_slice(&bytes);

        let packet_type = b.get_u8()?;
        match packet_type {
            0 => {
                // SmallReliable
                let sequence = Self::deserialize_sequence(&mut b)?;
                let channel_id = b.get_u8()?;
                let messages_len = b.get_u16()?;
                let messages = SmallReliableIterator::new(messages_len, bytes.slice(b.off()..));
                Ok(Self::SmallReliable {
                    sequence,
                    channel_id,
                    messages,
                })
            }
            1 => {
                // SmallUnreliable
                let sequence = Self::deserialize_sequence(&mut b)?;
                let channel_id = b.get_u8()?;
                let messages_len = b.get_u16()?;
                let messages = SmallUnreliableIterator::new(messages_len, bytes.slice(b.off()..));
                Ok(Self::SmallUnreliable {
                    sequence,
                    channel_id,
                    messages,
                })
            }
            2 => {
                // ReliableSlice
                let sequence = Self::deserialize_sequence(&mut b)?;
                let channel_id = b.get_u8()?;
                let message_id = b.get_varint()?;
                let slice_index = b.get_varint()? as usize;
                let num_slices = b.get_varint()? as usize;
                if num_slices == 0 || num_slices > 1_000_000 {
                    return Err(SerializationError::InvalidNumSlices);
                }

                let message_len = b.get_varint()? as usize;
                if b.cap() < message_len {
                    return Err(SerializationError::BufferTooShort);
                }
                let payload = bytes.slice(b.off()..b.off() + message_len);
                if payload.is_empty() {
                    return Err(SerializationError::EmptySlice);
                }
                if payload.len() > SLICE_SIZE {
                    return Err(SerializationError::SliceSizeAboveLimit);
                }
                let pos = b.off() + message_len;
                let ack_ranges = AckRangesIterator::try_deserialize(bytes.slice(pos..))?;

                let slice = Slice {
                    message_id,
                    slice_index,
                    num_slices,
                    payload,
                };
                Ok(Self::ReliableSlice {
                    sequence,
                    channel_id,
                    slice,
                    ack_ranges,
                })
            }
            3 => {
                // UnreliableSlice
                let sequence = Self::deserialize_sequence(&mut b)?;
                let channel_id = b.get_u8()?;
                let message_id = b.get_varint()?;
                let slice_index = b.get_varint()? as usize;
                let num_slices = b.get_varint()? as usize;
                if num_slices == 0 || num_slices > 1_000_000 {
                    return Err(SerializationError::InvalidNumSlices);
                }

                let message_len = b.get_varint()? as usize;
                if b.cap() < message_len {
                    return Err(SerializationError::BufferTooShort);
                }
                let payload = bytes.slice(b.off()..b.off() + message_len);
                if payload.is_empty() {
                    return Err(SerializationError::EmptySlice);
                }
                if payload.len() > SLICE_SIZE {
                    return Err(SerializationError::SliceSizeAboveLimit);
                }
                let pos = b.off() + message_len;
                let ack_ranges = AckRangesIterator::try_deserialize(bytes.slice(pos..))?;

                let slice = Slice {
                    message_id,
                    slice_index,
                    num_slices,
                    payload,
                };
                Ok(Self::UnreliableSlice {
                    sequence,
                    channel_id,
                    slice,
                    ack_ranges,
                })
            }
            4 => {
                // KeepAlive
                let sequence = Self::deserialize_sequence(&mut b)?;
                let pos = b.off();
                let ack_ranges = AckRangesIterator::try_deserialize(bytes.slice(pos..))?;
                Ok(Self::KeepAlive { sequence, ack_ranges })
            }
            _ => Err(SerializationError::InvalidPacketType),
        }
    }

    /// Fully deserialize to a [`Packet`] and [`AckRanges`].
    pub fn into_packet(self) -> Result<(Packet, Option<AckRanges>), SerializationError> {
        match self {
            Self::SmallReliable {
                sequence,
                channel_id,
                mut messages,
            } => {
                messages.reset();
                let mut accumulated = Vec::with_capacity(messages.len as usize);
                while let Some((message_id, message)) = messages.next()? {
                    accumulated.push((message_id, message));
                }
                let maybe_acks = Self::get_maybe_acks(messages.into_acks_iter()?)?;

                Ok((
                    Packet::SmallReliable {
                        sequence,
                        channel_id,
                        messages: accumulated,
                    },
                    maybe_acks,
                ))
            }
            Self::SmallUnreliable {
                sequence,
                channel_id,
                mut messages,
            } => {
                messages.reset();
                let mut accumulated = Vec::with_capacity(messages.len as usize);
                while let Some(message) = messages.next()? {
                    accumulated.push(message);
                }
                let maybe_acks = Self::get_maybe_acks(messages.into_acks_iter()?)?;

                Ok((
                    Packet::SmallUnreliable {
                        sequence,
                        channel_id,
                        messages: accumulated,
                    },
                    maybe_acks,
                ))
            }
            Self::ReliableSlice {
                sequence,
                channel_id,
                slice,
                ack_ranges,
            } => {
                let maybe_acks = Self::get_maybe_acks(ack_ranges)?;
                Ok((
                    Packet::ReliableSlice {
                        sequence,
                        channel_id,
                        slice,
                    },
                    maybe_acks,
                ))
            }
            Self::UnreliableSlice {
                sequence,
                channel_id,
                slice,
                ack_ranges,
            } => {
                let maybe_acks = Self::get_maybe_acks(ack_ranges)?;
                Ok((
                    Packet::UnreliableSlice {
                        sequence,
                        channel_id,
                        slice,
                    },
                    maybe_acks,
                ))
            }
            Self::KeepAlive { sequence, ack_ranges } => {
                let maybe_acks = Self::get_maybe_acks(ack_ranges)?;
                Ok((Packet::KeepAlive { sequence }, maybe_acks))
            }
        }
    }

    fn deserialize_sequence(b: &mut octets::Octets) -> Result<u64, SerializationError> {
        let sequence = b.get_varint()?;
        // Sequence ranges end in sequence + 1 so we must check this.
        if sequence == u64::MAX {
            return Err(SerializationError::InvalidSequence);
        }
        Ok(sequence)
    }

    fn get_maybe_acks(maybe_it: Option<AckRangesIterator>) -> Result<Option<AckRanges>, SerializationError> {
        let Some(mut it) = maybe_it else { return Ok(None) };
        let mut acks = AckRanges::default();
        it.make_acks(&mut acks)?;
        Ok(Some(acks))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_small_reliable_packet() {
        let mut buffer = [0u8; MAX_PAYLOAD_BYTES];
        let packet = Packet::SmallReliable {
            sequence: 0,
            channel_id: 0,
            messages: vec![(0, vec![0, 0, 0].into()), (1, vec![1, 1, 1].into()), (2, vec![2, 2, 2].into())],
        };

        let mut b = octets::OctetsMut::with_slice(&mut buffer);
        packet.to_bytes(&mut b, None).unwrap();

        let (recv_packet, recv_acks) = Packet::from_bytes(buffer.as_slice()).unwrap();
        assert_eq!(recv_packet, packet);
        assert_eq!(recv_acks, None);
    }

    #[test]
    fn serialize_small_reliable_packet_with_acks() {
        let mut ack_buffer = [0u8; MAX_PAYLOAD_BYTES];
        let mut buffer = [0u8; MAX_PAYLOAD_BYTES];
        let packet = Packet::SmallReliable {
            sequence: 0,
            channel_id: 0,
            messages: vec![(0, vec![0, 0, 0].into()), (1, vec![1, 1, 1].into()), (2, vec![2, 2, 2].into())],
        };
        let acks = AckRanges {
            ack_ranges: vec![3..7, 8..20],
            rtt_delay_ms: None,
        };

        let mut acks_b = octets::OctetsMut::with_slice(&mut ack_buffer);
        let acks_len = acks.to_bytes(&mut acks_b).unwrap();

        let mut b = octets::OctetsMut::with_slice(&mut buffer);
        let (_, acks_written) = packet.to_bytes(&mut b, Some(&ack_buffer[..acks_len])).unwrap();
        assert!(acks_written);

        let (recv_packet, recv_acks) = Packet::from_bytes(buffer.as_slice()).unwrap();
        assert_eq!(recv_packet, packet);
        assert_eq!(recv_acks, Some(acks));
    }

    #[test]
    fn serialize_small_unreliable_packet() {
        let mut buffer = [0u8; MAX_PAYLOAD_BYTES];
        let packet = Packet::SmallUnreliable {
            sequence: 0,
            channel_id: 0,
            messages: vec![vec![0, 0, 0].into(), vec![1, 1, 1].into(), vec![2, 2, 2].into()],
        };

        let mut b = octets::OctetsMut::with_slice(&mut buffer);
        packet.to_bytes(&mut b, None).unwrap();

        let (recv_packet, recv_acks) = Packet::from_bytes(buffer.as_slice()).unwrap();
        assert_eq!(recv_packet, packet);
        assert_eq!(recv_acks, None);
    }

    #[test]
    fn serialize_small_unreliable_packet_with_acks() {
        let mut ack_buffer = [0u8; MAX_PAYLOAD_BYTES];
        let mut buffer = [0u8; MAX_PAYLOAD_BYTES];
        let packet = Packet::SmallUnreliable {
            sequence: 0,
            channel_id: 0,
            messages: vec![vec![0, 0, 0].into(), vec![1, 1, 1].into(), vec![2, 2, 2].into()],
        };
        let acks = AckRanges {
            ack_ranges: vec![3..7, 8..20],
            rtt_delay_ms: None,
        };

        let mut acks_b = octets::OctetsMut::with_slice(&mut ack_buffer);
        let acks_len = acks.to_bytes(&mut acks_b).unwrap();

        let mut b = octets::OctetsMut::with_slice(&mut buffer);
        let (_, acks_written) = packet.to_bytes(&mut b, Some(&ack_buffer[..acks_len])).unwrap();
        assert!(acks_written);

        let (recv_packet, recv_acks) = Packet::from_bytes(buffer.as_slice()).unwrap();
        assert_eq!(recv_packet, packet);
        assert_eq!(recv_acks, Some(acks));
    }

    #[test]
    fn serialize_reliable_slice_packet() {
        let mut buffer = [0u8; MAX_PAYLOAD_BYTES];

        let packet = Packet::ReliableSlice {
            sequence: 0,
            channel_id: 0,
            slice: Slice {
                message_id: 0,
                slice_index: 0,
                num_slices: 1,
                payload: vec![5; SLICE_SIZE].into(),
            },
        };

        let mut b = octets::OctetsMut::with_slice(&mut buffer);
        let (_, acks_written) = packet.to_bytes(&mut b, None).unwrap();
        assert!(!acks_written);

        let (recv_packet, recv_acks) = Packet::from_bytes(buffer.as_slice()).unwrap();
        assert_eq!(recv_packet, packet);
        assert_eq!(recv_acks, None);
    }

    #[test]
    fn serialize_reliable_slice_packet_with_acks() {
        let mut ack_buffer = [0u8; MAX_PAYLOAD_BYTES];
        let acks = AckRanges {
            ack_ranges: vec![3..7, 8..20],
            rtt_delay_ms: None,
        };
        let mut acks_b = octets::OctetsMut::with_slice(&mut ack_buffer);
        let acks_len = acks.to_bytes(&mut acks_b).unwrap();

        let mut buffer = [0u8; MAX_PAYLOAD_BYTES];
        let packet = Packet::ReliableSlice {
            sequence: 0,
            channel_id: 0,
            slice: Slice {
                message_id: 0,
                slice_index: 0,
                num_slices: 1,
                payload: vec![5; MAX_PAYLOAD_BYTES / 2].into(),
            },
        };
        let mut b = octets::OctetsMut::with_slice(&mut buffer);
        let (_, acks_written) = packet.to_bytes(&mut b, Some(&ack_buffer[..acks_len])).unwrap();
        assert!(acks_written);

        let (recv_packet, recv_acks) = Packet::from_bytes(buffer.as_slice()).unwrap();
        assert_eq!(recv_packet, packet);
        assert_eq!(recv_acks, Some(acks));
    }

    #[test]
    fn serialize_unreliable_slice_packet() {
        let mut buffer = [0u8; MAX_PAYLOAD_BYTES];

        let packet = Packet::UnreliableSlice {
            sequence: 0,
            channel_id: 0,
            slice: Slice {
                message_id: 0,
                slice_index: 0,
                num_slices: 1,
                payload: vec![5; SLICE_SIZE].into(),
            },
        };

        let mut b = octets::OctetsMut::with_slice(&mut buffer);
        let (_, acks_written) = packet.to_bytes(&mut b, None).unwrap();
        assert!(!acks_written);

        let (recv_packet, recv_acks) = Packet::from_bytes(buffer.as_slice()).unwrap();
        assert_eq!(recv_packet, packet);
        assert_eq!(recv_acks, None);
    }

    #[test]
    fn serialize_unreliable_slice_packet_with_acks() {
        let mut ack_buffer = [0u8; MAX_PAYLOAD_BYTES];
        let acks = AckRanges {
            ack_ranges: vec![3..7, 8..20],
            rtt_delay_ms: None,
        };
        let mut acks_b = octets::OctetsMut::with_slice(&mut ack_buffer);
        let acks_len = acks.to_bytes(&mut acks_b).unwrap();

        let mut buffer = [0u8; MAX_PAYLOAD_BYTES];
        let packet = Packet::UnreliableSlice {
            sequence: 0,
            channel_id: 0,
            slice: Slice {
                message_id: 0,
                slice_index: 0,
                num_slices: 1,
                payload: vec![5; MAX_PAYLOAD_BYTES / 2].into(),
            },
        };
        let mut b = octets::OctetsMut::with_slice(&mut buffer);
        let (_, acks_written) = packet.to_bytes(&mut b, Some(&ack_buffer[..acks_len])).unwrap();
        assert!(acks_written);

        let (recv_packet, recv_acks) = Packet::from_bytes(buffer.as_slice()).unwrap();
        assert_eq!(recv_packet, packet);
        assert_eq!(recv_acks, Some(acks));
    }

    #[test]
    fn serialize_keepalive_packet() {
        let mut buffer = [0u8; MAX_PAYLOAD_BYTES];

        let packet = Packet::KeepAlive { sequence: 222 };

        let mut b = octets::OctetsMut::with_slice(&mut buffer);
        let (_, acks_written) = packet.to_bytes(&mut b, None).unwrap();
        assert!(!acks_written);

        let (recv_packet, recv_acks) = Packet::from_bytes(buffer.as_slice()).unwrap();
        assert_eq!(recv_packet, packet);
        assert_eq!(recv_acks, None);
    }

    #[test]
    fn serialize_keepalive_packet_with_ack() {
        let mut ack_buffer = [0u8; MAX_PAYLOAD_BYTES];
        let mut buffer = [0u8; MAX_PAYLOAD_BYTES];

        let packet = Packet::KeepAlive { sequence: 0 };
        let acks = AckRanges {
            ack_ranges: vec![3..7, 10..20, 30..100],
            rtt_delay_ms: None,
        };

        let mut acks_b = octets::OctetsMut::with_slice(&mut ack_buffer);
        let acks_len = acks.to_bytes(&mut acks_b).unwrap();

        let mut b = octets::OctetsMut::with_slice(&mut buffer);
        let (_, acks_written) = packet.to_bytes(&mut b, Some(&ack_buffer[..acks_len])).unwrap();
        assert!(acks_written);

        let (recv_packet, recv_acks) = Packet::from_bytes(buffer.as_slice()).unwrap();
        assert_eq!(recv_packet, packet);
        assert_eq!(recv_acks, Some(acks));
    }

    #[test]
    fn serialize_keepalive_packet_with_delay() {
        let mut ack_buffer = [0u8; MAX_PAYLOAD_BYTES];
        let mut buffer = [0u8; MAX_PAYLOAD_BYTES];

        let packet = Packet::KeepAlive { sequence: 20 };
        let acks = AckRanges {
            ack_ranges: vec![3..7, 10..20, 30..100],
            rtt_delay_ms: Some(555),
        };

        let mut acks_b = octets::OctetsMut::with_slice(&mut ack_buffer);
        let acks_len = acks.to_bytes(&mut acks_b).unwrap();

        let mut b = octets::OctetsMut::with_slice(&mut buffer);
        let (_, acks_written) = packet.to_bytes(&mut b, Some(&ack_buffer[..acks_len])).unwrap();
        assert!(acks_written);

        let (recv_packet, recv_acks) = Packet::from_bytes(buffer.as_slice()).unwrap();
        assert_eq!(recv_packet, packet);
        assert_eq!(recv_acks, Some(acks));
    }

    #[test]
    fn serialize_ack_ranges_empty() {
        let mut ack_buffer = [0u8; MAX_PAYLOAD_BYTES];

        let acks = AckRanges {
            // zero ranges not allowed
            ack_ranges: vec![],
            rtt_delay_ms: None,
        };

        let mut acks_b = octets::OctetsMut::with_slice(&mut ack_buffer);
        assert!(acks.to_bytes(&mut acks_b).is_err());
    }

    #[test]
    fn serialize_ack_ranges_err_size() {
        let mut ack_buffer = [0u8; MAX_PAYLOAD_BYTES];

        let acks = AckRanges {
            // empty range not allowed
            ack_ranges: vec![3..7, 10..20, 22..22],
            rtt_delay_ms: None,
        };

        let mut acks_b = octets::OctetsMut::with_slice(&mut ack_buffer);
        assert!(acks.to_bytes(&mut acks_b).is_err());
    }

    #[test]
    fn serialize_ack_ranges_err_gap() {
        let mut ack_buffer = [0u8; MAX_PAYLOAD_BYTES];

        let acks = AckRanges {
            // gap of 0 not allowed
            ack_ranges: vec![3..7, 7..20],
            rtt_delay_ms: None,
        };

        let mut acks_b = octets::OctetsMut::with_slice(&mut ack_buffer);
        assert!(acks.to_bytes(&mut acks_b).is_err());
    }
}
