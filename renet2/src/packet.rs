use bytes::Bytes;
use std::{fmt, ops::Range};

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
    // Contains the packets that were acked
    // Acks are saved in multiple ranges, all values in the ranges are considered acked.
    Ack {
        sequence: u64,
        ack_ranges: Vec<Range<u64>>,
    },
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

    pub fn reset(&mut self) {
        self.current_idx = 0;
        self.bytes_pos = 0;
    }
}

#[derive(Debug)]
pub struct AckRangesIterator {
    first_range: Range<u64>,
    len: u64,
    current_idx: u64,
    bytes: Bytes,
    bytes_pos: usize,
    prev_end: u64,
}

impl AckRangesIterator {
    pub fn new(first_range: Range<u64>, len_extra: u64, bytes: Bytes) -> Self {
        Self {
            first_range,
            len: len_extra + 1,
            current_idx: 0,
            bytes,
            bytes_pos: 0,
            prev_end: 0,
        }
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

        if self.prev_end > u64::MAX - gap {
            return Err(SerializationError::InvalidAckRange);
        }

        // Get the end of the current range using the start of the previous one and the gap
        let range_start = self.prev_end + gap;
        let range_size = b.get_varint()?.checked_add(1).ok_or(SerializationError::InvalidAckRange)?;

        if range_start > u64::MAX - range_size {
            return Err(SerializationError::InvalidAckRange);
        }

        let range_end = range_start + range_size;
        self.prev_end = range_end;
        self.bytes_pos += b.off();

        Ok(Some(range_start..range_end))
    }

    pub fn reset(&mut self) {
        self.current_idx = 0;
        self.bytes_pos = 0;
        self.prev_end = 0;
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
        messages: SmallReliableIterator,
    },
    SmallUnreliable {
        sequence: u64,
        channel_id: u8,
        messages: SmallUnreliableIterator,
    },
    UnreliableSlice {
        sequence: u64,
        channel_id: u8,
        slice: Slice,
    },
    ReliableSlice {
        sequence: u64,
        channel_id: u8,
        slice: Slice,
    },
    Ack {
        sequence: u64,
        ack_ranges: AckRangesIterator,
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
        }
    }
}

impl From<octets::BufferTooShortError> for SerializationError {
    fn from(_: octets::BufferTooShortError) -> Self {
        SerializationError::BufferTooShort
    }
}

impl Packet {
    #[allow(unused)]
    pub fn sequence(&self) -> u64 {
        match self {
            Packet::SmallReliable { sequence, .. }
            | Packet::SmallUnreliable { sequence, .. }
            | Packet::UnreliableSlice { sequence, .. }
            | Packet::ReliableSlice { sequence, .. }
            | Packet::Ack { sequence, .. } => *sequence,
        }
    }

    /// Returns number of bytes written to `b`.
    pub fn to_bytes(&self, b: &mut octets::OctetsMut) -> Result<usize, SerializationError> {
        let before = b.cap();

        match self {
            Packet::SmallReliable {
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
            }
            Packet::SmallUnreliable {
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
            }
            Packet::ReliableSlice {
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
            }
            Packet::UnreliableSlice {
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
            }
            Packet::Ack { sequence, ack_ranges } => {
                b.put_u8(4)?;
                b.put_varint(*sequence)?;

                // Consider this ranges:
                // [20010..20020   ,  20035..20040]
                //  <----10----><-15-><----5------>
                //
                // We can represented more compactly each range if we serialize it as a sequence of
                // offsets starting with the first 'start', since the difference is usually small.
                // The ranges would become before serializing:
                // 20010 10 1 15 5
                //   |   |  | |  |
                //   |   |  | |  +---> 5: size of 20035..20040
                //   |   |  | +-----> 15: gap between ranges 20010..20020 and 20035..20040
                //   |   |  +--------> 1: remaining number of ranges
                //   |   +----------> 10: size of 20010..20020
                //   +-----------> 20040: start of 20010..20020
                //
                // Since ranges and gaps should always be at least size 1, we store them with -1.
                //
                // We can always reconstruct the ranges using the end of the previous one and the gap.

                // Iterate
                let mut it = ack_ranges.iter();

                // Extract the first range (first in the iterator)
                let first = it.next().unwrap();
                let first_range_size = first.end - first.start;
                if first_range_size == 0 {
                    return Err(SerializationError::InvalidAckRange);
                }

                b.put_varint(first.start)?;
                b.put_varint(first_range_size - 1)?;

                // Write the number of remaining ranges
                b.put_varint(it.len() as u64)?;

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
            }
        }

        Ok(before - b.cap())
    }

    /// See [`PacketPartialDeser`] for the non-allocating version if you just want to process packets
    /// and discard them.
    #[allow(unused)]
    pub fn from_bytes(data: &[u8]) -> Result<Self, SerializationError> {
        PacketPartialDeser::from_raw_bytes(data)?.into_packet()
    }
}

impl PacketPartialDeser {
    pub fn sequence(&self) -> u64 {
        match self {
            Self::SmallReliable { sequence, .. }
            | Self::SmallUnreliable { sequence, .. }
            | Self::UnreliableSlice { sequence, .. }
            | Self::ReliableSlice { sequence, .. }
            | Self::Ack { sequence, .. } => *sequence,
        }
    }

    pub fn from_raw_bytes(data: &[u8]) -> Result<Self, SerializationError> {
        // This is the only allocation we need.
        Self::from_bytes(Bytes::copy_from_slice(data))
    }

