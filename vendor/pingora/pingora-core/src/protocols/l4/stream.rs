// Copyright 2026 Cloudflare, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Transport layer connection

use async_trait::async_trait;
use futures::FutureExt;
use log::{debug, error};

use pingora_error::{ErrorType::*, OrErr, Result};
#[cfg(target_os = "linux")]
use std::io::IoSliceMut;
#[cfg(unix)]
use std::os::unix::io::AsRawFd;
#[cfg(windows)]
use std::os::windows::io::AsRawSocket;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant, SystemTime};
#[cfg(target_os = "linux")]
use tokio::io::Interest;
// ★ 樞衡改動 22：`BufStream` 換成本檔案的 `L4BufStream`（tokio 1.54 起容量 0 的 `BufReader` 會 panic）。
use tokio::io::{self, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, BufWriter, ReadBuf};
use tokio::net::TcpStream;
#[cfg(unix)]
use tokio::net::UnixStream;

use crate::protocols::l4::ext::{set_tcp_keepalive, TcpKeepalive};
use crate::protocols::l4::virt;
use crate::protocols::raw_connect::ProxyDigest;
use crate::protocols::{
    GetProxyDigest, GetSocketDigest, GetTimingDigest, Peek, Shutdown, SocketDigest, Ssl,
    TimingDigest, UniqueID, UniqueIDType,
};
use crate::upstreams::peer::Tracer;

#[derive(Debug)]
enum RawStream {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(UnixStream),
    Virtual(virt::VirtualSocketStream),
}

impl AsyncRead for RawStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        // Safety: Basic enum pin projection
        unsafe {
            match &mut Pin::get_unchecked_mut(self) {
                RawStream::Tcp(s) => Pin::new_unchecked(s).poll_read(cx, buf),
                #[cfg(unix)]
                RawStream::Unix(s) => Pin::new_unchecked(s).poll_read(cx, buf),
                RawStream::Virtual(s) => Pin::new_unchecked(s).poll_read(cx, buf),
            }
        }
    }
}

impl AsyncWrite for RawStream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context, buf: &[u8]) -> Poll<io::Result<usize>> {
        // Safety: Basic enum pin projection
        unsafe {
            match &mut Pin::get_unchecked_mut(self) {
                RawStream::Tcp(s) => Pin::new_unchecked(s).poll_write(cx, buf),
                #[cfg(unix)]
                RawStream::Unix(s) => Pin::new_unchecked(s).poll_write(cx, buf),
                RawStream::Virtual(s) => Pin::new_unchecked(s).poll_write(cx, buf),
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        // Safety: Basic enum pin projection
        unsafe {
            match &mut Pin::get_unchecked_mut(self) {
                RawStream::Tcp(s) => Pin::new_unchecked(s).poll_flush(cx),
                #[cfg(unix)]
                RawStream::Unix(s) => Pin::new_unchecked(s).poll_flush(cx),
                RawStream::Virtual(s) => Pin::new_unchecked(s).poll_flush(cx),
            }
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        // Safety: Basic enum pin projection
        unsafe {
            match &mut Pin::get_unchecked_mut(self) {
                RawStream::Tcp(s) => Pin::new_unchecked(s).poll_shutdown(cx),
                #[cfg(unix)]
                RawStream::Unix(s) => Pin::new_unchecked(s).poll_shutdown(cx),
                RawStream::Virtual(s) => Pin::new_unchecked(s).poll_shutdown(cx),
            }
        }
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        // Safety: Basic enum pin projection
        unsafe {
            match &mut Pin::get_unchecked_mut(self) {
                RawStream::Tcp(s) => Pin::new_unchecked(s).poll_write_vectored(cx, bufs),
                #[cfg(unix)]
                RawStream::Unix(s) => Pin::new_unchecked(s).poll_write_vectored(cx, bufs),
                RawStream::Virtual(s) => Pin::new_unchecked(s).poll_write_vectored(cx, bufs),
            }
        }
    }

    fn is_write_vectored(&self) -> bool {
        match self {
            RawStream::Tcp(s) => s.is_write_vectored(),
            #[cfg(unix)]
            RawStream::Unix(s) => s.is_write_vectored(),
            RawStream::Virtual(s) => s.is_write_vectored(),
        }
    }
}

#[cfg(unix)]
impl AsRawFd for RawStream {
    fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
        match self {
            RawStream::Tcp(s) => s.as_raw_fd(),
            RawStream::Unix(s) => s.as_raw_fd(),
            RawStream::Virtual(_) => -1, // Virtual stream does not have a real fd
        }
    }
}

#[cfg(windows)]
impl AsRawSocket for RawStream {
    fn as_raw_socket(&self) -> std::os::windows::io::RawSocket {
        match self {
            RawStream::Tcp(s) => s.as_raw_socket(),
            // Virtual stream does not have a real socket, return INVALID_SOCKET (!0)
            RawStream::Virtual(_) => !0,
        }
    }
}

#[derive(Debug)]
struct RawStreamWrapper {
    pub(crate) stream: RawStream,
    /// store the last rx timestamp of the stream.
    pub(crate) rx_ts: Option<SystemTime>,
    /// enable reading rx timestamp
    #[cfg(target_os = "linux")]
    pub(crate) enable_rx_ts: bool,
    #[cfg(target_os = "linux")]
    /// This can be reused across multiple recvmsg calls. The cmsg buffer may
    /// come from old sockets created by older version of pingora and so,
    /// this vector can only grow.
    reusable_cmsg_space: Vec<u8>,
}

impl RawStreamWrapper {
    pub fn new(stream: RawStream) -> Self {
        RawStreamWrapper {
            stream,
            rx_ts: None,
            #[cfg(target_os = "linux")]
            enable_rx_ts: false,
            #[cfg(target_os = "linux")]
            reusable_cmsg_space: nix::cmsg_space!(nix::sys::socket::Timestamps),
        }
    }

