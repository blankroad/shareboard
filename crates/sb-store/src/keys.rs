//! 키 매니저 (§4.2). 앱이 쓰는 명명 키를 get-or-create 로 관리한다.
//!
//! - `identity` (P-256 signing pkcs8 + X25519 kem) — 장치 신원.
//! - `history` (256-bit) — 히스토리 필드 암호화.
//! - `dedup` (256-bit) — keyed BLAKE3 dedup(D21).
//! - `ws-mac` (256-bit) — workspace.json 무결성 MAC(D22).

use rand_core::{OsRng, RngCore};
use zeroize::Zeroizing;

use sb_crypto::Identity;

use crate::keystore::KeyStore;
use crate::StoreError;

pub const K_IDENTITY_SIGNING: &str = "identity.signing";
pub const K_IDENTITY_KEM: &str = "identity.kem";
pub const K_HISTORY: &str = "history.key";
pub const K_DEDUP: &str = "dedup.key";
pub const K_WS_MAC: &str = "ws-mac.key";

pub struct KeyManager<'a> {
    store: &'a dyn KeyStore,
}

impl<'a> KeyManager<'a> {
    pub fn new(store: &'a dyn KeyStore) -> Self {
        Self { store }
    }

    /// 32바이트 키 get-or-create.
    pub fn get_or_create_32(&self, name: &str) -> Result<Zeroizing<[u8; 32]>, StoreError> {
        if let Some(v) = self.store.get(name)? {
            if v.len() == 32 {
                let mut k = [0u8; 32];
                k.copy_from_slice(&v);
                return Ok(Zeroizing::new(k));
            }
            return Err(StoreError::Corrupt(format!("{name} 길이 이상")));
        }
        let mut k = [0u8; 32];
        OsRng.fill_bytes(&mut k);
        self.store.set(name, &k)?;
        Ok(Zeroizing::new(k))
    }

    pub fn history_key(&self) -> Result<Zeroizing<[u8; 32]>, StoreError> {
        self.get_or_create_32(K_HISTORY)
    }
    pub fn dedup_key(&self) -> Result<Zeroizing<[u8; 32]>, StoreError> {
        self.get_or_create_32(K_DEDUP)
    }
    /// ws-mac 키는 히스토리 설정과 무관하게 항상 provisioning(D22).
    pub fn ws_mac_key(&self) -> Result<Zeroizing<[u8; 32]>, StoreError> {
        self.get_or_create_32(K_WS_MAC)
    }

    /// 장치 신원 get-or-create.
    pub fn load_or_create_identity(&self) -> Result<Identity, StoreError> {
        match (self.store.get(K_IDENTITY_SIGNING)?, self.store.get(K_IDENTITY_KEM)?) {
            (Some(signing), Some(kem)) if kem.len() == 32 => {
                let mut kem_arr = [0u8; 32];
                kem_arr.copy_from_slice(&kem);
                Identity::from_parts(&signing, &kem_arr).map_err(|e| StoreError::Crypto(e.to_string()))
            }
            _ => {
                let id = Identity::generate();
                let signing =
                    id.signing_pkcs8_der().map_err(|e| StoreError::Crypto(e.to_string()))?;
                self.store.set(K_IDENTITY_SIGNING, &signing)?;
                self.store.set(K_IDENTITY_KEM, &id.kem_secret_bytes())?;
                Ok(id)
            }
        }
    }

    /// 히스토리 crypto-erase (§4.6) — 히스토리·dedup 키 파기·재생성.
    pub fn crypto_erase_history(&self) -> Result<(), StoreError> {
        self.store.delete(K_HISTORY)?;
        self.store.delete(K_DEDUP)?;
        // 다음 get_or_create 에서 새 키 생성 → 기존 암호문 복호 불가.
        self.history_key()?;
        self.dedup_key()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keystore::MemoryKeyStore;

    #[test]
    fn keys_are_stable_and_created_once() {
        let store = MemoryKeyStore::new();
        let km = KeyManager::new(&store);
        let a = km.history_key().unwrap();
        let b = km.history_key().unwrap();
        assert_eq!(*a, *b, "같은 키 재사용");
    }

    #[test]
    fn identity_persists() {
        let store = MemoryKeyStore::new();
        let km = KeyManager::new(&store);
        let id1 = km.load_or_create_identity().unwrap();
        let id2 = km.load_or_create_identity().unwrap();
        assert_eq!(id1.device_id(), id2.device_id());
    }

    #[test]
    fn crypto_erase_changes_key() {
        let store = MemoryKeyStore::new();
        let km = KeyManager::new(&store);
        let before = *km.history_key().unwrap();
        km.crypto_erase_history().unwrap();
        let after = *km.history_key().unwrap();
        assert_ne!(before, after, "crypto-erase 후 새 키");
    }
}
