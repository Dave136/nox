//! Bounded Tokio TCP primitives used by later sync state machines.

use crate::frame::{
    FRAME_HEADER_BYTES, FRAME_PREFIX_BYTES, Frame, FrameClass, FrameError, FrameOperation,
    MAX_FRAME_BODY_BYTES, encode_header, encode_prefix, validate_frame_header,
};
use std::{fmt, io, net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError},
    time::{self, error::Elapsed},
};

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
pub const READ_TIMEOUT: Duration = Duration::from_secs(15);
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(15);
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
pub const MAX_CONCURRENT_CONNECTIONS: usize = 16;

#[derive(Clone, Copy, Debug)]
pub struct TransportLimits {
    pub connect_timeout: Duration,
    pub handshake_timeout: Duration,
    pub read_timeout: Duration,
    pub write_timeout: Duration,
    pub idle_timeout: Duration,
    pub max_connections: usize,
}

impl TransportLimits {
    #[must_use]
    pub const fn v1() -> Self {
        Self {
            connect_timeout: CONNECT_TIMEOUT,
            handshake_timeout: HANDSHAKE_TIMEOUT,
            read_timeout: READ_TIMEOUT,
            write_timeout: WRITE_TIMEOUT,
            idle_timeout: IDLE_TIMEOUT,
            max_connections: MAX_CONCURRENT_CONNECTIONS,
        }
    }

    fn validate(self) -> Result<(), TransportError> {
        if self.connect_timeout.is_zero()
            || self.handshake_timeout.is_zero()
            || self.read_timeout.is_zero()
            || self.write_timeout.is_zero()
            || self.idle_timeout.is_zero()
            || !(1..=1024).contains(&self.max_connections)
        {
            return Err(TransportError::InvalidLimits);
        }
        Ok(())
    }
}

impl Default for TransportLimits {
    fn default() -> Self {
        Self::v1()
    }
}

