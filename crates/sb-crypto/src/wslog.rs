//! 워크스페이스 로그 — roster 무결성 서명 해시체인 (§4.3.1, D28).
//!
//! 빌더는 엔트리를 서명해 만들고, [`verify_chain`] 은 검증 규칙 ①~⑦을 적용해
//! **최장 유효 접두**(longest valid prefix)까지의 roster·epoch·head 를 확정한다.
//! 서명은 sig 필드를 비운 "서명 뷰" 바이트에 대해 수행하고, 체인 해시는 sig 포함 저장
//! 바이트열에 대해 계산한다(§4.3.1 — 재직렬화/canonicalization 회피).

use std::collections::{BTreeMap, BTreeSet};

use p256::ecdsa::SigningKey;
use serde::Serialize;

use sb_proto::log::{GrantCert, LogEntry};
use sb_proto::{DeviceId, Epoch, EpochReason};

use crate::hash::{blake3_hash, sha256};
use crate::identity::{sign_with, verify_with_spki, Identity, IdentityPublic, DOMAIN_GRANT, DOMAIN_LOG};
use crate::CryptoError;

const DAY_MS: u64 = 24 * 3600 * 1000;

fn enc<T: Serialize>(v: &T) -> Vec<u8> {
    let mut b = Vec::new();
    ciborium::into_writer(v, &mut b).expect("log entry CBOR encode");
    b
}

/// 저장 바이트열(sig 포함) 그대로 인코딩.
pub fn entry_bytes(e: &LogEntry) -> Vec<u8> {
    enc(e)
}

/// 체인 해시 = BLAKE3(저장 바이트열).
pub fn entry_hash(bytes: &[u8]) -> [u8; 32] {
    blake3_hash(bytes)
}

/// sig 필드를 비운 서명 뷰의 바이트.
fn entry_sign_view(e: &LogEntry) -> Vec<u8> {
    let mut c = e.clone();
    match &mut c {
        LogEntry::Genesis { sig, .. }
        | LogEntry::Add { sig, .. }
        | LogEntry::Remove { sig, .. }
        | LogEntry::Epoch { sig, .. }
        | LogEntry::RotateKem { sig, .. } => sig.clear(),
    }
    enc(&c)
}

fn grant_sign_view(gc: &GrantCert) -> Vec<u8> {
    let mut c = gc.clone();
    c.sig.clear();
    enc(&c)
}

/// 현 roster의 한 멤버.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberInfo {
    pub device_id: DeviceId,
    pub spki_der: Vec<u8>,
    pub kem_pk: [u8; 32],
}

/// `member_set_hash` (규칙 ⑥) — roster 공개키 집합의 결정적 해시.
/// device_id 오름차순(BTreeMap 순회)으로 `did || spki_len(LE32) || spki || kem_pk` 연결 후 BLAKE3.
pub fn member_set_hash(members: &BTreeMap<DeviceId, MemberInfo>) -> [u8; 32] {
    let mut buf = Vec::new();
    for (did, m) in members {
        buf.extend_from_slice(did);
        buf.extend_from_slice(&(m.spki_der.len() as u32).to_le_bytes());
        buf.extend_from_slice(&m.spki_der);
        buf.extend_from_slice(&m.kem_pk);
    }
    blake3_hash(&buf)
}

/// 검증된 로그 상태 (최장 유효 접두 기준).
#[derive(Debug, Clone)]
pub struct VerifiedLog {
    pub workspace_id: [u8; 32],
    pub workspace_name: String,
    pub creator: DeviceId,
    pub head_seq: u64,
    pub head_hash: [u8; 32],
    pub epoch: Epoch,
    pub members: BTreeMap<DeviceId, MemberInfo>,
    /// 검증 통과 엔트리 수(오염 엔트리는 이 지점에서 잘림).
    pub accepted: usize,
}

impl VerifiedLog {
    pub fn is_member(&self, id: &DeviceId) -> bool {
        self.members.contains_key(id)
    }
    pub fn member_set_hash(&self) -> [u8; 32] {
        member_set_hash(&self.members)
    }
}

// ───────────────────────── 빌더 ─────────────────────────

/// Genesis 엔트리 + workspace_id 생성 (§4.3.2).
pub fn build_genesis(id: &Identity, workspace_name: &str, created_at: u64) -> (LogEntry, [u8; 32]) {
    let mut e = LogEntry::Genesis {
        v: 1,
        workspace_name: workspace_name.to_string(),
        creator_spki: id.spki_der().to_vec(),
        creator_kem_pk: id.kem_public(),
        created_at,
        sig: Vec::new(),
    };
    let sig = id.sign(DOMAIN_LOG, &entry_sign_view(&e));
    if let LogEntry::Genesis { sig: s, .. } = &mut e {
        *s = sig;
    }
    let wid = entry_hash(&entry_bytes(&e));
    (e, wid)
}

