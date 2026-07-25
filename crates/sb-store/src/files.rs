//! 원자적 파일 저장 (§7.1). settings.json 은 temp→rename, workspace.json 은 MAC 보호(D22).

use std::path::Path;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use sb_crypto::hash::hmac_sha256;

use crate::StoreError;

/// temp 파일에 쓰고 rename — 부분 쓰기(torn write) 방지.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("file");
    let tmp = dir.join(format!(".{fname}.tmp"));
    std::fs::write(&tmp, bytes)?;
    set_0600(&tmp);
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<(), StoreError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| StoreError::Serde(e.to_string()))?;
    atomic_write(path, &bytes)
}

pub fn load_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, StoreError> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(
            serde_json::from_slice(&b).map_err(|e| StoreError::Serde(e.to_string()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(StoreError::Io(e)),
    }
}

/// MAC 로 보호되는 저장 래퍼(workspace.json 캐시 — D22).
#[derive(Serialize, Deserialize)]
struct Maced {
    data: Vec<u8>,
    mac: [u8; 32],
}

/// `data` 를 HMAC-SHA256(mac_key) 로 서명해 저장.
pub fn save_maced(path: &Path, mac_key: &[u8; 32], data: &[u8]) -> Result<(), StoreError> {
    let wrapper = Maced {
        data: data.to_vec(),
        mac: hmac_sha256(mac_key, data),
    };
    save_json(path, &wrapper)
}

/// MAC 검증 후 로드. 불일치 시 `MacMismatch`(변조 의심 — 호출자는 캐시 격리·로그 재취득, §7.1).
pub fn load_maced(path: &Path, mac_key: &[u8; 32]) -> Result<Option<Vec<u8>>, StoreError> {
    let w: Option<Maced> = load_json(path)?;
    match w {
        None => Ok(None),
        Some(w) => {
            // 상수시간 비교까지는 아니어도 되지만(로컬 파일), 단순 일치 검사.
            if hmac_sha256(mac_key, &w.data) == w.mac {
                Ok(Some(w.data))
            } else {
                Err(StoreError::MacMismatch)
            }
        }
    }
}

#[cfg(unix)]
fn set_0600(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600));
}
#[cfg(not(unix))]
fn set_0600(_p: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static CNT: AtomicU64 = AtomicU64::new(0);
        let n = CNT.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("sb-files-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn json_atomic_roundtrip() {
        let dir = tmpdir();
        let path = dir.join("settings.json");
        let v = serde_json::json!({"a": 1, "b": "x"});
        save_json(&path, &v).unwrap();
        let back: serde_json::Value = load_json(&path).unwrap().unwrap();
        assert_eq!(back, v);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn maced_detects_tampering() {
        let dir = tmpdir();
        let path = dir.join("workspace.json");
        let key = [3u8; 32];
        save_maced(&path, &key, b"trusted-roster-bytes").unwrap();
        assert_eq!(load_maced(&path, &key).unwrap().unwrap(), b"trusted-roster-bytes");

        // 잘못된 키(=변조 또는 다른 장치) → MacMismatch.
        assert!(matches!(
            load_maced(&path, &[9u8; 32]),
            Err(StoreError::MacMismatch)
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_is_none() {
        let dir = tmpdir();
        let p: Option<serde_json::Value> = load_json(&dir.join("nope.json")).unwrap();
        assert!(p.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
