//! Which web hosts Postvak may contact for a mail: remote content the reader asks for, one-click unsubscribe.
use std::net::IpAddr;

/// An `http`/`https` URL whose host is a name, or an address outside loopback and private networks, so a mail cannot
/// make the reader's machine call its router or a local service. A name that resolves to such an address still passes.
pub fn public(url: &str) -> bool {
    let Ok(url) = url::Url::parse(url) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    match host.trim_start_matches('[').trim_end_matches(']').parse() {
        Ok(ip) => routable(ip),
        Err(_) => {
            let name = host.trim_end_matches('.').to_ascii_lowercase();
            name != "localhost" && !name.ends_with(".localhost") && !name.ends_with(".local")
        }
    }
}

fn routable(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            !(ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.octets()[0] == 100 && ip.octets()[1] & 0xc0 == 64)
        }
        IpAddr::V6(ip) => {
            let first = ip.segments()[0];
            !(ip.is_loopback()
                || ip.is_unspecified()
                || first & 0xfe00 == 0xfc00
                || first & 0xffc0 == 0xfe80
                || ip
                    .to_ipv4_mapped()
                    .is_some_and(|v4| !routable(IpAddr::V4(v4))))
        }
    }
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
            "not a url",
        ] {
            assert!(!public(refused), "{refused}");
        }
    }
}