/// grant 인증서 생성 — sponsor(초대 발급자) 서명 (§4.3.3).
pub fn build_grant_cert(
    sponsor: &Identity,
    grant_pk_der: Vec<u8>,
    expires_at: u64,
    workspace_id: [u8; 32],
) -> GrantCert {
    let mut gc = GrantCert {
        grant_pk: grant_pk_der,
        sponsor: sponsor.device_id(),
        expires_at,
        workspace_id,
        sig: Vec::new(),
    };
    gc.sig = sponsor.sign(DOMAIN_GRANT, &grant_sign_view(&gc));
    gc
}

/// Add 엔트리 — grant_sk(일회용) 서명 (§4.3.4).
pub fn build_add(
    grant_sk: &SigningKey,
    grant_cert: GrantCert,
    subject: &IdentityPublic,
    prev_hash: [u8; 32],
    seq: u64,
    joined_at: u64,
) -> LogEntry {
    let mut e = LogEntry::Add {
        prev_hash,
        seq,
        grant_cert,
        subject_spki: subject.spki_der.clone(),
        subject_kem_pk: subject.kem_pk,
        joined_at,
        sig: Vec::new(),
    };
    let sig = sign_with(grant_sk, DOMAIN_LOG, &entry_sign_view(&e));
    if let LogEntry::Add { sig: s, .. } = &mut e {
        *s = sig;
    }
    e
}

/// Remove 엔트리 — 잔존 멤버 서명 (§4.4).
pub fn build_remove(id: &Identity, prev_hash: [u8; 32], seq: u64, target: DeviceId, ts: u64) -> LogEntry {
    let mut e = LogEntry::Remove {
        prev_hash,
        seq,
        target,
        ts,
        by: id.device_id(),
        sig: Vec::new(),
    };
    let sig = id.sign(DOMAIN_LOG, &entry_sign_view(&e));
    if let LogEntry::Remove { sig: s, .. } = &mut e {
        *s = sig;
    }
    e
}

/// Epoch 엔트리 — 회전자 서명 (§4.4). `member_set_hash` 는 회전 시점 roster로 계산해 넣는다.
#[allow(clippy::too_many_arguments)]
pub fn build_epoch(
    id: &Identity,
    prev_hash: [u8; 32],
    seq: u64,
    epoch_no: Epoch,
    reason: EpochReason,
    member_set_hash: [u8; 32],
    wrapped_bundle_hash: [u8; 32],
    ts: u64,
) -> LogEntry {
    let mut e = LogEntry::Epoch {
        prev_hash,
        seq,
        epoch_no,
        rotator: id.device_id(),
        reason,
        member_set_hash,
        wrapped_bundle_hash,
        ts,
        sig: Vec::new(),
    };
    let sig = id.sign(DOMAIN_LOG, &entry_sign_view(&e));
    if let LogEntry::Epoch { sig: s, .. } = &mut e {
        *s = sig;
    }
    e
}

/// RotateKem 엔트리 — 본인 서명 (§4.1 축 B).
pub fn build_rotate_kem(
    id: &Identity,
    prev_hash: [u8; 32],
    seq: u64,
    new_kem_pk: [u8; 32],
    ts: u64,
) -> LogEntry {
    let mut e = LogEntry::RotateKem {
        prev_hash,
        seq,
        subject: id.device_id(),
        new_kem_pk,
        ts,
        sig: Vec::new(),
    };
    let sig = id.sign(DOMAIN_LOG, &entry_sign_view(&e));
    if let LogEntry::RotateKem { sig: s, .. } = &mut e {
        *s = sig;
    }
    e
}

// ───────────────────────── 검증 ─────────────────────────

