//! SUSI Swarm OS IPC Wire Framing.
//!
//! High-throughput, framing protocol for Unix Domain Sockets and streaming pipes.
//! Supports QoS flags for priority, reliability, and ordering guarantees.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Magic bytes preceding every valid SUSI wire frame (`b"SUSI"`).
pub const SUSI_WIRE_MAGIC: [u8; 4] = *b"SUSI";

/// Current wire protocol specification version (v2 adds QoS flags).
pub const SUSI_WIRE_VERSION: u8 = 2;

/// Maximum payload size allowed for a single wire frame (32 MiB).
pub const MAX_WIRE_PAYLOAD_BYTES: usize = 32 * 1024 * 1024;

/// Errors encountered when parsing or encoding wire frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireError {
    /// Received buffer is shorter than the minimum header length (10 bytes).
    IncompleteHeader,
    /// Magic bytes did not match `SUSI`.
    InvalidMagic([u8; 4]),
    /// Frame version is unsupported.
    UnsupportedVersion(u8),
    /// Frame payload exceeds the maximum safety bound.
    PayloadTooLarge(usize),
    /// Received buffer has fewer bytes than declared in the header.
    IncompletePayload { expected: usize, available: usize },
    /// JSON serialization or deserialization failed.
    Serialization(String),
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IncompleteHeader => write!(f, "wire frame header is incomplete"),
            Self::InvalidMagic(m) => write!(f, "invalid magic header: {:?}", m),
            Self::UnsupportedVersion(v) => write!(f, "unsupported wire protocol version: {}", v),
            Self::PayloadTooLarge(s) => write!(f, "payload size {} exceeds 32MiB safety limit", s),
            Self::IncompletePayload {
                expected,
                available,
            } => {
                write!(
                    f,
                    "expected {} bytes of payload, only {} available",
                    expected, available
                )
            }
            Self::Serialization(msg) => write!(f, "wire serialization error: {}", msg),
        }
    }
}

impl std::error::Error for WireError {}

/// Message type discriminant on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageType {
    /// AI Syscall Request payload.
    SyscallRequest = 0x01,
    /// AI Syscall Response payload.
    SyscallResponse = 0x02,
    /// Swarm Stigmergic Pheromone payload.
    SwarmPheromone = 0x03,
    /// High-frequency Heartbeat ping.
    Heartbeat = 0x04,
    /// Raw unparsed byte stream.
    RawBytes = 0x05,
    /// Task Negotiation: Offer/Accept/Commit/Reject (Bullet 19).
    TaskNegotiation = 0x06,
    /// Durable event log entry for time-travel debugging (Bullet 7).
    EventLog = 0x07,
}

impl MessageType {
    /// Converts a raw byte to a MessageType.
    #[must_use]
    pub const fn from_u8(val: u8) -> Option<Self> {
        match val {
            0x01 => Some(Self::SyscallRequest),
            0x02 => Some(Self::SyscallResponse),
            0x03 => Some(Self::SwarmPheromone),
            0x04 => Some(Self::Heartbeat),
            0x05 => Some(Self::RawBytes),
            0x06 => Some(Self::TaskNegotiation),
            0x07 => Some(Self::EventLog),
            _ => None,
        }
    }
}

// ──────────────────────────────────────────────────────────
// QoS Flags (Swarm OS Vision – Bullet 5)
// ──────────────────────────────────────────────────────────

/// Quality-of-Service flags carried in the wire frame header.
///
/// Encoded as a 2-byte bitfield:
///
/// | Bits  | Field         | Description                               |
/// |-------|---------------|-------------------------------------------|
/// | 0-2   | Priority      | 0 = best-effort, 7 = critical             |
/// | 3     | Reliable      | 1 = at-least-once delivery guarantee      |
/// | 4     | Ordered       | 1 = strict per-sender ordering required   |
/// | 5     | Idempotent    | 1 = safe to retry without side effects    |
/// | 6-15  | Reserved      | Must be zero                              |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct QosFlags(pub u16);

impl QosFlags {
    /// Best-effort delivery, no ordering guarantees.
    pub const BEST_EFFORT: Self = Self(0);

    /// Critical priority, reliable, ordered.
    pub const CRITICAL: Self = Self(0b0001_1111);

    /// Creates QoS flags with the given priority (0-7).
    #[must_use]
    pub const fn with_priority(priority: u8) -> Self {
        Self((priority & 0x07) as u16)
    }

    /// Returns the priority level (0 = best-effort, 7 = critical).
    #[must_use]
    pub const fn priority(self) -> u8 {
        (self.0 & 0x07) as u8
    }