#[derive(Debug)]
pub enum TransportError {
    InvalidLimits,
    ConnectTimeout,
    ConnectionLimit,
    Io(io::Error),
    Frame(FrameError),
}

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimits => formatter.write_str("invalid sync transport limits"),
            Self::ConnectTimeout => formatter.write_str("sync connection timed out"),
            Self::ConnectionLimit => formatter.write_str("sync connection limit reached"),
            Self::Io(_) => formatter.write_str("sync transport I/O error"),
            Self::Frame(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for TransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Frame(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for TransportError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<FrameError> for TransportError {
    fn from(error: FrameError) -> Self {
        Self::Frame(error)
    }
}

/// A single-owner framed stream. Taking `&mut self` for every operation makes
/// concurrent frame writes impossible without adding a lock to the protocol.
pub struct FramedIo<S> {
    stream: Option<S>,
    limits: TransportLimits,
    pending: Option<Frame>,
}

impl<S> FramedIo<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    pub fn new(stream: S, limits: TransportLimits) -> Result<Self, FrameError> {
        Ok(Self {
            stream: Some(stream),
            limits,
            pending: None,
        })
    }

    pub async fn read_frame(&mut self) -> Result<Frame, FrameError> {
        if let Some(frame) = self.pending.take() {
            return Ok(frame);
        }
        let Some(_) = self.stream else {
            return Err(FrameError::Closed);
        };
        let timeout = self.limits.read_timeout;
        let result = time::timeout(timeout, self.read_frame_inner()).await;
        match result {
            Err(_) => {
                self.invalidate();
                Err(FrameError::Timeout(FrameOperation::Read))
            }
            Ok(Err(error)) => {
                self.invalidate();
                Err(error)
            }
            Ok(Ok(frame)) => Ok(frame),
        }
    }

    /// Requeue one already-read frame for a protocol handler after the actor
    /// has classified the connection.  Only the actor uses this one-frame
    /// lookahead, so a second pending frame is a programming error.
    pub(crate) fn pushback_frame(&mut self, frame: Frame) {
        debug_assert!(self.pending.is_none());
        self.pending = Some(frame);
    }

    async fn read_frame_inner(&mut self) -> Result<Frame, FrameError> {
        let stream = self.stream.as_mut().ok_or(FrameError::Closed)?;
        let mut prefix = [0_u8; FRAME_PREFIX_BYTES];
        match read_exact_progress(stream, &mut prefix).await {
            Ok(()) => {}
            Err(ReadFailure::Eof(0)) => return Err(FrameError::Closed),
            Err(ReadFailure::Eof(_)) => return Err(FrameError::Truncated),
            Err(ReadFailure::Io(error)) => return Err(FrameError::Io(error)),
        }

        let body_length = u32::from_le_bytes(prefix) as usize;
        if body_length < FRAME_HEADER_BYTES {
            return Err(FrameError::FrameTooSmall);
        }
        if body_length > MAX_FRAME_BODY_BYTES {
            return Err(FrameError::FrameTooLarge);
        }

        let mut header = [0_u8; FRAME_HEADER_BYTES];
        match read_exact_progress(stream, &mut header).await {
            Ok(()) => {}
            Err(ReadFailure::Eof(_)) => return Err(FrameError::Truncated),
            Err(ReadFailure::Io(error)) => return Err(FrameError::Io(error)),
        }
        let (payload_length, class) = validate_frame_header(&prefix, &header)?;
        let mut payload = vec![0_u8; payload_length];
        match read_exact_progress(stream, &mut payload).await {
            Ok(()) => Ok(Frame { class, payload }),
            Err(ReadFailure::Eof(_)) => Err(FrameError::Truncated),
            Err(ReadFailure::Io(error)) => Err(FrameError::Io(error)),
        }
    }

    pub async fn write_frame(
        &mut self,
        class: FrameClass,
        payload: &[u8],
    ) -> Result<(), FrameError> {
        let prefix = encode_prefix(payload.len())?;
        let header = encode_header(class);
        let Some(stream) = self.stream.as_mut() else {
            return Err(FrameError::Closed);
        };
        let timeout = self.limits.write_timeout;
        let result = time::timeout(timeout, async {
            stream.write_all(&prefix).await?;
            stream.write_all(&header).await?;
            stream.write_all(payload).await?;
            stream.flush().await
        })
        .await;
        match result {
            Err(_) => {
                self.invalidate();
                Err(FrameError::Timeout(FrameOperation::Write))
            }
            Ok(Err(error)) => {
                self.invalidate();
                Err(FrameError::Io(error))
            }
            Ok(Ok(())) => Ok(()),
        }
    }

    pub async fn shutdown(&mut self) -> Result<(), FrameError> {
        let Some(mut stream) = self.stream.take() else {
            return Ok(());
        };
        match time::timeout(self.limits.write_timeout, stream.shutdown()).await {
            Err(_) => Err(FrameError::Timeout(FrameOperation::Shutdown)),
            Ok(Err(error)) => Err(FrameError::Io(error)),
            Ok(Ok(())) => Ok(()),
        }
    }

    fn invalidate(&mut self) {
        self.stream.take();
    }
}

#[derive(Debug, Clone)]
pub struct ConnectionLimiter {
    permits: Arc<Semaphore>,
}

impl ConnectionLimiter {
    pub fn new(max: usize) -> Result<Self, TransportError> {
        if !(1..=1024).contains(&max) {
            return Err(TransportError::InvalidLimits);
        }
        Ok(Self {
            permits: Arc::new(Semaphore::new(max)),
        })
    }

    pub fn try_acquire(&self) -> Result<ConnectionPermit, TransportError> {
        match Arc::clone(&self.permits).try_acquire_owned() {
            Ok(permit) => Ok(ConnectionPermit {
                permit: Some(permit),
            }),
            Err(TryAcquireError::NoPermits) => Err(TransportError::ConnectionLimit),
            Err(TryAcquireError::Closed) => Err(TransportError::ConnectionLimit),
        }
    }

    #[must_use]
    pub fn available(&self) -> usize {
        self.permits.available_permits()
    }
}

#[derive(Debug)]
pub struct ConnectionPermit {
    permit: Option<OwnedSemaphorePermit>,
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        let _ = self.permit.take();
    }
}

pub struct AcceptedConnection {
    pub peer_addr: SocketAddr,
    pub io: FramedIo<TcpStream>,
    permit: ConnectionPermit,
}

impl Drop for AcceptedConnection {
    fn drop(&mut self) {
        let _ = self.permit.permit.is_some();
    }
}

impl fmt::Debug for AcceptedConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcceptedConnection")
            .field("peer_addr", &self.peer_addr)
            .finish_non_exhaustive()
    }
}