    #[cfg(target_os = "linux")]
    pub fn enable_rx_ts(&mut self, enable_rx_ts: bool) {
        self.enable_rx_ts = enable_rx_ts;
    }
}

impl AsyncRead for RawStreamWrapper {
    #[cfg(not(target_os = "linux"))]
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        // Safety: Basic enum pin projection
        unsafe {
            let rs_wrapper = Pin::get_unchecked_mut(self);
            match &mut rs_wrapper.stream {
                RawStream::Tcp(s) => Pin::new_unchecked(s).poll_read(cx, buf),
                #[cfg(unix)]
                RawStream::Unix(s) => Pin::new_unchecked(s).poll_read(cx, buf),
                RawStream::Virtual(s) => Pin::new_unchecked(s).poll_read(cx, buf),
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        use futures::ready;
        use nix::sys::socket::{recvmsg, ControlMessageOwned, MsgFlags, SockaddrStorage};

        // if we do not need rx timestamp, then use the standard path
        if !self.enable_rx_ts {
            // Safety: Basic enum pin projection
            unsafe {
                let rs_wrapper = Pin::get_unchecked_mut(self);
                match &mut rs_wrapper.stream {
                    RawStream::Tcp(s) => return Pin::new_unchecked(s).poll_read(cx, buf),
                    RawStream::Unix(s) => return Pin::new_unchecked(s).poll_read(cx, buf),
                    RawStream::Virtual(s) => return Pin::new_unchecked(s).poll_read(cx, buf),
                }
            }
        }

        // Safety: Basic pin projection to get mutable stream
        let rs_wrapper = unsafe { Pin::get_unchecked_mut(self) };
        match &mut rs_wrapper.stream {
            RawStream::Tcp(s) => {
                loop {
                    ready!(s.poll_read_ready(cx))?;
                    // Safety: maybe uninitialized bytes will only be passed to recvmsg
                    let b = unsafe {
                        &mut *(buf.unfilled_mut() as *mut [std::mem::MaybeUninit<u8>]
                            as *mut [u8])
                    };
                    let mut iov = [IoSliceMut::new(b)];

                    rs_wrapper.reusable_cmsg_space.fill(0);

                    match s.try_io(Interest::READABLE, || {
                        recvmsg::<SockaddrStorage>(
                            s.as_raw_fd(),
                            &mut iov,
                            Some(&mut rs_wrapper.reusable_cmsg_space),
                            MsgFlags::empty(),
                        )
                        .map_err(|errno| errno.into())
                    }) {
                        Ok(r) => {
                            if let Some(ControlMessageOwned::ScmTimestampsns(rtime)) = r
                                .cmsgs()?
                                .find(|i| matches!(i, ControlMessageOwned::ScmTimestampsns(_)))
                            {
                                // The returned timestamp is a real (i.e. not monotonic) timestamp
                                // https://docs.kernel.org/networking/timestamping.html
                                rs_wrapper.rx_ts =
                                    SystemTime::UNIX_EPOCH.checked_add(rtime.system.into());
                            }
                            // Safety: We trust `recvmsg` to have filled up `r.bytes` bytes in the buffer.
                            unsafe {
                                buf.assume_init(r.bytes);
                            }
                            buf.advance(r.bytes);
                            return Poll::Ready(Ok(()));
                        }
                        Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                        Err(e) => return Poll::Ready(Err(e)),
                    }
                }
            }
            // Unix RX timestamp only works with datagram for now, so we do not care about it
            RawStream::Unix(s) => unsafe { Pin::new_unchecked(s).poll_read(cx, buf) },
            RawStream::Virtual(s) => unsafe { Pin::new_unchecked(s).poll_read(cx, buf) },
        }
    }
}

impl AsyncWrite for RawStreamWrapper {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context, buf: &[u8]) -> Poll<io::Result<usize>> {
        // Safety: Basic enum pin projection
        unsafe {
            match &mut Pin::get_unchecked_mut(self).stream {
                RawStream::Tcp(s) => Pin::new_unchecked(s).poll_write(cx, buf),
                #[cfg(unix)]
                RawStream::Unix(s) => Pin::new_unchecked(s).poll_write(cx, buf),
                RawStream::Virtual(s) => Pin::new_unchecked(s).poll_write(cx, buf),
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        // Safety: Basic enum pin projection
        unsafe {
            match &mut Pin::get_unchecked_mut(self).stream {
                RawStream::Tcp(s) => Pin::new_unchecked(s).poll_flush(cx),
                #[cfg(unix)]
                RawStream::Unix(s) => Pin::new_unchecked(s).poll_flush(cx),
                RawStream::Virtual(s) => Pin::new_unchecked(s).poll_flush(cx),
            }
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        // Safety: Basic enum pin projection
        unsafe {
            match &mut Pin::get_unchecked_mut(self).stream {
                RawStream::Tcp(s) => Pin::new_unchecked(s).poll_shutdown(cx),
                #[cfg(unix)]
                RawStream::Unix(s) => Pin::new_unchecked(s).poll_shutdown(cx),
                RawStream::Virtual(s) => Pin::new_unchecked(s).poll_shutdown(cx),
            }
        }
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        // Safety: Basic enum pin projection
        unsafe {
            match &mut Pin::get_unchecked_mut(self).stream {
                RawStream::Tcp(s) => Pin::new_unchecked(s).poll_write_vectored(cx, bufs),
                #[cfg(unix)]
                RawStream::Unix(s) => Pin::new_unchecked(s).poll_write_vectored(cx, bufs),
                RawStream::Virtual(s) => Pin::new_unchecked(s).poll_write_vectored(cx, bufs),
            }
        }
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }
}

#[cfg(unix)]
impl AsRawFd for RawStreamWrapper {
    fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
        self.stream.as_raw_fd()
    }
}

#[cfg(windows)]
impl AsRawSocket for RawStreamWrapper {
    fn as_raw_socket(&self) -> std::os::windows::io::RawSocket {
        self.stream.as_raw_socket()
    }
}

/// The default L4 read buffer size.
///
/// Large read buffering helps reducing syscalls with little trade-off. The SSL
/// layer always does "small" reads in 16k chunks (TLS record size), so L4 read
/// buffering helps a lot.
pub const DEFAULT_L4_READ_BUFFER_SIZE: usize = 64 * 1024;

/// The default L4 write buffer size.
///
/// Small write buffering matches a typical MSS. Too large a write buffer delays
/// real-time communication. This buffering effectively implements something
/// similar to Nagle's algorithm, but user space can control when to flush.
pub const DEFAULT_L4_WRITE_BUFFER_SIZE: usize = 1460;

/// L4 [`BufStream`](tokio::io::BufStream) buffer sizing.
///
/// Leaving either side as `None` preserves Pingora's default for that side.
/// Setting either side to `Some(0)` disables `BufStream` buffering for that
/// direction.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct L4BufferSettings {
    /// Read buffer size in bytes. `None` uses [`DEFAULT_L4_READ_BUFFER_SIZE`].
    pub read: Option<usize>,
    /// Write buffer size in bytes. `None` uses [`DEFAULT_L4_WRITE_BUFFER_SIZE`].
    pub write: Option<usize>,
}

impl L4BufferSettings {
    /// Create settings with both read and write buffer sizes set explicitly.
    pub fn new(read: usize, write: usize) -> Self {
        Self {
            read: Some(read),
            write: Some(write),
        }
    }

