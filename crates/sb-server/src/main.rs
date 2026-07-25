//! shareboard blind relay 서버 바이너리.
//!
//! `sb-server --config server.toml` — LAN 인터페이스에 바인딩, 자기서명 cert 지문을 출력한다.
//! `sb-server --gen-token` — setup 토큰과 그 해시(server.toml 용)를 생성.

use std::net::SocketAddr;
use std::path::Path;

use anyhow::{bail, Context, Result};
use clap::Parser;
use rustls::pki_types::CertificateDer;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use sb_crypto::hash::sha256;
use sb_crypto::{invite, Identity};
use sb_net::{cert_fingerprint, init_crypto};
use sb_server::{config::ServerConfig, serve, tls, Shared};

#[derive(Parser)]
#[command(name = "sb-server", about = "shareboard blind relay 서버")]
struct Args {
    #[arg(long, default_value = "server.toml")]
    config: String,
    /// setup 토큰 + 해시를 생성하고 종료.
    #[arg(long)]
    gen_token: bool,
    /// 서버를 한 번에 초기화: server.toml 작성 + 신원 생성 + 토큰/지문 출력 후 종료.
    #[arg(long)]
    init: bool,
    /// (--init) 바인딩 주소. LAN 주소여야 함. 여러 대에서 쓰려면 서버의 사내망 IP:포트.
    #[arg(long, default_value = "127.0.0.1:45871")]
    bind: String,
    /// (--init) 데이터 디렉터리(신원·로그·초대 저장).
    #[arg(long, default_value = "./sb-server-data")]
    data_dir: String,
}

fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

fn load_or_create_identity(data_dir: &str) -> Result<Identity> {
    std::fs::create_dir_all(data_dir).ok();
    let path = Path::new(data_dir).join("identity.bin");
    if path.exists() {
        let raw = std::fs::read(&path)?;
        if raw.len() < 4 {
            bail!("identity.bin 손상");
        }
        let len = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as usize;
        let pkcs8 = &raw[4..4 + len];
        let kem: [u8; 32] = raw[4 + len..4 + len + 32].try_into().context("kem")?;
        Ok(Identity::from_parts(pkcs8, &kem).map_err(|e| anyhow::anyhow!("{e}"))?)
    } else {
        let id = Identity::generate();
        let pkcs8 = id.signing_pkcs8_der().map_err(|e| anyhow::anyhow!("{e}"))?;
        let kem = id.kem_secret_bytes();
        let mut buf = Vec::new();
        buf.extend_from_slice(&(pkcs8.len() as u32).to_le_bytes());
        buf.extend_from_slice(&pkcs8);
        buf.extend_from_slice(&kem);
        std::fs::write(&path, &buf)?;
        set_0600(&path);
        Ok(id)
    }
}

#[cfg(unix)]
fn set_0600(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}
#[cfg(not(unix))]
fn set_0600(_path: &Path) {}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args = Args::parse();

    if args.gen_token {
        // 관리자용: 토큰과 해시 출력(토큰은 창립자에게, 해시는 server.toml 에).
        let token = invite::generate_code();
        let h = sha256(token.as_bytes());
        println!("setup_token = {}", &*token);
        println!("setup_token_hash = \"{}\"   # server.toml 에 넣으세요", hex(&h));
        return Ok(());
    }

    if args.init {
        // 원스톱 초기화: server.toml 작성 + 신원 생성 + 토큰/지문 안내.
        let bind: SocketAddr = args.bind.parse().context("--bind 파싱 (host:port)")?;
        if !sb_proto::is_lan_allowed(&bind.ip()) {
            bail!(
                "--bind {} 는 LAN(사설망) 주소여야 합니다 (§4.5). 예: 127.0.0.1:45871 또는 192.168.x.y:45871",
                bind.ip()
            );
        }
        let id = load_or_create_identity(&args.data_dir)?;
        let (cert_der, _) = id.tls_material().map_err(|e| anyhow::anyhow!("{e}"))?;
        let fp = cert_fingerprint(&CertificateDer::from(cert_der));
        let token = invite::generate_code();
        let hash = sha256(token.as_bytes());

        let toml = format!(
            "# shareboard 서버 설정 (sb-server --init 로 생성)\n\
             bind_addr = \"{bind}\"\n\
             data_dir = \"{}\"\n\
             setup_token_hash = \"{}\"\n",
            args.data_dir,
            hex(&hash),
        );
        std::fs::write(&args.config, &toml).context("server.toml 쓰기")?;

        println!("✅ 서버 설정 완료 → {}\n", args.config);
        println!("① 이 서버 실행:");
        println!("     sb-server --config {}\n", args.config);
        println!("② 워크스페이스를 처음 만들 사람(창립자)에게 전달할 setup 토큰:");
        println!("     {}\n", &*token);
        println!("③ 참여할 모든 멤버가 앱에 입력할 값:");
        println!("     서버 주소 : {bind}");
        println!("     서버 지문 : {}\n", hex(&fp));
        println!(
            "※ 여러 대에서 쓰려면 --bind 를 서버 PC 의 사내망 IP 로:  sb-server --init --bind 192.168.0.10:45871"
        );
        return Ok(());
    }

    init_crypto();
    let cfg = ServerConfig::load(&args.config)?;
    let addr: SocketAddr = cfg.bind_addr.parse().context("bind_addr 파싱")?;
    if !sb_proto::is_lan_allowed(&addr.ip()) {
        bail!("bind_addr {} 은 LAN allowlist 밖입니다 (§4.5)", addr.ip());
    }

    let id = load_or_create_identity(&cfg.data_dir)?;
    let (cert_der, _) = id.tls_material().map_err(|e| anyhow::anyhow!("{e}"))?;
    let fp = cert_fingerprint(&CertificateDer::from(cert_der));
    tracing::info!("shareboard 서버 시작: {addr}");
    println!("server fingerprint (SHA-256 SPKI): {}", hex(&fp));
    println!("  → 초대 URI/설정에 이 지문을 넣어 클라이언트가 pinning 합니다.");

    let acceptor = TlsAcceptor::from(tls::server_config(&id).map_err(|e| anyhow::anyhow!("{e}"))?);
    let shared = Shared::new(cfg.setup_token_hash_bytes());
    let listener = TcpListener::bind(addr).await.context("bind 실패")?;
    serve(listener, acceptor, shared).await;
    Ok(())
}