pub async fn connect(
    address: SocketAddr,
    limits: TransportLimits,
) -> Result<FramedIo<TcpStream>, TransportError> {
    limits.validate()?;
    let stream = match time::timeout(limits.connect_timeout, TcpStream::connect(address)).await {
        Err(Elapsed { .. }) => return Err(TransportError::ConnectTimeout),
        Ok(Err(error)) => return Err(TransportError::Io(error)),
        Ok(Ok(stream)) => stream,
    };
    stream.set_nodelay(true).map_err(TransportError::Io)?;
    FramedIo::new(stream, limits).map_err(TransportError::Frame)
}

pub async fn accept(
    listener: &TcpListener,
    limiter: &ConnectionLimiter,
    limits: TransportLimits,
) -> Result<AcceptedConnection, TransportError> {
    limits.validate()?;
    let (stream, peer_addr) = listener.accept().await.map_err(TransportError::Io)?;
    let permit = match limiter.try_acquire() {
        Ok(permit) => permit,
        Err(error) => {
            drop(stream);
            return Err(error);
        }
    };
    if let Err(error) = stream.set_nodelay(true) {
        drop(permit);
        return Err(TransportError::Io(error));
    }
    let io = FramedIo::new(stream, limits).map_err(TransportError::Frame)?;
    Ok(AcceptedConnection {
        peer_addr,
        io,
        permit,
    })
}

enum ReadFailure {
    Eof(usize),
    Io(io::Error),
}