    /// Create settings that disable both read and write `BufStream` buffering.
    pub fn unbuffered() -> Self {
        Self::new(0, 0)
    }

    /// Set the read buffer size.
    pub fn read(mut self, read: usize) -> Self {
        self.read = Some(read);
        self
    }

    /// Set the write buffer size.
    pub fn write(mut self, write: usize) -> Self {
        self.write = Some(write);
        self
    }

    /// Resolved read buffer size after applying defaults.
    pub fn read_capacity(&self) -> usize {
        self.read.unwrap_or(DEFAULT_L4_READ_BUFFER_SIZE)
    }

    /// Resolved write buffer size after applying defaults.
    pub fn write_capacity(&self) -> usize {
        self.write.unwrap_or(DEFAULT_L4_WRITE_BUFFER_SIZE)
    }
}

/// ★ 樞衡改動 22（`vendor/pingora/FORK.md`）：取代上游的 `BufStream<RawStreamWrapper>`。
/// tokio 1.54（#8494）起 `BufReader` / `BufStream` 容量 0 直接 panic，而上游建流一律 `(0, 0)`
/// （accept 熱路徑上不分配、`set_buffer` 時才建緩衝；連出去的上游連接從不 `set_buffer`）。
/// 讀容量 0 ⇒ 不要 `BufReader`、讀直接打到 raw —— 與 tokio 1.53 下容量 0 的 `BufReader` 相同（它每次讀都繞過內部緩衝）；
/// 讀容量 > 0 ⇒ 就是 `BufStream` 的內部構造。寫那一側兩態都是 `BufWriter`（容量 0 在 1.54 仍合法、不分配、每次寫都寫穿）。
/// 判據：本檔案 `mod tests` 的 `枢衡改动22_*` 與 `tests/fulcrum_read_request_alloc.rs` 的 `枢衡改动22_建流不分配`。
#[derive(Debug)]
enum L4BufStream {
    NoReadBuf(BufWriter<RawStreamWrapper>),
    ReadBuf(BufReader<BufWriter<RawStreamWrapper>>),
}

impl L4BufStream {
    fn new(read_capacity: usize, write_capacity: usize, raw: RawStreamWrapper) -> Self {
        let writer = BufWriter::with_capacity(write_capacity, raw);
        if read_capacity == 0 {
            L4BufStream::NoReadBuf(writer)
        } else {
            L4BufStream::ReadBuf(BufReader::with_capacity(read_capacity, writer))
        }
    }

    fn get_ref(&self) -> &RawStreamWrapper {
        match self {
            L4BufStream::NoReadBuf(w) => w.get_ref(),
            L4BufStream::ReadBuf(r) => r.get_ref().get_ref(),
        }
    }

    fn get_mut(&mut self) -> &mut RawStreamWrapper {
        match self {
            L4BufStream::NoReadBuf(w) => w.get_mut(),
            L4BufStream::ReadBuf(r) => r.get_mut().get_mut(),
        }
    }

