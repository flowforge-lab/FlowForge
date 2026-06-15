//! SSRF guard for outbound HTTP tools (`web_fetch`). It rejects requests aimed at
//! internal, loopback, link-local, CGNAT, and cloud-metadata addresses *before*
//! any connection — and the fetch loop re-applies it on every redirect hop, which
//! is the classic SSRF-via-redirect bypass.
//!
//! Known limitation (documented, acceptable for v1): DNS rebinding. We resolve and
//! check, but `reqwest` re-resolves when it connects, so a name that flips from a
//! public to a private answer between our check and the connect is not caught.
//! Pinning the connection to the checked IP is a follow-up.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use url::{Host, Url};

/// The outbound address policy. Production is [`SsrfPolicy::strict`]; tests relax
/// only loopback (so a local mock server is reachable) while every other private,
/// link-local, and metadata range stays blocked — keeping the redirect-to-internal
/// test meaningful.
#[derive(Debug, Clone, Copy)]
pub struct SsrfPolicy {
    /// When true, `127.0.0.0/8` and `::1` are permitted. Never enabled in prod.
    pub allow_loopback: bool,
}

impl SsrfPolicy {
    /// Block every non-public destination, including loopback. The only policy the
    /// shipped `web_fetch` tool is constructed with.
    pub fn strict() -> Self {
        Self {
            allow_loopback: false,
        }
    }

    /// Parse and statically validate a user-supplied URL: `http`/`https` only, a
    /// host must be present, and a *literal-IP* host is checked immediately. Named
    /// hosts are checked after DNS via [`SsrfPolicy::check_ip`].
    pub fn check_url(&self, raw: &str) -> Result<Url, String> {
        let url = Url::parse(raw).map_err(|e| format!("invalid URL `{raw}`: {e}"))?;
        match url.scheme() {
            "http" | "https" => {}
            other => {
                return Err(format!(
                    "unsupported URL scheme `{other}` (only http/https allowed)"
                ))
            }
        }
        match url.host() {
            // Literal IP hosts are checked immediately (no DNS).
            Some(Host::Ipv4(v4)) => self.check_ip(IpAddr::V4(v4))?,
            Some(Host::Ipv6(v6)) => self.check_ip(IpAddr::V6(v6))?,
            // A named host is validated after DNS (see `check_host`).
            Some(Host::Domain(d)) if !d.is_empty() => {}
            _ => return Err(format!("URL has no host: `{raw}`")),
        }
        Ok(url)
    }

    /// Reject an IP that falls in a blocked range. Public so the fetch loop can
    /// re-check every address a host resolves to and every redirect target.
    pub fn check_ip(&self, ip: IpAddr) -> Result<(), String> {
        let blocked = match ip {
            IpAddr::V4(v4) => self.is_blocked_v4(v4),
            IpAddr::V6(v6) => self.is_blocked_v6(v6),
        };
        if blocked {
            Err(format!("blocked address (SSRF guard): {ip}"))
        } else {
            Ok(())
        }
    }

    /// Resolve `host:port` and ensure *every* answer is allowed. Resolving all and
    /// rejecting if any is blocked closes round-robin DNS bypasses.
    pub async fn resolve_and_check(&self, host: &str, port: u16) -> Result<(), String> {
        let addrs: Vec<IpAddr> = tokio::net::lookup_host((host, port))
            .await
            .map_err(|e| format!("DNS resolution failed for `{host}`: {e}"))?
            .map(|sa| sa.ip())
            .collect();
        if addrs.is_empty() {
            return Err(format!("`{host}` resolved to no addresses"));
        }
        for ip in addrs {
            self.check_ip(ip)?;
        }
        Ok(())
    }

    /// Validate the host of a (possibly post-redirect) URL: a literal IP is checked
    /// directly; a named host is resolved and every answer checked.
    pub async fn check_host(&self, url: &Url) -> Result<(), String> {
        match url.host() {
            Some(Host::Ipv4(v4)) => self.check_ip(IpAddr::V4(v4)),
            Some(Host::Ipv6(v6)) => self.check_ip(IpAddr::V6(v6)),
            Some(Host::Domain(d)) if !d.is_empty() => {
                let port = url.port_or_known_default().unwrap_or(80);
                self.resolve_and_check(d, port).await
            }
            _ => Err("URL has no host".to_string()),
        }
    }

    fn is_blocked_v4(&self, ip: Ipv4Addr) -> bool {
        if ip.is_loopback() {
            return !self.allow_loopback; // 127.0.0.0/8
        }
        ip.is_private()            // 10/8, 172.16/12, 192.168/16
            || ip.is_link_local()  // 169.254.0.0/16 — covers 169.254.169.254 metadata
            || ip.is_broadcast()
            || ip.is_unspecified() // 0.0.0.0
            || ip.is_documentation()
            || is_cgnat(ip)        // 100.64.0.0/10
            || ip.octets()[0] == 0 // 0.0.0.0/8 "this network"
    }

