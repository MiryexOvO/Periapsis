//! Bridge upstream transport trait 0.5 to the interface required by moq-net.
//! 将新版传输接口适配到 moq-net 所需的旧接口，不修改线格式或握手。

use moq_net::{
    bytes::{Buf, BufMut, Bytes},
    web_transport_trait as old,
};
use std::{
    fmt,
    task::{Context, Poll},
    time::Duration,
};
use transport_v5 as new;

#[derive(Clone, Debug)]
pub(crate) struct Compat<T>(pub T);

impl<E: fmt::Display> fmt::Display for Compat<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl<E: new::Error> std::error::Error for Compat<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}
impl<E: new::Error> old::Error for Compat<E> {
    fn session_error(&self) -> Option<(u32, String)> {
        self.0.session_error()
    }
    fn stream_error(&self) -> Option<u32> {
        self.0.stream_error()
    }
}

// 同名统计字段逐项转发；新版 streams 快照在旧接口中没有对应字段。
impl<S: new::Stats> old::Stats for Compat<S> {
    fn bytes_sent(&self) -> Option<u64> {
        self.0.bytes_sent()
    }
    fn bytes_received(&self) -> Option<u64> {
        self.0.bytes_received()
    }
    fn bytes_lost(&self) -> Option<u64> {
        self.0.bytes_lost()
    }
    fn packets_sent(&self) -> Option<u64> {
        self.0.packets_sent()
    }
    fn packets_received(&self) -> Option<u64> {
        self.0.packets_received()
    }
    fn packets_lost(&self) -> Option<u64> {
        self.0.packets_lost()
    }
    fn rtt(&self) -> Option<Duration> {
        self.0.rtt()
    }
    fn estimated_send_rate(&self) -> Option<u64> {
        self.0.estimated_send_rate()
    }
}

impl<S: new::poll::Session> old::poll::Session for Compat<S> {
    type SendStream = Compat<S::SendStream>;
    type RecvStream = Compat<S::RecvStream>;
    type Error = Compat<S::Error>;
    fn poll_accept_uni(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::RecvStream, Self::Error>> {
        self.0
            .poll_accept_uni(cx)
            .map(|r| r.map(Compat).map_err(Compat))
    }
    fn poll_accept_bi(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<old::poll::BiStreams<Self>, Self::Error>> {
        self.0
            .poll_accept_bi(cx)
            .map(|r| r.map(|(s, r)| (Compat(s), Compat(r))).map_err(Compat))
    }
    fn poll_open_uni(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Self::SendStream, Self::Error>> {
        self.0
            .poll_open_uni(cx)
            .map(|r| r.map(Compat).map_err(Compat))
    }
    fn poll_open_bi(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<old::poll::BiStreams<Self>, Self::Error>> {
        self.0
            .poll_open_bi(cx)
            .map(|r| r.map(|(s, r)| (Compat(s), Compat(r))).map_err(Compat))
    }
    fn poll_send_datagram(
        &mut self,
        cx: &mut Context<'_>,
        payload: &[u8],
    ) -> Poll<Result<(), Self::Error>> {
        self.0
            .poll_send_datagram(cx, payload)
            .map(|r| r.map_err(Compat))
    }
    fn poll_recv_datagram(&mut self, cx: &mut Context<'_>) -> Poll<Result<Bytes, Self::Error>> {
        self.0.poll_recv_datagram(cx).map(|r| r.map_err(Compat))
    }
    fn max_datagram_size(&self) -> usize {
        self.0.max_datagram_size()
    }
    fn protocol(&self) -> Option<&str> {
        self.0.protocol()
    }
    fn close(&mut self, code: u32, reason: &str) {
        self.0.close(code, reason);
    }
    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Self::Error> {
        self.0.poll_closed(cx).map(Compat)
    }
    fn stats(&self) -> impl old::Stats {
        Compat(self.0.stats())
    }
}

impl<S: new::poll::SendStream> old::poll::SendStream for Compat<S> {
    type Error = Compat<S::Error>;
    fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, Self::Error>> {
        self.0.poll_write(cx, buf).map(|r| r.map_err(Compat))
    }
    fn poll_write_buf<B: Buf>(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut B,
    ) -> Poll<Result<usize, Self::Error>> {
        self.0.poll_write_buf(cx, buf).map(|r| r.map_err(Compat))
    }
    fn set_priority(&mut self, order: u8) {
        // 旧接口的全部取值可无损扩展，保持数值越高越优先的语义。
        self.0.set_priority(i32::from(order));
    }
    fn finish(&mut self) -> Result<(), Self::Error> {
        self.0.finish().map_err(Compat)
    }
    fn reset(&mut self, code: u32) {
        self.0.reset(code);
    }
    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.0.poll_closed(cx).map(|r| r.map_err(Compat))
    }
}

impl<S: new::poll::RecvStream> old::poll::RecvStream for Compat<S> {
    type Error = Compat<S::Error>;
    fn poll_read(
        &mut self,
        cx: &mut Context<'_>,
        dst: &mut [u8],
    ) -> Poll<Result<Option<usize>, Self::Error>> {
        self.0.poll_read(cx, dst).map(|r| r.map_err(Compat))
    }
    fn poll_read_buf<B: BufMut>(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut B,
    ) -> Poll<Result<Option<usize>, Self::Error>> {
        self.0.poll_read_buf(cx, buf).map(|r| r.map_err(Compat))
    }
    fn poll_read_chunk(
        &mut self,
        cx: &mut Context<'_>,
        max: usize,
    ) -> Poll<Result<Option<Bytes>, Self::Error>> {
        self.0.poll_read_chunk(cx, max).map(|r| r.map_err(Compat))
    }
    fn stop(&mut self, code: u32) {
        self.0.stop(code);
    }
    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.0.poll_closed(cx).map(|r| r.map_err(Compat))
    }
}
