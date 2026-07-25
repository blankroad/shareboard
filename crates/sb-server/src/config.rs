//! 서버 설정 (server.toml) (§7.4).

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    /// "192.168.x.y:45871" 형태. LAN 주소여야 함(§4.5).
    pub bind_addr: String,
    #[serde(default = "default_data_dir")]
    pub data_dir: String,
    /// 관리자가 오프라인 생성한 setup 토큰의 SHA-256(hex). 서버는 평문 토큰을 보관하지 않음(§4.3.2).
    #[serde(default)]
    pub setup_token_hash: Option<String>,
    #[serde(default)]
    pub log_dir: Option<String>,
    #[serde(default = "default_health")]
    pub health_bind: Option<String>,
}

fn default_data_dir() -> String {
    "./data".into()
}
fn default_health() -> Option<String> {
    Some("127.0.0.1:45872".into())
}

impl ServerConfig {
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let s = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("설정 파일 읽기 실패 {path}: {e}"))?;
        Ok(toml::from_str(&s)?)
    }

    pub fn setup_token_hash_bytes(&self) -> Option<[u8; 32]> {
        self.setup_token_hash.as_deref().and_then(hex_to_32)
    }
}

/// 64자 hex → [u8;32].
pub fn hex_to_32(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char).to_digit(16)?;
        let lo = (chunk[1] as char).to_digit(16)?;
        out[i] = (hi * 16 + lo) as u8;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_toml() {
        let toml = r#"
            bind_addr = "192.168.1.10:45871"
            setup_token_hash = "aabb00112233445566778899aabbccddeeff00112233445566778899aabbccdd"
        "#;
        let c: ServerConfig = toml::from_str(toml).unwrap();
        assert_eq!(c.bind_addr, "192.168.1.10:45871");
        assert_eq!(c.data_dir, "./data");
        assert!(c.setup_token_hash_bytes().is_some());
    }

    #[test]
    fn hex_parse() {
        assert_eq!(hex_to_32("00").is_none(), true);
        let h = "ff".repeat(32);
        assert_eq!(hex_to_32(&h), Some([0xffu8; 32]));
    }
}
