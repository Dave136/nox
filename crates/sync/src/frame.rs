//! Bounded, versioned outer framing for all sync traffic.

use std::{fmt, io};

pub const SYNC_PROTOCOL_VERSION: u16 = 1;
pub const FRAME_PREFIX_BYTES: usize = 4;
pub const FRAME_HEADER_BYTES: usize = 4;
pub const MAX_FRAME_PAYLOAD_BYTES: usize = 65_535;
pub const MAX_FRAME_BODY_BYTES: usize = FRAME_HEADER_BYTES + MAX_FRAME_PAYLOAD_BYTES;

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameClass {
    Pairing = 1,
    NoiseHandshake = 2,
    NoiseTransport = 3,
}

impl TryFrom<u8> for FrameClass {
    type Error = FrameError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Pairing),
            2 => Ok(Self::NoiseHandshake),
            3 => Ok(Self::NoiseTransport),
            other => Err(FrameError::UnknownClass(other)),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame {
    pub class: FrameClass,
    pub payload: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameOperation {
    Read,
    Write,
    Shutdown,
}

#[derive(Debug)]
pub enum FrameError {
    Io(io::Error),
    Timeout(FrameOperation),
    Truncated,
    FrameTooSmall,
    FrameTooLarge,
    UnsupportedVersion(u16),
    UnknownClass(u8),
    UnsupportedFlags(u8),
    Closed,
}

impl fmt::Display for FrameOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Shutdown => "shutdown",
        })
    }
}

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(_) => formatter.write_str("sync I/O error"),
            Self::Timeout(operation) => write!(formatter, "sync {operation} timed out"),
            Self::Truncated => formatter.write_str("truncated sync frame"),
            Self::FrameTooSmall => formatter.write_str("sync frame is too small"),
            Self::FrameTooLarge => formatter.write_str("sync frame is too large"),
            Self::UnsupportedVersion(_) => formatter.write_str("unsupported sync protocol version"),
            Self::UnknownClass(_) => formatter.write_str("unknown sync frame class"),
            Self::UnsupportedFlags(_) => formatter.write_str("unsupported sync frame flags"),
            Self::Closed => formatter.write_str("sync connection is closed"),
        }
    }
}

impl std::error::Error for FrameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

/// Validate the non-payload portion of a frame without allocating.
///
/// The returned payload length is the number of bytes following the four-byte
/// frame header. This helper is shared by the asynchronous reader and parser
/// tests so malformed prefixes never reach an allocation site.
pub fn validate_frame_header(
    prefix: &[u8],
    header: &[u8],
) -> Result<(usize, FrameClass), FrameError> {
    if prefix.len() != FRAME_PREFIX_BYTES || header.len() != FRAME_HEADER_BYTES {
        return Err(FrameError::Truncated);
    }
    let body_length =
        u32::from_le_bytes(prefix.try_into().map_err(|_| FrameError::Truncated)?) as usize;
    if body_length < FRAME_HEADER_BYTES {
        return Err(FrameError::FrameTooSmall);
    }
    if body_length > MAX_FRAME_BODY_BYTES {
        return Err(FrameError::FrameTooLarge);
    }
    let version = u16::from_le_bytes(
        header
            .get(..2)
            .ok_or(FrameError::Truncated)?
            .try_into()
            .map_err(|_| FrameError::Truncated)?,
    );
    if version != SYNC_PROTOCOL_VERSION {
        return Err(FrameError::UnsupportedVersion(version));
    }
    let class = FrameClass::try_from(*header.get(2).ok_or(FrameError::Truncated)?)?;
    let flags = *header.get(3).ok_or(FrameError::Truncated)?;
    if flags != 0 {
        return Err(FrameError::UnsupportedFlags(flags));
    }
    Ok((body_length - FRAME_HEADER_BYTES, class))
}

pub(crate) fn encode_prefix(payload_length: usize) -> Result<[u8; FRAME_PREFIX_BYTES], FrameError> {
    let body_length = FRAME_HEADER_BYTES
        .checked_add(payload_length)
        .ok_or(FrameError::FrameTooLarge)?;
    if body_length > MAX_FRAME_BODY_BYTES {
        return Err(FrameError::FrameTooLarge);
    }
    Ok((body_length as u32).to_le_bytes())
}

pub(crate) fn encode_header(class: FrameClass) -> [u8; FRAME_HEADER_BYTES] {
    let version = SYNC_PROTOCOL_VERSION.to_le_bytes();
    [version[0], version[1], class as u8, 0]
}