    /// 與 `BufStream::into_inner` 相同：緩衝裡還沒讀走、還沒寫出的位元組都丟掉。
    fn into_inner(self) -> RawStreamWrapper {
        match self {
            L4BufStream::NoReadBuf(w) => w.into_inner(),
            L4BufStream::ReadBuf(r) => r.into_inner().into_inner(),
        }
    }
}

impl AsyncRead for L4BufStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match &mut *self {
            // `BufWriter` 的讀原樣交給它包著的 raw。
            L4BufStream::NoReadBuf(w) => Pin::new(w).poll_read(cx, buf),
            L4BufStream::ReadBuf(r) => Pin::new(r).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for L4BufStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match &mut *self {
            L4BufStream::NoReadBuf(w) => Pin::new(w).poll_write(cx, buf),
            L4BufStream::ReadBuf(r) => Pin::new(r).poll_write(cx, buf),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            L4BufStream::NoReadBuf(w) => Pin::new(w).poll_flush(cx),
            L4BufStream::ReadBuf(r) => Pin::new(r).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            L4BufStream::NoReadBuf(w) => Pin::new(w).poll_shutdown(cx),
            L4BufStream::ReadBuf(r) => Pin::new(r).poll_shutdown(cx),
        }
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        match &mut *self {
            L4BufStream::NoReadBuf(w) => Pin::new(w).poll_write_vectored(cx, bufs),
            L4BufStream::ReadBuf(r) => Pin::new(r).poll_write_vectored(cx, bufs),
        }
    }

    fn is_write_vectored(&self) -> bool {
        match self {
            L4BufStream::NoReadBuf(w) => w.is_write_vectored(),
            L4BufStream::ReadBuf(r) => r.is_write_vectored(),
        }
    }
}

// NOTE: with writer buffering, users need to call flush() to make sure the data is actually
// sent. Otherwise data could be stuck in the buffer forever or get lost when stream is closed.

/// A concrete type for transport layer connection + extra fields for logging
#[derive(Debug)]
pub struct Stream {
    // Use `Option` to be able to swap to adjust the buffer size. Always safe to unwrap
    // ★ 樞衡改動 22：上游是 `Option<BufStream<RawStreamWrapper>>`。
    stream: Option<L4BufStream>,
    // the data put back at the front of the read buffer, in order to replay the read
    rewind_read_buf: Vec<Vec<u8>>,
    buffer_write: bool,
    proxy_digest: Option<Arc<ProxyDigest>>,
    socket_digest: Option<Arc<SocketDigest>>,
    /// When this connection is established
    pub established_ts: SystemTime,
    establishment_duration: Option<Duration>,
    offload_wait_duration: Option<Duration>,
    /// The distributed tracing object for this stream
    pub tracer: Option<Tracer>,
    read_pending_time: AccumulatedDuration,
    write_pending_time: AccumulatedDuration,
    /// Last rx timestamp associated with the last recvmsg call.
    pub rx_ts: Option<SystemTime>,
}

impl Stream {
    fn stream(&self) -> &L4BufStream {
        self.stream.as_ref().expect("stream should always be set")
    }

    fn stream_mut(&mut self) -> &mut L4BufStream {
        self.stream.as_mut().expect("stream should always be set")
    }

    /// Record transport connection timing for this stream.
    pub(crate) fn set_establishment_timing(
        &mut self,
        establishment_duration: Duration,
        offload_wait_duration: Option<Duration>,
    ) {
        self.establishment_duration = Some(establishment_duration);
        self.offload_wait_duration = offload_wait_duration;
    }

    /// set TCP nodelay for this connection if `self` is TCP
    pub fn set_nodelay(&mut self) -> Result<()> {
        match &self.stream_mut().get_mut().stream {
            RawStream::Tcp(s) => {
                s.set_nodelay(true)
                    .or_err(ConnectError, "failed to set_nodelay")?;
            }
            RawStream::Virtual(s) => {
                s.set_socket_option(virt::VirtualSockOpt::NoDelay)
                    .or_err(ConnectError, "failed to set_nodelay on virtual socket")?;
            }
            _ => (),
        }
        Ok(())
    }

