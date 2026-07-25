//! 키 저장소 추상화 (§4.2 폴백 사다리).
//!
//! 우선순위: OS keychain(별도 feature) → 패스프레이즈 암호화 파일 → 명시 동의 평문 파일.
//! 여기서는 항상 사용 가능한 `MemoryKeyStore`(테스트)와 `FileKeyStore`(0600 파일)를 제공한다.
//! OS keychain 백엔드는 Tauri 앱 계층에서 `keyring` 크레이트로 주입한다.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::StoreError;

/// 이름 붙은 비밀값 저장소.
pub trait KeyStore: Send + Sync {
    fn get(&self, name: &str) -> Result<Option<Vec<u8>>, StoreError>;
    fn set(&self, name: &str, value: &[u8]) -> Result<(), StoreError>;
    fn delete(&self, name: &str) -> Result<(), StoreError>;
}

/// 인메모리(테스트 전용 — 영속 안 됨).
#[derive(Default)]
pub struct MemoryKeyStore {
    map: Mutex<HashMap<String, Vec<u8>>>,
}

impl MemoryKeyStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl KeyStore for MemoryKeyStore {
    fn get(&self, name: &str) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.map.lock().unwrap().get(name).cloned())
    }
    fn set(&self, name: &str, value: &[u8]) -> Result<(), StoreError> {
        self.map.lock().unwrap().insert(name.to_string(), value.to_vec());
        Ok(())
    }
    fn delete(&self, name: &str) -> Result<(), StoreError> {
        self.map.lock().unwrap().remove(name);
        Ok(())
    }
}

/// 파일 기반(0600). 폴백 사다리 최하단(§4.2). OS keychain 부재 시에만 사용하며
/// 사용 중에는 UI/트레이에 상시 경고를 띄워야 한다.
pub struct FileKeyStore {
    dir: PathBuf,
}

impl FileKeyStore {
    pub fn new(dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir).map_err(StoreError::Io)?;
        set_dir_0700(&dir);
        Ok(Self { dir })
    }

    fn path(&self, name: &str) -> PathBuf {
        // 이름 정규화 — 경로 구분자·상위 참조('.') 등을 전부 '_' 로. 영숫자/-/_ 만 통과.
        let safe: String =
            name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
        self.dir.join(format!("{safe}.key"))
    }
}

impl KeyStore for FileKeyStore {
    fn get(&self, name: &str) -> Result<Option<Vec<u8>>, StoreError> {
        let p = self.path(name);
        match std::fs::read(&p) {
            Ok(v) => Ok(Some(v)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(StoreError::Io(e)),
        }
    }
    fn set(&self, name: &str, value: &[u8]) -> Result<(), StoreError> {
        let p = self.path(name);
        std::fs::write(&p, value).map_err(StoreError::Io)?;
        set_file_0600(&p);
        Ok(())
    }
    fn delete(&self, name: &str) -> Result<(), StoreError> {
        match std::fs::remove_file(self.path(name)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(StoreError::Io(e)),
        }
    }
}

#[cfg(unix)]
fn set_file_0600(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600));
}
#[cfg(not(unix))]
fn set_file_0600(_p: &Path) {}

#[cfg(unix)]
fn set_dir_0700(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700));
}
#[cfg(not(unix))]
fn set_dir_0700(_p: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(store: &dyn KeyStore) {
        assert!(store.get("k").unwrap().is_none());
        store.set("k", b"secret").unwrap();
        assert_eq!(store.get("k").unwrap().unwrap(), b"secret");
        store.delete("k").unwrap();
        assert!(store.get("k").unwrap().is_none());
    }

    #[test]
    fn memory_roundtrip() {
        roundtrip(&MemoryKeyStore::new());
    }

    #[test]
    fn file_roundtrip() {
        let dir = std::env::temp_dir().join(format!("sb-keystore-test-{}", std::process::id()));
        let store = FileKeyStore::new(&dir).unwrap();
        roundtrip(&store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_name_sanitized() {
        let dir = std::env::temp_dir().join(format!("sb-keystore-san-{}", std::process::id()));
        let store = FileKeyStore::new(&dir).unwrap();
        // 경로 조작 시도가 dir 밖으로 나가지 않음 ("../evil" → "___evil.key").
        store.set("../evil", b"x").unwrap();
        assert!(store.get("../evil").unwrap().is_some());
        assert!(dir.join("___evil.key").exists());
        assert!(!dir.parent().unwrap().join("evil.key").exists(), "dir 밖으로 새지 않음");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
