//! # sb-crypto
//!
//! shareboard 암호 계층 (PLAN.md §4).
//!
//! - [`identity`] — 장치 신원(P-256 = TLS + 로그 서명), X25519 KEM, device_id, 도메인 분리 서명.
//! - [`hash`] — BLAKE3(keyed/plain), HKDF GK 서브키, workspace_id, ContentId.
//! - [`aead`] — XChaCha20-Poly1305 봉인 프리미티브(24B nonce 전치).
//! - [`groupkey`] — 그룹 키 GK_e, SignalBody/본문 봉인.
//! - [`wrap`] — GK wrap(X25519 ECDH-ES + HKDF + XChaCha20).
//! - [`invite`] — 초대 코드(Crockford Base32 60-bit) + Argon2id 파생 + 봉인 blob.
//! - [`wslog`] — 워크스페이스 로그 해시체인 빌더/검증(규칙 ①~⑦).
//!
//! 이 크레이트는 순수 계산만 담당한다. 키 저장(keychain)은 sb-store, 전송은 sb-net/sb-server.

pub mod aead;
pub mod groupkey;
pub mod hash;
pub mod identity;
pub mod invite;
pub mod wrap;
pub mod wslog;

pub use groupkey::GroupKey;
pub use identity::{
    device_id_from_spki, sign_with, verify_with_spki, Identity, IdentityPublic, DOMAIN_GRANT, DOMAIN_LOG,
    DOMAIN_ROTWRAP,
};
pub use invite::{build_add_from_blob, generate_grant, make_invite, GrantKeypair, InviteSecret};
pub use wrap::{build_signed_rotation, open_rotation, rotation_aad, seal_rotation, verify_rotation};
pub use wslog::{verify_chain, MemberInfo, VerifiedLog};

/// 암호 계층 오류. 세부 사유는 로그에만, 사용자 메시지는 뭉뚱그린다(타이밍/오라클 최소화).
#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("AEAD 봉인 실패")]
    Seal,
    #[error("AEAD 개봉 실패(위조/키 불일치)")]
    Open,
    #[error("키 인코딩/디코딩 실패")]
    KeyEncoding,
    #[error("TLS 인증서 생성 실패")]
    CertGen,
    #[error("KDF 실패")]
    Kdf,
    #[error("CBOR 인코딩 실패")]
    Encode,
    #[error("CBOR 디코딩 실패")]
    Decode,
    #[error("워크스페이스 로그 검증 실패: {0}")]
    Log(String),
}

#[cfg(test)]
mod integration {
    //! 크레이트 간 흐름 스모크 테스트: 조인 후 GK wrap 전달 → unwrap.
    use super::*;
    use sb_proto::e2e::RotationBlob;
    use sb_proto::EpochReason;

    #[test]
    fn end_to_end_gk_delivery() {
        // 창립자 GK 생성.
        let founder = Identity::generate();
        let gk = GroupKey::generate(0);

        // 새 기기 조인(KEM pk 확보 가정).
        let joiner = Identity::generate();

        // 회전자(founder)가 RotationBlob 을 만들어 joiner KEM pk 로 wrap.
        let blob = RotationBlob {
            new_epoch: 0,
            group_key: *gk.expose(),
            reason: EpochReason::Join,
            epoch_entry_hash: [1u8; 32],
            member_set_hash: [2u8; 32],
            from: founder.device_id(),
            sig: vec![], // 실제로는 founder.sign(DOMAIN_ROTWRAP, view)
        };
        let mut blob_bytes = Vec::new();
        ciborium::into_writer(&blob, &mut blob_bytes).unwrap();

        let aad = b"workspace|epoch=0|joiner";
        let wrapped = wrap::wrap(&joiner.kem_public(), aad, &blob_bytes).unwrap();

        // joiner 가 unwrap → GK 확보.
        let opened = wrap::unwrap(&joiner, aad, &wrapped).unwrap();
        let recovered: RotationBlob = ciborium::from_reader(&opened[..]).unwrap();
        assert_eq!(recovered.group_key, *gk.expose());

        // 확보한 GK 로 콘텐츠 복호가 되는지: founder 가 봉인 → joiner 가 개봉.
        let gk2 = GroupKey::from_bytes(recovered.new_epoch, recovered.group_key);
        let sealed = gk.seal_body(b"aad", b"clipboard text").unwrap();
        assert_eq!(gk2.open_body(b"aad", &sealed).unwrap(), b"clipboard text");
    }
}