    /// set TCP keepalive settings for this connection if `self` is TCP
    pub fn set_keepalive(&mut self, ka: &TcpKeepalive) -> Result<()> {
        match &self.stream_mut().get_mut().stream {
            RawStream::Tcp(s) => {
                debug!("Setting tcp keepalive");
                set_tcp_keepalive(s, ka)?;
            }
            RawStream::Virtual(s) => {
                s.set_socket_option(virt::VirtualSockOpt::KeepAlive(ka.clone()))
                    .or_err(ConnectError, "failed to set_keepalive on virtual socket")?;
            }
            _ => (),
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    pub fn set_rx_timestamp(&mut self) -> Result<()> {
        use nix::sys::socket::{setsockopt, sockopt, TimestampingFlag};

        if let RawStream::Tcp(s) = &self.stream_mut().get_mut().stream {
            let timestamp_options = TimestampingFlag::SOF_TIMESTAMPING_RX_SOFTWARE
                | TimestampingFlag::SOF_TIMESTAMPING_SOFTWARE;
            setsockopt(&s, sockopt::Timestamping, &timestamp_options)
                .or_err(InternalError, "failed to set SOF_TIMESTAMPING_RX_SOFTWARE")?;
            self.stream_mut().get_mut().enable_rx_ts(true);
        }

        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    pub fn set_rx_timestamp(&mut self) -> io::Result<()> {
        Ok(())
    }

    /// Put some data back to the head of the stream to be read again.
    ///
    /// This is useful when you've read data to detect a protocol (e.g., PROXY protocol)
    /// but the data wasn't what you expected, so you need to "unread" it.
    pub fn rewind(&mut self, data: &[u8]) {
        if !data.is_empty() {
            self.rewind_read_buf.push(data.to_vec());
        }
    }

    /// Set the buffer of BufStream
    /// It is only set later because of the malloc overhead in critical accept() path
    pub(crate) fn set_buffer(&mut self, buffer: L4BufferSettings) {
        use std::mem;
        // Since BufStream doesn't provide an API to adjust the buf directly,
        // we take the raw stream out of it and put it in a new BufStream with the size we want
        let stream = mem::take(&mut self.stream);
        // ★ 樞衡改動 22：讀容量 0 時建「沒有讀緩衝」那一態（上游這裡是 `BufStream::with_capacity`，tokio 1.54 下讀容量 0 會 panic）。
        let stream = stream.map(|s| {
            L4BufStream::new(
                buffer.read_capacity(),
                buffer.write_capacity(),
                s.into_inner(),
            )
        });
        let _ = mem::replace(&mut self.stream, stream);
    }
}

impl From<TcpStream> for Stream {
    fn from(s: TcpStream) -> Self {
        Stream {
            // ★ 樞衡改動 22：上游是 `BufStream::with_capacity(0, 0, …)`（下面兩個 `From` 同）。
            stream: Some(L4BufStream::new(
                0,
                0,
                RawStreamWrapper::new(RawStream::Tcp(s)),
            )),
            rewind_read_buf: Vec::new(),
            buffer_write: true,
            established_ts: SystemTime::now(),
            establishment_duration: None,
            offload_wait_duration: None,
            proxy_digest: None,
            socket_digest: None,
            tracer: None,
            read_pending_time: AccumulatedDuration::new(),
            write_pending_time: AccumulatedDuration::new(),
            rx_ts: None,
        }
    }
}

impl From<virt::VirtualSocketStream> for Stream {
    fn from(s: virt::VirtualSocketStream) -> Self {
        Stream {
            stream: Some(L4BufStream::new(
                0,
                0,
                RawStreamWrapper::new(RawStream::Virtual(s)),
            )),
            rewind_read_buf: Vec::new(),
            buffer_write: true,
            established_ts: SystemTime::now(),
            establishment_duration: None,
            offload_wait_duration: None,
            proxy_digest: None,
            socket_digest: None,
            tracer: None,
            read_pending_time: AccumulatedDuration::new(),
            write_pending_time: AccumulatedDuration::new(),
            rx_ts: None,
        }
    }
}

#[cfg(unix)]
impl From<UnixStream> for Stream {
    fn from(s: UnixStream) -> Self {
        Stream {
            stream: Some(L4BufStream::new(
                0,
                0,
                RawStreamWrapper::new(RawStream::Unix(s)),
            )),
            rewind_read_buf: Vec::new(),
            buffer_write: true,
            established_ts: SystemTime::now(),
            establishment_duration: None,
            offload_wait_duration: None,
            proxy_digest: None,
            socket_digest: None,
            tracer: None,
            read_pending_time: AccumulatedDuration::new(),
            write_pending_time: AccumulatedDuration::new(),
            rx_ts: None,
        }
    }
}

#[cfg(unix)]
impl AsRawFd for Stream {
    fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
        self.stream().get_ref().as_raw_fd()
    }
}

#[cfg(windows)]
impl AsRawSocket for Stream {
    fn as_raw_socket(&self) -> std::os::windows::io::RawSocket {
        self.stream().get_ref().as_raw_socket()
    }
}

#[cfg(unix)]
impl UniqueID for Stream {
    fn id(&self) -> UniqueIDType {
        self.as_raw_fd()
    }
}

#[cfg(windows)]
impl UniqueID for Stream {
    fn id(&self) -> usize {
        self.as_raw_socket() as usize
    }
}

impl Ssl for Stream {}

#[async_trait]
impl Peek for Stream {
    async fn try_peek(&mut self, buf: &mut [u8]) -> std::io::Result<bool> {
        use tokio::io::AsyncReadExt;
        self.read_exact(buf).await?;
        // rewind regardless of what is read
        self.rewind(buf);
        Ok(true)
    }
}

#[async_trait]
impl Shutdown for Stream {
    async fn shutdown(&mut self) {
        AsyncWriteExt::shutdown(self).await.unwrap_or_else(|e| {
            debug!("Failed to shutdown connection: {:?}", e);
        });
    }
}

impl GetTimingDigest for Stream {
    fn get_timing_digest(&self) -> Vec<Option<TimingDigest>> {
        let mut digest = Vec::with_capacity(2); // expect to have both L4 stream and TLS layer
        digest.push(Some(TimingDigest {
            established_ts: self.established_ts,
            establishment_duration: self.establishment_duration,
            offload_wait_duration: self.offload_wait_duration,
        }));
        digest
    }

    fn get_read_pending_time(&self) -> Duration {
        self.read_pending_time.total
    }

    fn get_write_pending_time(&self) -> Duration {
        self.write_pending_time.total
    }
}

impl GetProxyDigest for Stream {
    fn get_proxy_digest(&self) -> Option<Arc<ProxyDigest>> {
        self.proxy_digest.clone()
    }

    fn set_proxy_digest(&mut self, digest: ProxyDigest) {
        self.proxy_digest = Some(Arc::new(digest));
    }
}

impl GetSocketDigest for Stream {
    fn get_socket_digest(&self) -> Option<Arc<SocketDigest>> {
        self.socket_digest.clone()
    }

    fn set_socket_digest(&mut self, socket_digest: SocketDigest) {
        self.socket_digest = Some(Arc::new(socket_digest))
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        if let Some(t) = self.tracer.as_ref() {
            t.0.on_disconnected();
        }
        /* use nodelay/local_addr function to detect socket status */
        let ret = match &self.stream().get_ref().stream {
            RawStream::Tcp(s) => s.nodelay().err(),
            #[cfg(unix)]
            RawStream::Unix(s) => s.local_addr().err(),
            RawStream::Virtual(_) => {
                // TODO: should this do something?
                None
            }
        };
        if let Some(e) = ret {
            match e.kind() {
                tokio::io::ErrorKind::Other => {
                    if let Some(ecode) = e.raw_os_error() {
                        if ecode == 9 {
                            // Or we could panic here
                            error!("Crit: socket {:?} is being double closed", self.stream);
                        }
                    }
                }
                _ => {
                    debug!("Socket is already broken {:?}", e);
                }
            }
        } else {
            // try flush the write buffer. We use now_or_never() because
            // 1. Drop cannot be async
            // 2. write should usually be ready, unless the buf is full.
            let _ = self.flush().now_or_never();
        }
        debug!("Dropping socket {:?}", self.stream);
    }
}

impl AsyncRead for Stream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let result = if !self.rewind_read_buf.is_empty() {
            let data_to_read = self.rewind_read_buf.pop().unwrap(); // safe
            let mut data_to_read = data_to_read.as_slice();
            let result = Pin::new(&mut data_to_read).poll_read(cx, buf);
            // return the remaining data back to the head of rewind_read_buf
            if !data_to_read.is_empty() {
                let remaining_buf = Vec::from(data_to_read);
                self.rewind_read_buf.push(remaining_buf);
            }
            result
        } else {
            Pin::new(&mut self.stream_mut()).poll_read(cx, buf)
        };
        // ★ 樞衡改動 16 ①：這裡（及下面三處寫）原來每次都記一筆等待計時，見 `AccumulatedDuration`。
        self.rx_ts = self.stream().get_ref().rx_ts;
        result
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.buffer_write {
            Pin::new(&mut self.stream_mut()).poll_write(cx, buf)
        } else {
            Pin::new(&mut self.stream_mut().get_mut()).poll_write(cx, buf)
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream_mut()).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream_mut()).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        if self.buffer_write {
            Pin::new(&mut self.stream_mut()).poll_write_vectored(cx, bufs)
        } else {
            Pin::new(&mut self.stream_mut().get_mut()).poll_write_vectored(cx, bufs)
        }
    }

