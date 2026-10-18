//! Which web hosts Postvak may contact for a mail: remote content the reader asks for, one-click unsubscribe.
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use ureq::config::Config;
use ureq::http::Uri;
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};

/// An `http`/`https` URL whose host may be public: an address outside loopback and private networks, or a name with
/// a dot outside the local-only suffixes. A name can still resolve inward, so connect only through `public_agent`.
pub fn public(url: &str) -> bool {
    let Ok(url) = url::Url::parse(url) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    match url.host() {
        Some(url::Host::Ipv4(ip)) => routable(IpAddr::V4(ip)),
        Some(url::Host::Ipv6(ip)) => routable(IpAddr::V6(ip)),
        Some(url::Host::Domain(name)) => public_name(name),
        None => false,
    }
}

/// A dotless name such as `router` is looked up on the local network, and these suffixes never leave it.
fn public_name(name: &str) -> bool {
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    const LOCAL: [&str; 4] = ["home.arpa", "internal", "local", "localhost"];
    name.contains('.')
        && !LOCAL
            .iter()
            .any(|suffix| name == *suffix || name.ends_with(&format!(".{suffix}")))
}

/// An HTTP agent that connects only to routable addresses: it checks every address a name resolves to and connects to
/// the one it checked, so neither a name pointing inward nor DNS rebinding reaches the reader's network. No proxy, as
/// a proxy would resolve the name itself.
pub fn public_agent(
    config: ureq::config::ConfigBuilder<ureq::typestate::AgentScope>,
) -> ureq::Agent {
    ureq::Agent::with_parts(
        config.proxy(None).build(),
        DefaultConnector::default(),
        PublicResolver(DefaultResolver::default()),
    )
}

#[derive(Debug)]
struct PublicResolver(DefaultResolver);

impl Resolver for PublicResolver {
    fn resolve(
        &self,
        uri: &Uri,
        config: &Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let resolved = self.0.resolve(uri, config, timeout)?;
        // Any inward address refuses the whole name: a rebinding name may list a public one first.
        if resolved.iter().any(|addr| !routable(addr.ip())) {
            return Err(ureq::Error::HostNotFound);
        }
        Ok(resolved)
    }
}

pub fn routable(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => routable_v4(ip),
        IpAddr::V6(ip) => routable_v6(ip),
    }
}

fn routable_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || a == 0
        || a >= 240
        || a == 100 && b & 0xc0 == 64
        || a == 192 && b == 0 && c == 0
        || a == 198 && b & 0xfe == 18)
}

fn routable_v6(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    let first = segments[0];
    let embedded = |high: u16, low: u16| {
        Ipv4Addr::new((high >> 8) as u8, high as u8, (low >> 8) as u8, low as u8)
    };
    // IPv4-mapped (::ffff:a.b.c.d) and IPv4-compatible (::a.b.c.d) forms reach the IPv4 address they carry.
    if let Some(v4) = ip.to_ipv4() {
        return !ip.is_loopback() && !ip.is_unspecified() && routable_v4(v4);
    }
    // NAT64 (64:ff9b::/96) carries it in the last 32 bits, 6to4 (2002::/16) in bits 16 to 48.
    if segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        return routable_v4(embedded(segments[6], segments[7]));
    }
    if first == 0x2002 {
        return routable_v4(embedded(segments[1], segments[2]));
    }
    !(ip.is_multicast()
        || first & 0xfe00 == 0xfc00
        || first & 0xffc0 == 0xfe80
        || first & 0xffc0 == 0xfec0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_public_web_hosts_are_fetched() {
        for allowed in [
            "https://example.com/a.png",
            "http://93.184.216.34/a.png",
            "https://[2606:2800:220:1::]/a.png",
        ] {
            assert!(public(allowed), "{allowed}");
        }
        for refused in [
            "file:///etc/passwd",
            "ftp://example.com/a",
            "http://localhost/a",
            "http://printer.local/a",
            "http://127.0.0.1/a",
            "http://10.0.0.1/a",
            "http://172.16.0.1/a",
            "http://192.168.0.1/a",
            "http://169.254.169.254/latest/meta-data",
            "http://100.64.0.1/a",
            "http://0.0.0.0/a",
            "http://[::1]/a",
            "http://[fd00::1]/a",
            "http://[fe80::1]/a",
            "http://[::ffff:192.168.0.1]/a",
            "http://router/a",
            "http://router./a",
            "http://nas.home.arpa/a",
            "http://db.internal/a",
            "http://0.1.2.3/a",
            "http://224.0.0.1/a",
            "http://192.0.0.8/a",
            "http://198.18.0.1/a",
            "http://[::192.168.0.1]/a",
            "http://[64:ff9b::c0a8:101]/a",
            "http://[2002:c0a8:101::1]/a",
            "http://[fec0::1]/a",
            "http://[ff02::1]/a",
            "not a url",
        ] {
            assert!(!public(refused), "{refused}");
        }
    }

    #[test]
    fn the_public_agent_refuses_a_name_that_resolves_inward() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(1) {
                let mut stream = stream.unwrap();
                let _ = stream.read(&mut [0; 1024]);
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                );
            }
        });
        let url = format!("http://localhost:{port}/");
        let agent = public_agent(ureq::Agent::config_builder());
        assert!(matches!(
            agent.get(&url).call(),
            Err(ureq::Error::HostNotFound)
        ));
        // The same request through a plain agent is answered, so the refusal is the resolver's.
        assert_eq!(ureq::get(&url).call().unwrap().status(), 200);
    }
}