/// 체인 전체 검증 (§4.3.1 규칙 ①~⑥; ⑦ wrapped_bundle_hash 는 GK 채택 시점에서 검사).
///
/// `now_ms == 0` 이면 grant 만료 검사(③)를 생략(테스트/오프라인 재검증용).
/// 검증 실패 엔트리를 만나면 그 지점에서 멈추고 마지막 유효 head 기준 상태를 반환한다.
pub fn verify_chain(entries: &[Vec<u8>], now_ms: u64) -> Result<VerifiedLog, CryptoError> {
    let first = entries
        .first()
        .ok_or_else(|| CryptoError::Log("빈 로그".into()))?;
    let g: LogEntry =
        ciborium::from_reader(&first[..]).map_err(|_| CryptoError::Log("Genesis 디코드 실패".into()))?;

    let (workspace_id, workspace_name, creator, mut members) = match &g {
        LogEntry::Genesis {
            workspace_name,
            creator_spki,
            creator_kem_pk,
            sig,
            ..
        } => {
            if !verify_with_spki(creator_spki, DOMAIN_LOG, &entry_sign_view(&g), sig) {
                return Err(CryptoError::Log("Genesis 서명 무효".into()));
            }
            let creator = sha256(creator_spki);
            let mut m = BTreeMap::new();
            m.insert(
                creator,
                MemberInfo {
                    device_id: creator,
                    spki_der: creator_spki.clone(),
                    kem_pk: *creator_kem_pk,
                },
            );
            (entry_hash(first), workspace_name.clone(), creator, m)
        }
        _ => return Err(CryptoError::Log("첫 엔트리가 Genesis 아님".into())),
    };

    let mut head_hash = workspace_id;
    let mut head_seq = 0u64;
    let mut epoch: Epoch = 0;
    let mut accepted = 1usize;
    let mut consumed_grants: BTreeSet<Vec<u8>> = BTreeSet::new();

    'chain: for b in &entries[1..] {
        let entry: LogEntry = match ciborium::from_reader(&b[..]) {
            Ok(e) => e,
            Err(_) => break 'chain,
        };
        // ① 해시 연결 + seq 단조
        match entry.prev_hash() {
            Some(h) if *h == head_hash => {}
            _ => break 'chain,
        }
        if entry.seq() != head_seq + 1 {
            break 'chain;
        }

        match &entry {
            LogEntry::Genesis { .. } => break 'chain, // 중복 Genesis 금지

            LogEntry::Add {
                grant_cert,
                subject_spki,
                subject_kem_pk,
                sig,
                ..
            } => {
                // sponsor 는 현 멤버여야
                let Some(sponsor_spki) = members.get(&grant_cert.sponsor).map(|m| m.spki_der.clone()) else {
                    break 'chain;
                };
                if grant_cert.workspace_id != workspace_id {
                    break 'chain;
                }
                if !verify_with_spki(
                    &sponsor_spki,
                    DOMAIN_GRANT,
                    &grant_sign_view(grant_cert),
                    &grant_cert.sig,
                ) {
                    break 'chain;
                }
                // ③ 만료
                if now_ms > 0 && now_ms > grant_cert.expires_at.saturating_add(DAY_MS) {
                    break 'chain;
                }
                // Add.sig 는 grant_pk 로
                if !verify_with_spki(&grant_cert.grant_pk, DOMAIN_LOG, &entry_sign_view(&entry), sig) {
                    break 'chain;
                }
                // ④ grant 재사용 금지
                if !consumed_grants.insert(grant_cert.grant_pk.clone()) {
                    break 'chain;
                }
                let did = sha256(subject_spki);
                members.insert(
                    did,
                    MemberInfo {
                        device_id: did,
                        spki_der: subject_spki.clone(),
                        kem_pk: *subject_kem_pk,
                    },
                );
            }

            LogEntry::Remove { target, by, sig, .. } => {
                let Some(by_spki) = members.get(by).map(|m| m.spki_der.clone()) else {
                    break 'chain;
                };
                if !verify_with_spki(&by_spki, DOMAIN_LOG, &entry_sign_view(&entry), sig) {
                    break 'chain;
                }
                members.remove(target);
            }

            LogEntry::Epoch {
                epoch_no,
                rotator,
                member_set_hash: msh,
                sig,
                ..
            } => {
                let Some(rot_spki) = members.get(rotator).map(|m| m.spki_der.clone()) else {
                    break 'chain;
                };
                if !verify_with_spki(&rot_spki, DOMAIN_LOG, &entry_sign_view(&entry), sig) {
                    break 'chain;
                }
                if *epoch_no != epoch + 1 {
                    break 'chain; // ⑤
                }
                if *msh != member_set_hash(&members) {
                    break 'chain; // ⑥ — roster 밖 키 wrap 탐지
                }
                epoch = *epoch_no;
            }

            LogEntry::RotateKem {
                subject,
                new_kem_pk,
                sig,
                ..
            } => {
                let Some(sub_spki) = members.get(subject).map(|m| m.spki_der.clone()) else {
                    break 'chain;
                };
                if !verify_with_spki(&sub_spki, DOMAIN_LOG, &entry_sign_view(&entry), sig) {
                    break 'chain;
                }
                if let Some(m) = members.get_mut(subject) {
                    m.kem_pk = *new_kem_pk;
                }
            }
        }

        head_hash = entry_hash(b);
        head_seq = entry.seq();
        accepted += 1;
    }

    Ok(VerifiedLog {
        workspace_id,
        workspace_name,
        creator,
        head_seq,
        head_hash,
        epoch,
        members,
        accepted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use p256::ecdsa::SigningKey;
    use p256::pkcs8::EncodePublicKey;
    use rand_core::OsRng;

    /// 창립자 + grant 발급 → 새 멤버 Add 까지 완성된 체인을 만든다.
    fn build_two_member_chain() -> (Identity, Identity, Vec<Vec<u8>>) {
        let founder = Identity::generate();
        let (genesis, wid) = build_genesis(&founder, "eng-team", 1000);
        let mut chain = vec![entry_bytes(&genesis)];

        // 초대: grant 키쌍
        let grant_sk = SigningKey::random(&mut OsRng);
        let grant_pk_der = p256::ecdsa::VerifyingKey::from(&grant_sk)
            .to_public_key_der()
            .unwrap()
            .as_bytes()
            .to_vec();
        let gc = build_grant_cert(&founder, grant_pk_der, 10_000, wid);

        let joiner = Identity::generate();
        let head = entry_hash(&chain[0]);
        let add = build_add(&grant_sk, gc, &joiner.public(), head, 1, 1100);
        chain.push(entry_bytes(&add));
        (founder, joiner, chain)
    }

    #[test]
    fn genesis_only_verifies() {
        let founder = Identity::generate();
        let (g, wid) = build_genesis(&founder, "team", 1);
        let v = verify_chain(&[entry_bytes(&g)], 0).unwrap();
        assert_eq!(v.workspace_id, wid);
        assert_eq!(v.epoch, 0);
        assert_eq!(v.members.len(), 1);
        assert!(v.is_member(&founder.device_id()));
    }

    #[test]
    fn add_joins_roster() {
        let (founder, joiner, chain) = build_two_member_chain();
        let v = verify_chain(&chain, 0).unwrap();
        assert_eq!(v.accepted, 2);
        assert_eq!(v.members.len(), 2);
        assert!(v.is_member(&founder.device_id()));
        assert!(v.is_member(&joiner.device_id()));
    }

    #[test]
    fn remove_then_epoch_rotates() {
        let (founder, joiner, mut chain) = build_two_member_chain();
        let v = verify_chain(&chain, 0).unwrap();

        // founder 가 joiner 제거 + epoch 회전.
        let remove = build_remove(&founder, v.head_hash, 2, joiner.device_id(), 1200);
        chain.push(entry_bytes(&remove));
        let v2 = verify_chain(&chain, 0).unwrap();
        assert!(!v2.is_member(&joiner.device_id()));

        let msh = v2.member_set_hash();
        let epoch = build_epoch(
            &founder,
            v2.head_hash,
            3,
            1,
            EpochReason::Revoke(joiner.device_id()),
            msh,
            [0u8; 32],
            1300,
        );
        chain.push(entry_bytes(&epoch));
        let v3 = verify_chain(&chain, 0).unwrap();
        assert_eq!(v3.epoch, 1);
        assert_eq!(v3.members.len(), 1);
    }

    #[test]
    fn forged_add_signature_truncates_prefix() {
        let (_f, _j, mut chain) = build_two_member_chain();
        // Add 엔트리의 마지막 바이트를 훼손 → 서명 검증 실패 → 접두가 Genesis까지만.
        let last = chain.last_mut().unwrap();
        let n = last.len();
        last[n - 1] ^= 0xff;
        let v = verify_chain(&chain, 0).unwrap();
        assert_eq!(v.accepted, 1, "오염 엔트리 이후는 잘린다(최장 유효 접두)");
        assert_eq!(v.members.len(), 1);
    }

    #[test]
    fn epoch_with_wrong_member_set_hash_rejected() {
        let (founder, _joiner, mut chain) = build_two_member_chain();
        let v = verify_chain(&chain, 0).unwrap();
        // 잘못된 member_set_hash 로 Epoch 작성 → 규칙 ⑥ 위반 → 거부.
        let epoch = build_epoch(
            &founder,
            v.head_hash,
            2,
            1,
            EpochReason::Manual,
            [0xaa; 32],
            [0u8; 32],
            1300,
        );
        chain.push(entry_bytes(&epoch));
        let v2 = verify_chain(&chain, 0).unwrap();
        assert_eq!(v2.accepted, 2, "잘못된 Epoch 는 채택 안 됨");
        assert_eq!(v2.epoch, 0);
    }

    #[test]
    fn wrong_seq_breaks_chain() {
        let (founder, _j, chain) = build_two_member_chain();
        let v = verify_chain(&chain[..1], 0).unwrap();
        // seq 를 1이 아니라 5로 준 Remove → ① 위반.
        let bad = build_remove(&founder, v.head_hash, 5, [9u8; 32], 1);
        let mut c = chain[..1].to_vec();
        c.push(entry_bytes(&bad));
        let v2 = verify_chain(&c, 0).unwrap();
        assert_eq!(v2.accepted, 1);
    }
}