    /// Returns true if at-least-once delivery is requested.
    #[must_use]
    pub const fn is_reliable(self) -> bool {
        (self.0 & 0x08) != 0
    }

    /// Returns true if strict per-sender ordering is required.
    #[must_use]
    pub const fn is_ordered(self) -> bool {
        (self.0 & 0x10) != 0
    }

    /// Returns true if the message is idempotent (safe to retry).
    #[must_use]
    pub const fn is_idempotent(self) -> bool {
        (self.0 & 0x20) != 0
    }

    /// Sets the reliable delivery flag.
    #[must_use]
    pub const fn set_reliable(self) -> Self {
        Self(self.0 | 0x08)
    }

    /// Sets the ordered delivery flag.
    #[must_use]
    pub const fn set_ordered(self) -> Self {
        Self(self.0 | 0x10)
    }

    /// Sets the idempotent flag.
    #[must_use]
    pub const fn set_idempotent(self) -> Self {
        Self(self.0 | 0x20)
    }

    /// Encodes to 2 bytes (big-endian).
    #[must_use]
    pub const fn to_bytes(self) -> [u8; 2] {
        self.0.to_be_bytes()
    }

    /// Decodes from 2 bytes (big-endian).
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 2]) -> Self {
        Self(u16::from_be_bytes(bytes))
    }
}

/// Header size in bytes for the v2 wire protocol.
const HEADER_SIZE: usize = 12;

/// A structured, framed message suitable for streaming over UDS or pipes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireFrame {
    /// Wire protocol version.
    pub version: u8,
    /// Type of the payload enclosed.
    pub msg_type: MessageType,
    /// Quality-of-Service flags (v2).
    pub qos: QosFlags,
    /// Raw payload bytes (typically UTF-8 JSON or binary data).
    pub payload: Vec<u8>,
}

impl WireFrame {
    /// Creates a new wire frame with the current protocol version and best-effort QoS.
    #[must_use]
    pub fn new(msg_type: MessageType, payload: Vec<u8>) -> Self {
        Self {
            version: SUSI_WIRE_VERSION,
            msg_type,
            qos: QosFlags::BEST_EFFORT,
            payload,
        }
    }

    /// Creates a new wire frame with explicit QoS flags.
    #[must_use]
    pub fn with_qos(msg_type: MessageType, qos: QosFlags, payload: Vec<u8>) -> Self {
        Self {
            version: SUSI_WIRE_VERSION,
            msg_type,
            qos,
            payload,
        }
    }

    /// Serializes this frame into a contiguous byte vector.
    ///
    /// Header layout (12 bytes, v2):
    /// - 0..4:   Magic `b"SUSI"`
    /// - 4..5:   Version (u8)
    /// - 5..6:   MessageType (u8)
    /// - 6..8:   QoS Flags (u16, big-endian)
    /// - 8..12:  Payload Length (u32, big-endian)
    ///   Followed by `payload`.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let payload_len = self.payload.len() as u32;
        let mut buf = Vec::with_capacity(HEADER_SIZE + self.payload.len());
        buf.extend_from_slice(&SUSI_WIRE_MAGIC);
        buf.push(self.version);
        buf.push(self.msg_type as u8);
        buf.extend_from_slice(&self.qos.to_bytes());
        buf.extend_from_slice(&payload_len.to_be_bytes());
        buf.extend_from_slice(&self.payload);
        buf
    }

    /// Attempts to parse a single frame from the start of a byte slice.
    ///
    /// Returns `Ok((frame, bytes_consumed))` on success, or an error if invalid/incomplete.
    pub fn decode(buf: &[u8]) -> Result<(Self, usize), WireError> {
        if buf.len() < HEADER_SIZE {
            return Err(WireError::IncompleteHeader);
        }

        let magic: [u8; 4] = [buf[0], buf[1], buf[2], buf[3]];
        if magic != SUSI_WIRE_MAGIC {
            return Err(WireError::InvalidMagic(magic));
        }

        let version = buf[4];
        if version != SUSI_WIRE_VERSION {
            return Err(WireError::UnsupportedVersion(version));
        }

        let msg_type_byte = buf[5];
        let msg_type = match MessageType::from_u8(msg_type_byte) {
            Some(t) => t,
            None => return Err(WireError::UnsupportedVersion(msg_type_byte)),
        };

        let qos = QosFlags::from_bytes([buf[6], buf[7]]);

        let len_bytes: [u8; 4] = [buf[8], buf[9], buf[10], buf[11]];
        let payload_len = u32::from_be_bytes(len_bytes) as usize;

        if payload_len > MAX_WIRE_PAYLOAD_BYTES {
            return Err(WireError::PayloadTooLarge(payload_len));
        }

        let total_frame_len = HEADER_SIZE + payload_len;
        if buf.len() < total_frame_len {
            return Err(WireError::IncompletePayload {
                expected: payload_len,
                available: buf.len() - HEADER_SIZE,
            });
        }

        let payload = buf[HEADER_SIZE..total_frame_len].to_vec();
        let frame = Self {
            version,
            msg_type,
            qos,
            payload,
        };

        Ok((frame, total_frame_len))
    }
}

