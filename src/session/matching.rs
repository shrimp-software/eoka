//! RFC 6265 request-URL parsing and cookie matching.

use super::SessionCookie;

/// The parts of a request URL that select cookies.
pub(super) struct RequestTarget<'a> {
    pub(super) secure: bool,
    pub(super) host: String,
    pub(super) path: &'a str,
}

impl<'a> RequestTarget<'a> {
    /// Fails closed: anything ambiguous is rejected, because a mistaken host here
    /// would send credentials to the wrong site.
    pub(super) fn parse(url: &'a str) -> Option<Self> {
        let (scheme, rest) = url.split_once("://")?;
        let secure = match scheme.to_ascii_lowercase().as_str() {
            "https" | "wss" => true,
            "http" | "ws" => false,
            _ => return None,
        };
        let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(authority_end);
        // Browsers treat `\\` as `/`, so `https://evil.test\\@bank.test` would
        // otherwise parse differently here than in a real client.
        if authority.contains('\\') {
            return None;
        }
        let host_port = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        let host = if let Some(bracketed) = host_port.strip_prefix('[') {
            let (ip, port) = bracketed.split_once(']')?;
            valid_port(port.strip_prefix(':').unwrap_or(port))?;
            ip.parse::<std::net::Ipv6Addr>().ok()?;
            ip
        } else {
            let (host, port) = host_port.split_once(':').unwrap_or((host_port, ""));
            valid_port(port)?;
            host
        };
        let host = host.strip_suffix('.').unwrap_or(host);
        if host.is_empty()
            || !host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':'))
        {
            return None;
        }
        let path = &tail[..tail.find(['?', '#']).unwrap_or(tail.len())];
        Some(Self {
            secure,
            host: host.to_ascii_lowercase(),
            path: if path.is_empty() { "/" } else { path },
        })
    }
}

fn valid_port(port: &str) -> Option<()> {
    (port.is_empty() || port.parse::<u16>().is_ok()).then_some(())
}

/// RFC 6265 domain match. A leading dot marks a domain cookie, which also covers
/// subdomains; anything else is host-only. IP addresses have no subdomains.
pub(super) fn domain_matches(host: &str, cookie_domain: &str) -> bool {
    let host = host.to_ascii_lowercase();
    let cookie_domain = cookie_domain.to_ascii_lowercase();
    match cookie_domain.strip_prefix('.') {
        Some(base) if !base.is_empty() => {
            host == base
                || (host.ends_with(&format!(".{base}"))
                    && host.parse::<std::net::IpAddr>().is_err())
        }
        Some(_) => false,
        None => host == cookie_domain,
    }
}

/// RFC 6265 §5.1.4 path match: `/a` covers `/a` and `/a/b`, but not `/ab`.
pub(super) fn path_matches(request_path: &str, cookie_path: &str) -> bool {
    request_path == cookie_path
        || (request_path.starts_with(cookie_path)
            && (cookie_path.ends_with('/')
                || request_path.as_bytes().get(cookie_path.len()) == Some(&b'/')))
}

/// Whether a cookie can appear in a header without changing the header's structure.
pub(super) fn header_safe(cookie: &SessionCookie) -> bool {
    let unsafe_byte = |b: u8| b.is_ascii_control() || b == b';';
    !cookie.name.is_empty()
        && !cookie.name.bytes().any(|b| unsafe_byte(b) || b == b'=')
        && !cookie.value.bytes().any(unsafe_byte)
}