    fn is_write_vectored(&self) -> bool {
        if self.buffer_write {
            self.stream().is_write_vectored() // it is true
        } else {
            self.stream().get_ref().is_write_vectored()
        }
    }
}

/// ★ 樞衡改動 16 ①（G157）：上游在每次 `poll_read` / `poll_write` / `poll_flush` /
/// `poll_write_vectored` 之後都記一筆等待計時（等待開始、結束各讀一次時鐘），只為
/// `get_read_pending_time` / `get_write_pending_time`。樞衡與本 fork 的 8 個 crate 裡**沒有任何讀者**
/// ⇒ 那四處呼叫刪掉了，這裡的結構與兩個 getter 原樣留著（少改上游的行），`total` 恆為零。
/// 判據：本檔案 `mod tests` 的 `枢衡改动16_读等待不再计时`。
#[derive(Debug)]
struct AccumulatedDuration {
    total: Duration,
    last_start: Option<Instant>,
}

#[allow(dead_code)] // ★ 樞衡改動 16 ①：除 `new` 外都沒有呼叫者了，見上。
impl AccumulatedDuration {
    fn new() -> Self {
        AccumulatedDuration {
            total: Duration::ZERO,
            last_start: None,
        }
    }

    fn start(&mut self) {
        if self.last_start.is_none() {
            self.last_start = Some(Instant::now());
        }
    }

    fn stop(&mut self) {
        if let Some(start) = self.last_start.take() {
            self.total += start.elapsed();
        }
    }

    fn poll_write_time(&mut self, result: &Poll<io::Result<usize>>, buf_size: usize) {
        match result {
            Poll::Ready(Ok(n)) => {
                if *n == buf_size {
                    self.stop();
                } else {
                    // partial write
                    self.start();
                }
            }
            Poll::Ready(Err(_)) => {
                self.stop();
            }
            _ => self.start(),
        }
    }

    fn poll_time(&mut self, result: &Poll<io::Result<()>>) {
        match result {
            Poll::Ready(_) => {
                self.stop();
            }
            _ => self.start(),
        }
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::io::AsyncReadExt;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;
    use tokio::sync::Notify;

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn test_rx_timestamp() {
        let message = "hello world".as_bytes();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let notify = Arc::new(Notify::new());
        let notify2 = notify.clone();

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            notify2.notified().await;
            stream.write_all(message).await.unwrap();
        });

