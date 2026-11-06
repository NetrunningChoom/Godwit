// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! LiveKit's signalling WebSocket, opened IPv4 first.
//!
//! livekit-net's own client tries the addresses in DNS order. Stoat's voice
//! host lists IPv6 first, and on machines whose IPv6 is broken Windows takes
//! about 7 s to give up on those (it retries refused connections), longer
//! than LiveKit's 5 s connect timeout, so the call never reaches IPv4. Here
//! every IPv4 address is tried before any IPv6 one, each with its own short
//! timeout. With a proxy configured, LiveKit's own client handles it.

use std::net::SocketAddr;
use std::sync::{Arc, Once};
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use livekit_net::{Header, TransportError, WsClient, WsConnectResult, WsConnection};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::error::ProtocolError;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// How long one address gets before the next is tried.
const PER_ADDRESS: Duration = Duration::from_secs(3);

/// Registers the client for the whole process; later calls do nothing.
pub fn install() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| livekit_net::set_ws_client(Arc::new(Ipv4First)));
}

struct Ipv4First;

fn proxy_configured() -> bool {
    ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"]
        .iter()
        .any(|k| std::env::var(k).is_ok_and(|v| !v.is_empty()))
}

/// IPv4 addresses first, each family keeping its DNS order.
fn ipv4_first(mut addrs: Vec<SocketAddr>) -> Vec<SocketAddr> {
    addrs.sort_by_key(|a| a.is_ipv6());
    addrs
}

fn conn_err(e: impl std::fmt::Display) -> TransportError {
    TransportError::Connection(e.to_string())
}

async fn open_tcp(host: &str, port: u16) -> Result<TcpStream, TransportError> {
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port)).await.map_err(conn_err)?.collect();
    let mut last = format!("{host} has no addresses");
    for addr in ipv4_first(addrs) {
        match tokio::time::timeout(PER_ADDRESS, TcpStream::connect(addr)).await {
            Ok(Ok(stream)) => {
                // Signalling frames are small; don't hold them back.
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Ok(Err(e)) => last = format!("{addr}: {e}"),
            Err(_) => last = format!("{addr}: timed out"),
        }
    }
    Err(TransportError::Connection(last))
}

#[async_trait::async_trait]
impl WsClient for Ipv4First {
    async fn connect(
        &self,
        url: String,
        headers: Vec<Header>,
        timeout_ms: u64,
    ) -> Result<WsConnectResult, TransportError> {
        if proxy_configured() {
            return livekit_net::testing::native_ws_client()
                .connect(url, headers, timeout_ms)
                .await;
        }
        let parsed = url::Url::parse(&url).map_err(conn_err)?;
        let host = parsed
            .host_str()
            .ok_or_else(|| conn_err("voice URL has no host"))?
            .to_owned();
        let port = parsed
            .port_or_known_default()
            .ok_or_else(|| conn_err("voice URL has no port"))?;
        let mut request = url.as_str().into_client_request().map_err(conn_err)?;
        for h in &headers {
            let name: http::HeaderName = h
                .name
                .parse()
                .map_err(|_| TransportError::Other("bad header name".into()))?;
            let value =
                http::HeaderValue::from_str(&h.value).map_err(|_| TransportError::Other("bad header value".into()))?;
            request.headers_mut().insert(name, value);
        }
        // The overall budget: every address may fail, so give it more room
        // than LiveKit's default, which is sized for a single attempt.
        let budget = Duration::from_millis(timeout_ms).max(Duration::from_secs(15));
        let connect = async {
            let tcp = open_tcp(&host, port).await?;
            let (ws, _) = tokio_tungstenite::client_async_tls(request, tcp)
                .await
                .map_err(conn_err)?;
            Ok::<_, TransportError>(ws)
        };
        let ws = tokio::time::timeout(budget, connect)
            .await
            .map_err(|_| TransportError::Timeout)??;
        let (writer, reader) = ws.split();
        Ok(WsConnectResult {
            connection: Arc::new(Connection {
                writer: Mutex::new(writer),
                reader: Mutex::new(reader),
            }),
        })
    }
}

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Binary frames only; pings are answered here (as livekit-net does).
struct Connection {
    writer: Mutex<SplitSink<Socket, Message>>,
    reader: Mutex<SplitStream<Socket>>,
}

#[async_trait::async_trait]
impl WsConnection for Connection {
    async fn send(&self, frame: Vec<u8>) -> Result<(), TransportError> {
        self.writer
            .lock()
            .await
            .send(Message::Binary(frame.into()))
            .await
            .map_err(conn_err)
    }

    async fn recv(&self) -> Result<Option<Vec<u8>>, TransportError> {
        let mut reader = self.reader.lock().await;
        loop {
            match reader.next().await {
                Some(Ok(Message::Binary(data))) => return Ok(Some(data.to_vec())),
                Some(Ok(Message::Ping(payload))) => {
                    let _ = self.writer.lock().await.send(Message::Pong(payload)).await;
                }
                Some(Ok(Message::Pong(_) | Message::Frame(_) | Message::Text(_))) => {}
                Some(Ok(Message::Close(_))) | None => return Ok(None),
                Some(Err(
                    WsError::Protocol(ProtocolError::ResetWithoutClosingHandshake)
                    | WsError::ConnectionClosed
                    | WsError::AlreadyClosed,
                )) => return Ok(None),
                // The server dropping TLS without close_notify is a normal close too.
                Some(Err(WsError::Io(e))) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
                Some(Err(e)) => return Err(conn_err(e)),
            }
        }
    }

    async fn close(&self) {
        let _ = self.writer.lock().await.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv4_goes_first_keeping_dns_order() {
        let a: Vec<SocketAddr> = ["[2a01::1]:443", "1.1.1.1:443", "[2a01::2]:443", "2.2.2.2:443"]
            .iter()
            .filter_map(|s| s.parse().ok())
            .collect();
        let sorted: Vec<String> = ipv4_first(a).iter().map(ToString::to_string).collect();
        assert_eq!(sorted, ["1.1.1.1:443", "2.2.2.2:443", "[2a01::1]:443", "[2a01::2]:443"]);
    }
}
