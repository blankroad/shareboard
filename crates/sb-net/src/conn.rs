//! 프레이밍된 TLS 연결 래퍼 (§5.1). `Envelope<C2s>`/`Envelope<S2c>` 를 주고받는다.

use std::net::SocketAddr;

use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::{client::TlsStream, TlsConnector};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

use sb_crypto::Identity;
use sb_proto::params::MAX_FRAME;
use sb_proto::{decode_env, encode_env, is_lan_allowed, C2s, S2c};

use crate::tls::{client_config, dummy_server_name};
use crate::NetError;

/// u32 LE 길이 접두 + 256KiB 상한 프레이머 (§5.1). 클라·서버 동일 설정.
pub fn framed_codec() -> LengthDelimitedCodec {
    LengthDelimitedCodec::builder()
        .little_endian()
        .length_field_length(4)
        .max_frame_length(MAX_FRAME)
        .new_codec()
}

/// 서버와의 Member lane 연결. 제네릭 스트림(테스트에서 교체 가능).
pub struct Connection<S = TlsStream<TcpStream>> {
    framed: Framed<S, LengthDelimitedCodec>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Connection<S> {
    pub fn from_stream(stream: S) -> Self {
        Self {
            framed: Framed::new(stream, framed_codec()),
        }
    }

    /// C2s 메시지 전송.
    pub async fn send(&mut self, msg: C2s) -> Result<(), NetError> {
        let bytes = encode_env(msg)?;
        self.framed.send(bytes.into()).await.map_err(NetError::Io)?;
        Ok(())
    }

    /// S2c 메시지 수신. 연결 종료 시 `Closed`.
    pub async fn recv(&mut self) -> Result<S2c, NetError> {
        match self.framed.next().await {
            Some(Ok(frame)) => Ok(decode_env(&frame)?),
            Some(Err(e)) => Err(NetError::Io(e)),
            None => Err(NetError::Closed),
        }
    }
}

/// TCP+TLS 로 서버에 접속(주소 allowlist 검증 → mTLS + 지문 pinning). Member/Guest 공통.
pub async fn connect(
    addr: SocketAddr,
    server_fp: [u8; 32],
    identity: &Identity,
) -> Result<Connection, NetError> {
    // §4.5: 사설망 주소만 다이얼. 바이트 전송 전 거부.
    if !is_lan_allowed(&addr.ip()) {
        return Err(NetError::AddressNotAllowed(addr.ip().to_string()));
    }
    let cfg = client_config(identity, server_fp)?;
    let connector = TlsConnector::from(cfg);
    let tcp = TcpStream::connect(addr).await.map_err(NetError::Io)?;
    tcp.set_nodelay(true).ok();
    let tls = connector
        .connect(dummy_server_name(), tcp)
        .await
        .map_err(NetError::Io)?;
    Ok(Connection::from_stream(tls))
}

/// 연결을 채널로 감싼 핸들. writer/reader task 가 프레이밍·인코딩을 담당한다.
/// 상위(Tauri 워커)는 `out.send(C2s)` / `inbox.recv()` 만 사용 → borrow 충돌 없음.
pub struct ClientHandle {
    pub out: tokio::sync::mpsc::Sender<C2s>,
    pub inbox: tokio::sync::mpsc::Receiver<S2c>,
}

/// 연결을 백그라운드 reader/writer task 로 구동하고 채널 핸들을 반환한다.
pub fn spawn<S>(conn: Connection<S>) -> ClientHandle
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut sink, mut stream) = conn.framed.split();
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<C2s>(64);
    let (in_tx, in_rx) = tokio::sync::mpsc::channel::<S2c>(64);

    tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            match encode_env(msg) {
                Ok(bytes) => {
                    let frame: bytes::Bytes = bytes.into();
                    if sink.send(frame).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    tokio::spawn(async move {
        while let Some(item) = stream.next().await {
            let frame = match item {
                Ok(f) => f,
                Err(_) => break,
            };
            match decode_env::<S2c>(&frame) {
                Ok(msg) => {
                    if in_tx.send(msg).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    ClientHandle {
        out: out_tx,
        inbox: in_rx,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[tokio::test]
    async fn rejects_public_address() {
        let id = Identity::generate();
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 45871);
        let res = connect(addr, [0u8; 32], &id).await;
        assert!(
            matches!(res, Err(NetError::AddressNotAllowed(_))),
            "공인 IP 는 거부"
        );
    }
}