/// Incremental decoder for a byte stream carrying back-to-back frames.
///
/// TCP delivers bytes, not frames: one read may hold several frames or only
/// part of one. Push every chunk read from the socket, then drain complete
/// frames with [`FrameStream::next_frame`]; partial data stays buffered.
#[derive(Debug, Default)]
pub struct FrameStream {
    buf: Vec<u8>,
}

impl FrameStream {
    /// Creates an empty stream decoder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends bytes received from the transport.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Number of buffered bytes not yet consumed by a complete frame.
    #[must_use]
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    /// Returns the next complete frame, `Ok(None)` when more bytes are
    /// needed.
    ///
    /// # Errors
    /// Returns the decode error when the buffered bytes can never form a
    /// valid frame (bad magic, version, type, or oversized payload); the
    /// stream is then unrecoverable and the connection should be dropped.
    pub fn next_frame(&mut self) -> Result<Option<WireFrame>, WireError> {
        match WireFrame::decode(&self.buf) {
            Ok((frame, used)) => {
                self.buf.drain(..used);
                Ok(Some(frame))
            }
            Err(WireError::IncompleteHeader | WireError::IncompletePayload { .. }) => Ok(None),
            Err(
                e @ (WireError::InvalidMagic(_)
                | WireError::UnsupportedVersion(_)
                | WireError::PayloadTooLarge(_)
                | WireError::Serialization(_)),
            ) => Err(e),
        }
    }
}

#[cfg(test)]
mod prop_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        // Property: every frame that encodes decodes back to an identical
        // frame, consuming exactly the bytes it produced.
        #[test]
        fn round_trip_preserves_frame(
            msg_type_byte in 1u8..=7u8,
            qos_raw in any::<u16>(),
            payload in proptest::collection::vec(any::<u8>(), 0..4096),
        ) {
            let msg_type = MessageType::from_u8(msg_type_byte).unwrap();
            let qos = QosFlags(qos_raw);
            let frame = WireFrame::with_qos(msg_type, qos, payload);
            let encoded = frame.encode();
            let (decoded, consumed) = WireFrame::decode(&encoded).unwrap();
            prop_assert_eq!(consumed, encoded.len());
            prop_assert_eq!(decoded, frame);
        }

        // Property: decode never panics on arbitrary bytes — the eventual
        // consumer is a UDS/pipe stream, i.e. untrusted input. Buffers
        // shorter than the fixed 12-byte header are the one length class
        // that can only ever fail one way.
        #[test]
        fn decode_never_panics_on_arbitrary_bytes(
            buf in proptest::collection::vec(any::<u8>(), 0..600),
        ) {
            let result = WireFrame::decode(&buf);
            if buf.len() < HEADER_SIZE {
                prop_assert_eq!(result, Err(WireError::IncompleteHeader));
            }
        }
    }
}

#[cfg(test)]
mod stream_tests {
    use super::*;

    #[test]
    fn frame_stream_splits_coalesced_and_joins_partial_frames() {
        let a = WireFrame::new(MessageType::Heartbeat, b"one".to_vec()).encode();
        let b = WireFrame::new(MessageType::RawBytes, b"two".to_vec()).encode();
        let mut wire = a.clone();
        wire.extend_from_slice(&b);

        // Deliver the two frames split at an arbitrary point inside `b`.
        let cut = a.len() + 5;
        let mut stream = FrameStream::new();
        stream.push(&wire[..cut]);
        assert_eq!(stream.next_frame().unwrap().unwrap().payload, b"one");
        assert!(stream.next_frame().unwrap().is_none());
        stream.push(&wire[cut..]);
        let second = stream.next_frame().unwrap().unwrap();
        assert_eq!(second.msg_type, MessageType::RawBytes);
        assert_eq!(second.payload, b"two");
        assert!(stream.next_frame().unwrap().is_none());
        assert_eq!(stream.buffered(), 0);
    }

    #[test]
    fn frame_stream_reports_corruption() {
        let mut stream = FrameStream::new();
        stream.push(b"NOPE-not-a-susi-frame");
        assert!(matches!(
            stream.next_frame(),
            Err(WireError::InvalidMagic(_))
        ));
    }
}