async fn read_exact_progress<S>(stream: &mut S, buffer: &mut [u8]) -> Result<(), ReadFailure>
where
    S: AsyncRead + Unpin,
{
    let mut offset = 0;
    while offset < buffer.len() {
        match stream.read(&mut buffer[offset..]).await {
            Ok(0) => return Err(ReadFailure::Eof(offset)),
            Ok(read) => offset += read,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                return Err(ReadFailure::Eof(offset));
            }
            Err(error) => return Err(ReadFailure::Io(error)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{FrameError, FrameOperation, MAX_FRAME_PAYLOAD_BYTES};
    use std::time::Duration;
    use tokio::io::duplex;

    fn limits() -> TransportLimits {
        TransportLimits {
            connect_timeout: Duration::from_secs(1),
            handshake_timeout: Duration::from_secs(1),
            read_timeout: Duration::from_millis(50),
            write_timeout: Duration::from_millis(50),
            idle_timeout: Duration::from_secs(1),
            max_connections: 2,
        }
    }

    #[tokio::test]
    async fn fragmented_frame_round_trip() {
        let (left, right) = duplex(8);
        let mut writer = FramedIo::new(left, limits()).unwrap();
        let mut reader = FramedIo::new(right, limits()).unwrap();
        let write = tokio::spawn(async move {
            writer
                .write_frame(FrameClass::NoiseTransport, b"payload")
                .await
                .unwrap();
        });
        let frame = reader.read_frame().await.unwrap();
        write.await.unwrap();
        assert_eq!(frame.class, FrameClass::NoiseTransport);
        assert_eq!(frame.payload, b"payload");
    }

    #[tokio::test]
    async fn maximum_payload_is_accepted_and_next_frame_is_preserved() {
        let (left, right) = duplex(70_000);
        let mut writer = FramedIo::new(left, limits()).unwrap();
        let mut reader = FramedIo::new(right, limits()).unwrap();
        let payload = vec![7_u8; MAX_FRAME_PAYLOAD_BYTES];
        writer
            .write_frame(FrameClass::Pairing, &payload)
            .await
            .unwrap();
        let frame = reader.read_frame().await.unwrap();
        assert_eq!(frame.payload, payload);
    }

    #[tokio::test]
    async fn partial_prefix_is_truncated_and_clean_close_is_closed() {
        let (mut left, right) = duplex(8);
        let mut reader = FramedIo::new(right, limits()).unwrap();
        left.write_all(&[4, 0]).await.unwrap();
        drop(left);
        assert!(matches!(
            reader.read_frame().await,
            Err(FrameError::Truncated)
        ));

        let (left, right) = duplex(8);
        drop(left);
        let mut reader = FramedIo::new(right, limits()).unwrap();
        assert!(matches!(reader.read_frame().await, Err(FrameError::Closed)));
    }

    #[tokio::test]
    async fn slow_peer_hits_one_read_deadline() {
        let (mut left, right) = duplex(8);
        let mut reader = FramedIo::new(right, limits()).unwrap();
        let task = tokio::spawn(async move {
            left.write_all(&[4, 0, 0, 0]).await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        });
        assert!(matches!(
            reader.read_frame().await,
            Err(FrameError::Timeout(FrameOperation::Read))
        ));
        task.await.unwrap();
    }

    #[tokio::test]
    async fn loopback_connect_and_accept_round_trip() {
        let listener = match TcpListener::bind(("127.0.0.1", 0)).await {
            Ok(listener) => listener,
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("loopback bind failed: {error}"),
        };
        let address = listener.local_addr().unwrap();
        let limiter = ConnectionLimiter::new(1).unwrap();
        let accept_task =
            tokio::spawn(async move { accept(&listener, &limiter, limits()).await.unwrap() });
        let mut client = connect(address, limits()).await.unwrap();
        let mut accepted = accept_task.await.unwrap();
        client
            .write_frame(FrameClass::NoiseHandshake, b"loopback")
            .await
            .unwrap();
        let frame = accepted.io.read_frame().await.unwrap();
        assert_eq!(frame.class, FrameClass::NoiseHandshake);
        assert_eq!(frame.payload, b"loopback");
    }

    #[tokio::test]
    async fn write_timeout_drops_a_stalled_stream() {
        let (left, _right) = duplex(1);
        let mut limits = limits();
        limits.write_timeout = Duration::from_millis(20);
        let mut writer = FramedIo::new(left, limits).unwrap();
        assert!(matches!(
            writer.write_frame(FrameClass::Pairing, &[9_u8; 32]).await,
            Err(FrameError::Timeout(FrameOperation::Write))
        ));
        assert!(matches!(
            writer.write_frame(FrameClass::Pairing, &[]).await,
            Err(FrameError::Closed)
        ));
    }

    #[test]
    fn malformed_headers_fail_before_payload_work() {
        assert!(matches!(
            validate_frame_header(&[3, 0, 0, 0], &[1, 0, 1, 0]),
            Err(FrameError::FrameTooSmall)
        ));
        assert!(matches!(
            validate_frame_header(&[0xff, 0xff, 0xff, 0xff], &[1, 0, 1, 0]),
            Err(FrameError::FrameTooLarge)
        ));
        assert!(matches!(
            validate_frame_header(&[4, 0, 0, 0], &[2, 0, 1, 0]),
            Err(FrameError::UnsupportedVersion(2))
        ));
        assert!(matches!(
            validate_frame_header(&[4, 0, 0, 0], &[1, 0, 1, 1]),
            Err(FrameError::UnsupportedFlags(1))
        ));
    }

    #[test]
    fn limiter_saturates_and_recovers_on_drop() {
        let limiter = ConnectionLimiter::new(2).unwrap();
        let first = limiter.try_acquire().unwrap();
        let second = limiter.try_acquire().unwrap();
        assert_eq!(limiter.available(), 0);
        assert!(matches!(
            limiter.try_acquire(),
            Err(TransportError::ConnectionLimit)
        ));
        drop(first);
        assert_eq!(limiter.available(), 1);
        drop(second);
        assert_eq!(limiter.available(), 2);
    }

    #[tokio::test]
    async fn oversized_write_is_rejected_before_touching_the_stream() {
        let (left, right) = duplex(128);
        let mut writer = FramedIo::new(left, limits()).unwrap();
        let mut reader = FramedIo::new(right, limits()).unwrap();
        assert!(matches!(
            writer
                .write_frame(FrameClass::Pairing, &[0_u8; MAX_FRAME_PAYLOAD_BYTES + 1])
                .await,
            Err(FrameError::FrameTooLarge)
        ));
        writer
            .write_frame(FrameClass::Pairing, b"ok")
            .await
            .unwrap();
        assert_eq!(reader.read_frame().await.unwrap().payload, b"ok");
    }

    #[test]
    fn parser_fuzz_inputs_never_panic() {
        let mut state = 0x1234_5678_u32;
        for _ in 0..10_000 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let prefix = state.to_le_bytes();
            state = state.rotate_left(7);
            let header = state.to_le_bytes();
            let _ = validate_frame_header(&prefix, &header);
        }
    }
}
