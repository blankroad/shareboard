//! 실행 중인 sb-server 에 실제 TCP+TLS 로 붙어 워크스페이스를 만들고 Welcome 을 받는 스모크 클라이언트.
//!
//! 사용: smoke_client <addr> <server_fp_hex> <setup_token>

use std::net::SocketAddr;
use std::time::{SystemTime, UNIX_EPOCH};

use sb_crypto::{wslog, Identity};
use sb_net::init_crypto;
use sb_proto::{C2s, S2c};

fn hex32(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, c) in s.as_bytes().chunks(2).enumerate() {
        let hi = (c[0] as char).to_digit(16)?;
        let lo = (c[1] as char).to_digit(16)?;
        out[i] = (hi * 16 + lo) as u8;
    }
    Some(out)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("사용: smoke_client <addr> <server_fp_hex> <setup_token>");
        std::process::exit(2);
    }
    let addr: SocketAddr = args[1].parse()?;
    let fp = hex32(&args[2]).ok_or_else(|| anyhow::anyhow!("지문 hex 64자 필요"))?;
    let token = args[3].clone();

    init_crypto();
    let founder = Identity::generate();
    println!(
        "[client] device_id = {}",
        sb_proto::short_id(&founder.device_id())
    );

    // 실제 TCP + TLS 1.3 + 지문 pinning + mTLS 로 접속.
    let mut conn = sb_net::connect(addr, fp, &founder).await?;
    println!("[client] TLS 연결 성공 (지문 pinning + mTLS)");

    // 워크스페이스 생성(창립자).
    let (genesis, _wid) = wslog::build_genesis(&founder, "smoke-team", now_ms());
    conn.send(C2s::ClaimWorkspace {
        token,
        genesis: wslog::entry_bytes(&genesis),
    })
    .await?;
    match conn.recv().await? {
        S2c::AppendAck { seq, .. } => println!("[client] ClaimWorkspace OK (genesis seq={seq})"),
        other => {
            println!("[client] ClaimWorkspace 실패: {other:?}");
            std::process::exit(1);
        }
    }

    // 멤버로 Hello → Welcome.
    conn.send(C2s::Hello(sb_proto::Hello {
        device_id: founder.device_id(),
        proto_min: 2,
        proto_max: 2,
        app_version: "smoke".into(),
        epoch: 0,
        log_head: (0, [0u8; 32]),
    }))
    .await?;
    match conn.recv().await? {
        S2c::Welcome(w) => {
            println!(
                "[client] ✅ Welcome 수신 — epoch={}, log_tail={}개, presence={}명, chosen_version={}",
                w.epoch,
                w.log_tail.len(),
                w.presence.len(),
                w.chosen_version
            );
        }
        other => println!("[client] Hello 응답 예상 밖: {other:?}"),
    }

    conn.send(C2s::Bye {
        reason: sb_proto::ByeReason::Shutdown,
    })
    .await?;
    println!("[client] 완료");
    Ok(())
}
