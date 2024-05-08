use std::{
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    task::Poll,
};

use tokio::io::{AsyncRead, AsyncWrite};

use super::Connection;

/// Wrapper which keeps track of how much bytes have been read and written from a connection.
pub struct Tracked<C> {
    /// Bytes read counter
    read: Arc<AtomicU64>,
    /// Bytes written counter
    write: Arc<AtomicU64>,
    /// Underlying connection we are measuring
    con: C,
}

impl<C> Tracked<C>
where
    C: Connection + Unpin,
{
    /// Create a new instance of a tracked connections. Counters are passed in so they can be
    /// reused accross connections.
    pub fn new(read: Arc<AtomicU64>, write: Arc<AtomicU64>, con: C) -> Self {
        Self { read, write, con }
    }
}

impl<C> Connection for Tracked<C>
where
    C: Connection + Unpin,
{
    #[inline]
    fn identifier(&self) -> Result<String, std::io::Error> {
        self.con.identifier()
    }

    #[inline]
    fn static_link_cost(&self) -> Result<u16, std::io::Error> {
        self.con.static_link_cost()
    }
}

impl<C> AsyncRead for Tracked<C>
where
    C: AsyncRead + Unpin,
{
    #[inline]
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let start_len = buf.filled().len();
        let res = Pin::new(&mut self.con).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = res {
            self.read
                .fetch_add((buf.filled().len() - start_len) as u64, Ordering::Relaxed);
        }
        res
    }
}

impl<C> AsyncWrite for Tracked<C>
where
    C: AsyncWrite + Unpin,
{
    #[inline]
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        let res = Pin::new(&mut self.con).poll_write(cx, buf);
        if let Poll::Ready(Ok(written)) = res {
            self.write.fetch_add(written as u64, Ordering::Relaxed);
        }
        res
    }

    #[inline]
    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.con).poll_flush(cx)
    }

    #[inline]
    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.con).poll_shutdown(cx)
    }

    #[inline]
    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<Result<usize, std::io::Error>> {
        let res = Pin::new(&mut self.con).poll_write_vectored(cx, bufs);
        if let Poll::Ready(Ok(written)) = res {
            self.write.fetch_add(written as u64, Ordering::Relaxed);
        }
        res
    }

    #[inline]
    fn is_write_vectored(&self) -> bool {
        self.con.is_write_vectored()
    }
}
