//! 그룹 키 GK_e (§4.6, D26). epoch별 256-bit 대칭키. 콘텐츠·정렬키 E2E 봉인의 뿌리.

use rand_core::{OsRng, RngCore};
use zeroize::{Zeroize, Zeroizing};

use sb_proto::{ContentId, Epoch};

use crate::aead::{xopen, xseal};
use crate::hash::{content_id, derive_subkey, INFO_BODY, INFO_CID, INFO_SIG};
use crate::CryptoError;

/// 한 epoch의 그룹 키. Drop 시 zeroize.
#[derive(Clone)]
pub struct GroupKey {
    epoch: Epoch,
    key: Zeroizing<[u8; 32]>,
}

impl GroupKey {
    /// 새 랜덤 GK (구 키에서 비파생 — 회전 경계의 PCS, §4.4).
    pub fn generate(epoch: Epoch) -> Self {
        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);
        let gk = Self { epoch, key: Zeroizing::new(key) };
        key.zeroize();
        gk
    }

    /// 원바이트로부터 (wrap unwrap 결과 등). 호출자가 zeroize 책임.
    pub fn from_bytes(epoch: Epoch, key: [u8; 32]) -> Self {
        Self { epoch, key: Zeroizing::new(key) }
    }

    pub fn epoch(&self) -> Epoch {
        self.epoch
    }

    /// 원바이트 노출 (wrap 대상 등). 반드시 즉시 봉인 후 폐기.
    pub fn expose(&self) -> &[u8; 32] {
        &self.key
    }

    fn k_sig(&self) -> [u8; 32] {
        derive_subkey(&self.key, INFO_SIG)
    }
    fn k_body(&self) -> [u8; 32] {
        derive_subkey(&self.key, INFO_BODY)
    }
    /// ContentId 계산용 keyed BLAKE3 키.
    pub fn k_cid(&self) -> [u8; 32] {
        derive_subkey(&self.key, INFO_CID)
    }

    /// SignalBody(정렬키·kind·inline) 봉인. AAD = 평문 헤더 캐노니컬 CBOR ‖ origin (§4.6).
    pub fn seal_signal(&self, aad: &[u8], body_plain: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let mut k = self.k_sig();
        let r = xseal(&k, aad, body_plain);
        k.zeroize();
        r
    }
    pub fn open_signal(&self, aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let mut k = self.k_sig();
        let r = xopen(&k, aad, sealed);
        k.zeroize();
        r
    }

    /// 콘텐츠 본문 봉인 (통짜 1회 → 청크 분할은 상위 계층).
    pub fn seal_body(&self, aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let mut k = self.k_body();
        let r = xseal(&k, aad, plaintext);
        k.zeroize();
        r
    }
    pub fn open_body(&self, aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let mut k = self.k_body();
        let r = xopen(&k, aad, sealed);
        k.zeroize();
        r
    }

    /// 이 GK/epoch에서의 ContentId.
    pub fn content_id(&self, plaintext: &[u8]) -> ContentId {
        let mut k = self.k_cid();
        let id = content_id(&k, plaintext);
        k.zeroize();
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_seal_open_roundtrip() {
        let gk = GroupKey::generate(1);
        let sealed = gk.seal_signal(b"hdr-aad", b"signal-body").unwrap();
        assert_eq!(gk.open_signal(b"hdr-aad", &sealed).unwrap(), b"signal-body");
        // 헤더 변조 = AEAD 실패 (서버 재라우팅 방어).
        assert!(gk.open_signal(b"tampered", &sealed).is_err());
    }

    #[test]
    fn different_epoch_keys_incompatible() {
        let a = GroupKey::generate(1);
        let b = GroupKey::generate(2);
        let sealed = a.seal_body(b"", b"x").unwrap();
        assert!(b.open_body(b"", &sealed).is_err(), "다른 GK로는 복호 불가");
    }

    #[test]
    fn content_id_changes_with_epoch_key() {
        let a = GroupKey::from_bytes(1, [1u8; 32]);
        let b = GroupKey::from_bytes(2, [2u8; 32]);
        // k_cid가 GK에 의존 → epoch 회전 시 dedup 끊김(§4.6).
        assert_ne!(a.content_id(b"same"), b.content_id(b"same"));
    }
}
