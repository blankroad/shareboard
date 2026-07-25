//! LAN 주소 allowlist (§4.5). 서버 bind·accept, 클라이언트 아웃바운드 다이얼 공통.
//!
//! 허용: IPv4 `10/8`·`172.16/12`·`192.168/16`·`169.254/16`·loopback,
//!       IPv6 `fc00::/7`(ULA)·`fe80::/10`(링크로컬)·`::1`.
//! 그 외 전부 거부 — **전역 IPv6 포함**. 바이트를 읽기 전에 거부하는 것이 원칙.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// 이 IP가 사내망(LAN) allowlist에 속하는가? (§4.5)
pub fn is_lan_allowed(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_lan_v4(v4),
        IpAddr::V6(v6) => is_lan_v6(v6),
    }
}

fn is_lan_v4(ip: &Ipv4Addr) -> bool {
    // is_private: 10/8, 172.16/12, 192.168/16
    // is_loopback: 127/8
    // is_link_local: 169.254/16
    ip.is_private() || ip.is_loopback() || ip.is_link_local()
}

fn is_lan_v6(ip: &Ipv6Addr) -> bool {
    if ip.is_loopback() {
        return true; // ::1
    }
    let seg0 = ip.segments()[0];
    // ULA fc00::/7  → 상위 7비트가 1111110
    let is_ula = (seg0 & 0xfe00) == 0xfc00;
    // 링크로컬 fe80::/10 → 상위 10비트가 1111111010
    let is_link_local = (seg0 & 0xffc0) == 0xfe80;
    is_ula || is_link_local
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn allowed(s: &str) -> bool {
        is_lan_allowed(&IpAddr::from_str(s).unwrap())
    }

    #[test]
    fn ipv4_private_allowed() {
        assert!(allowed("10.0.0.1"));
        assert!(allowed("172.16.5.4"));
        assert!(allowed("172.31.255.254"));
        assert!(allowed("192.168.1.10"));
        assert!(allowed("169.254.10.10")); // link-local
        assert!(allowed("127.0.0.1"));
    }

    #[test]
    fn ipv4_public_rejected() {
        assert!(!allowed("8.8.8.8"));
        assert!(!allowed("1.1.1.1"));
        assert!(!allowed("172.32.0.1")); // 12비트 경계 밖
        assert!(!allowed("192.169.0.1"));
        assert!(!allowed("11.0.0.1"));
    }

    #[test]
    fn ipv6_local_allowed() {
        assert!(allowed("::1"));
        assert!(allowed("fc00::1")); // ULA
        assert!(allowed("fd12:3456::1")); // ULA
        assert!(allowed("fe80::1")); // link-local
    }

    #[test]
    fn ipv6_global_rejected() {
        assert!(!allowed("2001:4860:4860::8888")); // 전역 IPv6 반드시 거부
        assert!(!allowed("2606:4700:4700::1111"));
        assert!(!allowed("fb00::1")); // fc00::/7 밖
    }
}