        let mut stream: Stream = TcpStream::connect(addr).await.unwrap().into();
        stream.set_rx_timestamp().unwrap();
        // Receive the message
        // setsockopt for SO_TIMESTAMPING is asynchronous so sleep a little bit
        // to let kernel do the work
        std::thread::sleep(Duration::from_micros(100));
        notify.notify_one();

        let mut buffer = vec![0u8; message.len()];
        let n = stream.read(buffer.as_mut_slice()).await.unwrap();
        assert_eq!(n, message.len());
        assert!(stream.rx_ts.is_some());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn test_rx_timestamp_standard_path() {
        let message = "hello world".as_bytes();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let notify = Arc::new(Notify::new());
        let notify2 = notify.clone();

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            notify2.notified().await;
            stream.write_all(message).await.unwrap();
        });

        let mut stream: Stream = TcpStream::connect(addr).await.unwrap().into();
        std::thread::sleep(Duration::from_micros(100));
        notify.notify_one();

        let mut buffer = vec![0u8; message.len()];
        let n = stream.read(buffer.as_mut_slice()).await.unwrap();
        assert_eq!(n, message.len());
        assert!(stream.rx_ts.is_none());
    }

    #[tokio::test]
    async fn test_stream_rewind() {
        let message = b"hello world";
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let notify = Arc::new(Notify::new());
        let notify2 = notify.clone();

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            notify2.notified().await;
            stream.write_all(message).await.unwrap();
        });

        let mut stream: Stream = TcpStream::connect(addr).await.unwrap().into();

        let rewind_test = b"this is Sparta!";
        stream.rewind(rewind_test);

        // partially read rewind_test because of the buffer size limit
        let mut buffer = vec![0u8; message.len()];
        let n = stream.read(buffer.as_mut_slice()).await.unwrap();
        assert_eq!(n, message.len());
        assert_eq!(buffer, rewind_test[..message.len()]);

        // read the rest of rewind_test
        let n = stream.read(buffer.as_mut_slice()).await.unwrap();
        assert_eq!(n, rewind_test.len() - message.len());
        assert_eq!(buffer[..n], rewind_test[message.len()..]);

        // read the actual data
        notify.notify_one();
        let n = stream.read(buffer.as_mut_slice()).await.unwrap();
        assert_eq!(n, message.len());
        assert_eq!(buffer, message);
    }

    #[tokio::test]
    async fn test_stream_peek() {
        let message = b"hello world";
        dbg!("try peek");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let notify = Arc::new(Notify::new());
        let notify2 = notify.clone();

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            notify2.notified().await;
            stream.write_all(message).await.unwrap();
            drop(stream);
        });

        notify.notify_one();

        let mut stream: Stream = TcpStream::connect(addr).await.unwrap().into();
        let mut buffer = vec![0u8; 5];
        assert!(stream.try_peek(&mut buffer).await.unwrap());
        assert_eq!(buffer, message[0..5]);
        let mut buffer = vec![];
        stream.read_to_end(&mut buffer).await.unwrap();
        assert_eq!(buffer, message);
    }

    #[tokio::test]
    async fn test_stream_two_subsequent_peek_calls_before_read() {
        let message = b"abcdefghijklmn";

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let notify = Arc::new(Notify::new());
        let notify2 = notify.clone();

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            notify2.notified().await;
            stream.write_all(message).await.unwrap();
            drop(stream);
        });

        notify.notify_one();

        let mut stream: Stream = TcpStream::connect(addr).await.unwrap().into();

        // Peek 4 bytes
        let mut buffer = vec![0u8; 4];
        assert!(stream.try_peek(&mut buffer).await.unwrap());
        assert_eq!(buffer, message[0..4]);

        // Peek 2 bytes
        let mut buffer = vec![0u8; 2];
        assert!(stream.try_peek(&mut buffer).await.unwrap());
        assert_eq!(buffer, message[0..2]);

        // Read 1 byte: ['a']
        let mut buffer = vec![0u8; 1];
        stream.read_exact(&mut buffer).await.unwrap();
        assert_eq!(buffer, message[0..1]);

        // Read as many bytes as possible, return 1 byte ['b']
        //  from the first retry buffer chunk
        let mut buffer = vec![0u8; 100];
        let n = stream.read(&mut buffer).await.unwrap();
        assert_eq!(n, 1);
        assert_eq!(buffer[..n], message[1..2]);

        // Read the rest ['cdefghijklmn']
        let mut buffer = vec![];
        stream.read_to_end(&mut buffer).await.unwrap();
        assert_eq!(buffer, message[2..]);
    }

    /// ★ 樞衡改動 16 ① 的判據（G157）：一次**真的等過**的讀之後，讀等待時間仍是零。
    /// 上游會把那段等待（這裡約 50 ms）記進 `get_read_pending_time()` ⇒ 改動被 rebase 合掉時這裡紅。
    /// 寫那一側（`poll_write` / `poll_flush` / `poll_write_vectored`）與讀同一處改動、一起刪，
    /// 寫掛起要先灌滿對端緩衝才造得出來 ⇒ 只釘讀這一側。
    #[tokio::test]
    async fn 枢衡改动16_读等待不再计时() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let mut stream: Stream = ours.into();
        let writer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            theirs.write_all(b"x").await.unwrap();
            theirs
        });
        let mut buf = [0u8; 1];
        let started = Instant::now();
        stream.read_exact(&mut buf).await.unwrap();
        let _theirs = writer.await.unwrap();
        // 自證：這次讀確實掛起過（否則下面的零說明不了任何事）。
        assert!(
            started.elapsed() >= Duration::from_millis(40),
            "这次读没有等 —— 判据失效"
        );
        assert_eq!(
            stream.get_read_pending_time(),
            Duration::ZERO,
            "读等待又在计时了：枢衡改动 16 ① 被合掉了？见 FORK.md 改动 16"
        );
    }

    /// 改動 22 那幾條共用：一對 Unix socket，對端先寫好 `data`，等本端可讀了才建 `Stream`
    /// ⇒ 第一次讀之前資料已經全數到了本端 socket。
    async fn unix_stream_with_pending(data: &[u8]) -> (Stream, UnixStream) {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        theirs.write_all(data).await.unwrap();
        ours.readable().await.unwrap();
        (ours.into(), theirs)
    }

    /// 繞過 `Stream`、直接從底下的 socket 讀（不等）：讀緩衝裡的位元組它看不見。
    fn raw_try_read(stream: &mut Stream, buf: &mut [u8]) -> io::Result<usize> {
        match &stream.stream_mut().get_mut().stream {
            RawStream::Unix(s) => s.try_read(buf),
            _ => unreachable!("改动 22 这几条测试只用 UnixStream"),
        }
    }

    /// ★ 樞衡改動 22：沒有讀緩衝（建流時如此、連出去的上游連接一直如此、`unbuffered()` 也是）⇒ 讀直接打到 socket，
    /// 讀走 1 位元組之後剩下的還在 socket 上。tokio 1.53 下容量 0 的 `BufReader` 正是這樣（每次讀都繞過內部緩衝）。
    #[tokio::test]
    async fn 枢衡改动22_没有读缓冲时读直接打到socket() {
        for settings in [None, Some(L4BufferSettings::unbuffered())] {
            let (mut stream, _theirs) = unix_stream_with_pending(b"0123456789").await;
            if let Some(settings) = settings {
                stream.set_buffer(settings);
            }
            let mut one = [0u8; 1];
            stream.read_exact(&mut one).await.unwrap();
            assert_eq!(&one, b"0");
            let mut rest = [0u8; 16];
            let n = raw_try_read(&mut stream, &mut rest).unwrap();
            assert_eq!(
                &rest[..n],
                b"123456789",
                "{settings:?}：剩下的不在 socket 上 —— 读被缓冲了"
            );
        }
    }

    /// ★ 樞衡改動 22 的另一個方向：`set_buffer` 給了讀容量（pingora 預設 64 KiB）⇒ 第一次讀就把 socket 上的全收進緩衝。
    #[tokio::test]
    async fn 枢衡改动22_有读缓冲时数据进了缓冲() {
        let (mut stream, _theirs) = unix_stream_with_pending(b"0123456789").await;
        stream.set_buffer(L4BufferSettings::default());
        let mut one = [0u8; 1];
        stream.read_exact(&mut one).await.unwrap();
        assert_eq!(&one, b"0");
        let mut rest = [0u8; 16];
        let err = raw_try_read(&mut stream, &mut rest).unwrap_err();
        assert_eq!(
            err.kind(),
            io::ErrorKind::WouldBlock,
            "socket 上还有数据 —— 读没有经过缓冲"
        );
        let n = stream.read(&mut rest).await.unwrap();
        assert_eq!(&rest[..n], b"123456789");
    }

    /// ★ 樞衡改動 22：讀 0、寫 > 0（改動 22 之前由同一個 `BufStream` 承擔，之後是「沒有讀緩衝」那一態裡的 `BufWriter`）
    /// ⇒ 寫（含分段寫）在 `flush` 之前留在緩衝裡、之後才發出去；讀照舊直接打到 socket。
    #[tokio::test]
    async fn 枢衡改动22_只缓冲写() {
        let (mut stream, mut theirs) = unix_stream_with_pending(b"0123456789").await;
        stream.set_buffer(L4BufferSettings::new(0, 16 * 1024));
        let n = stream
            .write_vectored(&[std::io::IoSlice::new(b"ab"), std::io::IoSlice::new(b"c")])
            .await
            .unwrap();
        assert_eq!(n, 3);
        let mut got = [0u8; 8];
        assert!(
            tokio::time::timeout(Duration::from_millis(100), theirs.read(&mut got))
                .await
                .is_err(),
            "没 flush 对端就收到了 —— 写没有经过缓冲"
        );
        stream.flush().await.unwrap();
        let n = theirs.read(&mut got).await.unwrap();
        assert_eq!(&got[..n], b"abc");

        let mut one = [0u8; 1];
        stream.read_exact(&mut one).await.unwrap();
        let mut rest = [0u8; 16];
        let n = raw_try_read(&mut stream, &mut rest).unwrap();
        assert_eq!(&rest[..n], b"123456789", "这一态的读被缓冲了");
    }

    /// ★ 樞衡改動 22：建流時寫容量是 0（連出去的上游連接一直如此）⇒ 不 `flush` 也立刻寫穿。
    #[tokio::test]
    async fn 枢衡改动22_写容量0不flush也发出去() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let mut stream: Stream = ours.into();
        stream.write_all(b"abc").await.unwrap();
        let mut got = [0u8; 8];
        let n = tokio::time::timeout(Duration::from_secs(1), theirs.read(&mut got))
            .await
            .expect("没 flush 对端就收不到 —— 写被缓冲了")
            .unwrap();
        assert_eq!(&got[..n], b"abc");
    }
}
