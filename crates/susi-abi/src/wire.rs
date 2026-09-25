//! SUSI Swarm OS IPC Wire Framing.
//!
//! High-throughput, framing protocol for Unix Domain Sockets and streaming pipes.

use std::fmt;

/// Magic bytes preceding every valid SUSI wire frame (`b"SUSI"`).
pub const SUSI_WIRE_MAGIC: [u8; 4] = *b"SUSI";

/// Current wire protocol specification version.
pub const SUSI_WIRE_VERSION: u8 = 1;

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
            _ => None,
        }
    }
}

/// A structured, framed message suitable for streaming over UDS or pipes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireFrame {
    /// Wire protocol version.
    pub version: u8,
    /// Type of the payload enclosed.
    pub msg_type: MessageType,
    /// Raw payload bytes (typically UTF-8 JSON or binary data).
    pub payload: Vec<u8>,
}

impl WireFrame {
    /// Creates a new wire frame with the current protocol version.
    #[must_use]
    pub fn new(msg_type: MessageType, payload: Vec<u8>) -> Self {
        Self {
            version: SUSI_WIRE_VERSION,
            msg_type,
            payload,
        }
    }

    /// Serializes this frame into a contiguous byte vector.
    ///
    /// Header layout (10 bytes):
    /// - 0..4: Magic `b"SUSI"`
    /// - 4..5: Version (u8)
    /// - 5..6: MessageType (u8)
    /// - 6..10: Payload Length (u32, big-endian)
    ///   Followed by `payload`.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let payload_len = self.payload.len() as u32;
        let mut buf = Vec::with_capacity(10 + self.payload.len());
        buf.extend_from_slice(&SUSI_WIRE_MAGIC);
        buf.push(self.version);
        buf.push(self.msg_type as u8);
        buf.extend_from_slice(&payload_len.to_be_bytes());
        buf.extend_from_slice(&self.payload);
        buf
    }

    /// Attempts to parse a single frame from the start of a byte slice.
    ///
    /// Returns `Ok((frame, bytes_consumed))` on success, or an error if invalid/incomplete.
    pub fn decode(buf: &[u8]) -> Result<(Self, usize), WireError> {
        if buf.len() < 10 {
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

        let len_bytes: [u8; 4] = [buf[6], buf[7], buf[8], buf[9]];
        let payload_len = u32::from_be_bytes(len_bytes) as usize;

        if payload_len > MAX_WIRE_PAYLOAD_BYTES {
            return Err(WireError::PayloadTooLarge(payload_len));
        }

        let total_frame_len = 10 + payload_len;
        if buf.len() < total_frame_len {
            return Err(WireError::IncompletePayload {
                expected: payload_len,
                available: buf.len() - 10,
            });
        }

        let payload = buf[10..total_frame_len].to_vec();
        let frame = Self {
            version,
            msg_type,
            payload,
        };

        Ok((frame, total_frame_len))
    }
}