    fn is_blocked_v6(&self, ip: Ipv6Addr) -> bool {
        if ip.is_loopback() {
            return !self.allow_loopback; // ::1
        }
        if ip.is_unspecified() {
            return true; // ::
        }
        // IPv4-mapped (::ffff:a.b.c.d): unwrap and re-check the embedded v4 so a
        // mapped 127.0.0.1 / 169.254.169.254 can't sneak through.
        if let Some(v4) = ip.to_ipv4_mapped() {
            return self.is_blocked_v4(v4);
        }
        let seg = ip.segments();
        let unique_local = (seg[0] & 0xfe00) == 0xfc00; // fc00::/7
        let link_local = (seg[0] & 0xffc0) == 0xfe80; // fe80::/10
        unique_local || link_local
    }
}

/// Carrier-grade NAT shared address space, `100.64.0.0/10` (RFC 6598).
fn is_cgnat(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 100 && (64..=127).contains(&o[1])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strict() -> SsrfPolicy {
        SsrfPolicy::strict()
    }

    fn check(raw: &str) -> Result<Url, String> {
        strict().check_url(raw)
    }

    #[test]
    fn allows_public_http_and_https() {
        assert!(check("http://example.com/").is_ok());
        assert!(check("https://example.com/path?q=1").is_ok());
        // A public literal IP is fine.
        assert!(check("https://93.184.216.34/").is_ok());
    }

    #[test]
    fn rejects_non_http_schemes() {
        for raw in [
            "file:///etc/passwd",
            "ftp://example.com/",
            "gopher://example.com/",
            "data:text/plain,hi",
        ] {
            assert!(check(raw).is_err(), "{raw} should be rejected");
        }
    }

    #[test]
    fn rejects_loopback() {
        for raw in ["http://127.0.0.1/", "http://127.5.5.5/", "http://[::1]/"] {
            assert!(check(raw).is_err(), "{raw} should be rejected");
        }
    }

    #[test]
    fn rejects_private_ranges() {
        for raw in [
            "http://10.0.0.1/",
            "http://172.16.0.1/",
            "http://172.31.255.255/",
            "http://192.168.1.1/",
        ] {
            assert!(check(raw).is_err(), "{raw} should be rejected");
        }
    }

    #[test]
    fn rejects_link_local_and_cloud_metadata() {
        assert!(check("http://169.254.0.1/").is_err());
        assert!(
            check("http://169.254.169.254/latest/meta-data/").is_err(),
            "cloud metadata endpoint must be blocked"
        );
        assert!(check("http://[fe80::1]/").is_err());
    }

    #[test]
    fn rejects_cgnat_unspecified_and_this_network() {
        assert!(check("http://100.64.0.1/").is_err()); // CGNAT
        assert!(check("http://0.0.0.0/").is_err()); // unspecified
        assert!(check("http://0.1.2.3/").is_err()); // 0.0.0.0/8
    }

    #[test]
    fn rejects_unique_local_ipv6() {
        assert!(check("http://[fd00::1]/").is_err()); // fc00::/7
    }

    #[test]
    fn rejects_ipv4_mapped_internal_ipv6() {
        // ::ffff:127.0.0.1 and ::ffff:169.254.169.254 must unwrap and be blocked.
        assert!(strict()
            .check_ip("::ffff:127.0.0.1".parse().unwrap())
            .is_err());
        assert!(strict()
            .check_ip("::ffff:169.254.169.254".parse().unwrap())
            .is_err());
    }

    #[test]
    fn loopback_relaxation_only_touches_loopback() {
        let relaxed = SsrfPolicy {
            allow_loopback: true,
        };
        // Loopback is now allowed...
        assert!(relaxed.check_ip("127.0.0.1".parse().unwrap()).is_ok());
        assert!(relaxed.check_ip("::1".parse().unwrap()).is_ok());
        // ...but metadata / private / link-local stay blocked.
        assert!(relaxed
            .check_ip("169.254.169.254".parse().unwrap())
            .is_err());
        assert!(relaxed.check_ip("10.0.0.1".parse().unwrap()).is_err());
        assert!(relaxed.check_ip("192.168.1.1".parse().unwrap()).is_err());
    }

    #[test]
    fn rejects_missing_host() {
        // `http://` has an empty authority -> the url crate errors at parse.
        assert!(check("http://").is_err());
        // A bare scheme with no authority is also rejected.
        assert!(check("https://").is_err());
    }
}