    pub fn from_bytes(bytes: Bytes) -> Result<Self, SerializationError> {
        let mut b = octets::Octets::with_slice(&bytes);

        let packet_type = b.get_u8()?;
        match packet_type {
            0 => {
                // SmallReliable
                let sequence = b.get_varint()?;
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
                let sequence = b.get_varint()?;
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
                let sequence = b.get_varint()?;
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
                })
            }
            3 => {
                // UnreliableSlice
                let sequence = b.get_varint()?;
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
                })
            }
            4 => {
                // Ack
                let sequence = b.get_varint()?;

                let first_range_start = b.get_varint()?;
                let first_range_size = b.get_varint()?.checked_add(1).ok_or(SerializationError::InvalidAckRange)?;
                let num_remaining_ranges = b.get_varint()?;

                if first_range_size > u64::MAX - first_range_start || num_remaining_ranges > 1_000 {
                    return Err(SerializationError::InvalidAckRange);
                }

                let ack_ranges = AckRangesIterator::new(
                    first_range_start..(first_range_start + first_range_size),
                    num_remaining_ranges,
                    bytes.slice(b.off()..),
                );

                Ok(Self::Ack { sequence, ack_ranges })
            }
            _ => Err(SerializationError::InvalidPacketType),
        }
    }

    /// Fully deserialize to a [`Packet`].
    pub fn into_packet(self) -> Result<Packet, SerializationError> {
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
                Ok(Packet::SmallReliable {
                    sequence,
                    channel_id,
                    messages: accumulated,
                })
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
                Ok(Packet::SmallUnreliable {
                    sequence,
                    channel_id,
                    messages: accumulated,
                })
            }
            Self::ReliableSlice {
                sequence,
                channel_id,
                slice,
            } => Ok(Packet::ReliableSlice {
                sequence,
                channel_id,
                slice,
            }),
            Self::UnreliableSlice {
                sequence,
                channel_id,
                slice,
            } => Ok(Packet::UnreliableSlice {
                sequence,
                channel_id,
                slice,
            }),
            Self::Ack { sequence, mut ack_ranges } => {
                ack_ranges.reset();
                let mut accumulated = Vec::with_capacity(ack_ranges.len as usize);
                while let Some(range) = ack_ranges.next()? {
                    accumulated.push(range);
                }
                Ok(Packet::Ack {
                    sequence,
                    ack_ranges: accumulated,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_small_reliable_packet() {
        let mut buffer = [0u8; 1300];
        let packet = Packet::SmallReliable {
            sequence: 0,
            channel_id: 0,
            messages: vec![(0, vec![0, 0, 0].into()), (1, vec![1, 1, 1].into()), (2, vec![2, 2, 2].into())],
        };

        let mut b = octets::OctetsMut::with_slice(&mut buffer);
        packet.to_bytes(&mut b).unwrap();

        let recv_packet = Packet::from_bytes(buffer.as_slice()).unwrap();
        assert_eq!(packet, recv_packet);
    }

    #[test]
    fn serialize_small_unreliable_packet() {
        let mut buffer = [0u8; 1300];
        let packet = Packet::SmallUnreliable {
            sequence: 0,
            channel_id: 0,
            messages: vec![vec![0, 0, 0].into(), vec![1, 1, 1].into(), vec![2, 2, 2].into()],
        };

        let mut b = octets::OctetsMut::with_slice(&mut buffer);
        packet.to_bytes(&mut b).unwrap();

        let recv_packet = Packet::from_bytes(buffer.as_slice()).unwrap();
        assert_eq!(packet, recv_packet);
    }

    #[test]
    fn serialize_reliable_slice_packet() {
        let mut buffer = [0u8; 1300];

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
        packet.to_bytes(&mut b).unwrap();

        let recv_packet = Packet::from_bytes(buffer.as_slice()).unwrap();
        assert_eq!(packet, recv_packet);
    }

    #[test]
    fn serialize_unreliable_slice_packet() {
        let mut buffer = [0u8; 1300];

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
        packet.to_bytes(&mut b).unwrap();

        let recv_packet = Packet::from_bytes(buffer.as_slice()).unwrap();
        assert_eq!(packet, recv_packet);
    }

    #[test]
    fn serialize_ack_packet() {
        let mut buffer = [0u8; 1300];

        let packet = Packet::Ack {
            sequence: 0,
            ack_ranges: vec![3..7, 10..20, 30..100],
        };

        let mut b = octets::OctetsMut::with_slice(&mut buffer);
        packet.to_bytes(&mut b).unwrap();

        let recv_packet = Packet::from_bytes(buffer.as_slice()).unwrap();
        assert_eq!(packet, recv_packet);
    }

    #[test]
    fn serialize_ack_packet_err_size() {
        let mut buffer = [0u8; 1300];

        let packet = Packet::Ack {
            sequence: 0,
            // empty range not allowed
            ack_ranges: vec![3..7, 10..20, 22..22],
        };

        let mut b = octets::OctetsMut::with_slice(&mut buffer);
        assert!(packet.to_bytes(&mut b).is_err());
    }

    #[test]
    fn serialize_ack_packet_err_gap() {
        let mut buffer = [0u8; 1300];

        let packet = Packet::Ack {
            sequence: 0,
            // gap of 0 not allowed
            ack_ranges: vec![3..7, 7..20],
        };

        let mut b = octets::OctetsMut::with_slice(&mut buffer);
        assert!(packet.to_bytes(&mut b).is_err());
    }
}
