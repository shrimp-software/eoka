//! Session export for browser-to-HTTP-client handoff
//!
//! After authenticating in the browser, export the session to make
//! direct HTTP requests.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::Path;

use crate::error::Result;
use crate::Redacted;

/// Browser cookie (simplified, serializable). `Debug` omits the value.
#[derive(Clone, Serialize, Deserialize)]
pub struct SessionCookie {
    /// Cookie name.
    pub name: String,
    /// Cookie value.
    pub value: String,
    /// Domain the cookie applies to.
    pub domain: String,
    /// Path the cookie applies to.
    pub path: String,
    /// Whether the cookie is sent only over HTTPS.
    pub secure: bool,
    /// Whether the cookie is inaccessible to JavaScript.
    pub http_only: bool,
    /// SameSite policy ("Strict", "Lax", "None"), if set.
    pub same_site: Option<String>,
    /// Expiry as a Unix timestamp in seconds, or `None` for a session cookie.
    pub expires: Option<f64>,
}

/// Exported browser session. `Debug` omits cookie values, header values and URL queries.
#[derive(Clone, Serialize, Deserialize)]
pub struct BrowserSession {
    /// All cookies from the browser
    pub cookies: Vec<SessionCookie>,
    /// User agent string
    pub user_agent: String,
    /// Current URL
    pub url: String,
    /// Additional headers
    #[serde(default)]
    pub extra_headers: HashMap<String, String>,
}

/// Complete browser state for restoring an authenticated browser session.
/// `Debug` omits cookie values, storage contents and URL queries.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserState {
    /// Every cookie in the browser context, for all sites, including HttpOnly
    /// cookies. Treat a saved state as a credential for all of those sites.
    pub cookies: Vec<SessionCookie>,
    /// Storage associated with the saved URL's origin.
    #[serde(default)]
    pub local_storage: HashMap<String, String>,
    /// Per-tab storage associated with the saved URL's origin.
    #[serde(default)]
    pub session_storage: HashMap<String, String>,
    /// User agent active when the state was captured.
    pub user_agent: String,
    /// URL whose origin owns the captured storage.
    pub url: String,
}

impl fmt::Debug for SessionCookie {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionCookie")
            .field("name", &self.name)
            .field("value", &Redacted)
            .field("domain", &self.domain)
            .field("path", &self.path)
            .field("secure", &self.secure)
            .field("http_only", &self.http_only)
            .field("same_site", &self.same_site)
            .field("expires", &self.expires)
            .finish()
    }
}

impl fmt::Debug for BrowserSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrowserSession")
            .field("cookies", &self.cookies)
            .field("user_agent", &self.user_agent)
            .field("url", &without_query(&self.url))
            .field("extra_header_names", &self.extra_headers.keys())
            .finish()
    }
}

impl fmt::Debug for BrowserState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrowserState")
            .field("cookies", &self.cookies)
            .field("local_storage_entries", &self.local_storage.len())
            .field("session_storage_entries", &self.session_storage.len())
            .field("user_agent", &self.user_agent)
            .field("url", &without_query(&self.url))
            .finish()
    }
}

/// Query strings and fragments often carry tokens.
fn without_query(url: &str) -> &str {
    &url[..url.find(['?', '#']).unwrap_or(url.len())]
}

impl BrowserSession {
    /// Create a new session from cookies
    pub fn new(cookies: Vec<SessionCookie>, user_agent: String, url: String) -> Self {
        Self {
            cookies,
            user_agent,
            url,
            extra_headers: HashMap::new(),
        }
    }

    /// Save session to a JSON file. The file holds live credentials.
    ///
    /// On Unix it is readable only by its owner (`0600`), including when it
    /// replaces a file with broader permissions. The write is atomic, so a failure
    /// leaves any existing file intact. Elsewhere the directory's ACLs apply.
    pub fn save(&self, path: &str) -> Result<()> {
        write_private(
            Path::new(path),
            serde_json::to_string_pretty(self)?.as_bytes(),
        )?;
        Ok(())
    }

    /// Load session from JSON file
    pub fn load(path: &str) -> Result<Self> {
        let json = std::fs::read_to_string(path)?;
        let session = serde_json::from_str(&json)?;
        Ok(session)
    }

    /// Get cookies for a specific domain (RFC 6265 domain matching)
    pub fn cookies_for_domain(&self, domain: &str) -> Vec<&SessionCookie> {
        self.cookies
            .iter()
            .filter(|c| domain_matches(domain, &c.domain))
            .collect()
    }

    /// Cookies a browser would send to `url`, longest path first (RFC 6265 §5.4):
    /// matching domain and path, not expired, and `Secure` only over https/wss.
    ///
    /// Returns nothing for a URL that cannot be parsed safely, including other
    /// schemes and non-ASCII hosts. SameSite is not applied, since a direct HTTP
    /// client has no initiating site.
    pub fn cookies_for_url(&self, url: &str) -> Vec<&SessionCookie> {
        let Some(target) = RequestTarget::parse(url) else {
            return Vec::new();
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |elapsed| elapsed.as_secs_f64());
        let mut cookies: Vec<_> = self
            .cookies
            .iter()
            .filter(|c| {
                domain_matches(&target.host, &c.domain)
                    && path_matches(target.path, &c.path)
                    && (target.secure || !c.secure)
                    && c.expires
                        .is_none_or(|expires| expires < 0.0 || expires > now)
            })
            .collect();
        cookies.sort_by_key(|c| std::cmp::Reverse(c.path.len()));
        cookies
    }

    /// `Cookie` header value for a request to `url`, or empty if no cookie applies.
    ///
    /// Only cookies from [`cookies_for_url`](Self::cookies_for_url) are included, so
    /// credentials are not sent to other sites. Values are byte-for-byte what the
    /// browser would send. A cookie that cannot be represented safely in a header
    /// (an empty name, or a control character, `;`, or `=` in its name) is dropped
    /// rather than altered.
    pub fn cookie_header_for(&self, url: &str) -> String {
        self.cookies_for_url(url)
            .into_iter()
            .filter(|c| header_safe(c))
            .map(|c| format!("{}={}", c.name, c.value))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// The parts of a request URL that select cookies.
struct RequestTarget<'a> {
    secure: bool,
    host: String,
    path: &'a str,
}

impl<'a> RequestTarget<'a> {
    /// Fails closed: anything ambiguous selects no cookies, because a mistaken
    /// host here would send credentials to the wrong site.
    fn parse(url: &'a str) -> Option<Self> {
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
fn domain_matches(host: &str, cookie_domain: &str) -> bool {
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
fn path_matches(request_path: &str, cookie_path: &str) -> bool {
    request_path == cookie_path
        || (request_path.starts_with(cookie_path)
            && (cookie_path.ends_with('/')
                || request_path.as_bytes().get(cookie_path.len()) == Some(&b'/')))
}

/// Whether a cookie can appear in a header without changing the header's structure.
fn header_safe(cookie: &SessionCookie) -> bool {
    let unsafe_byte = |b: u8| b.is_ascii_control() || b == b';';
    !cookie.name.is_empty()
        && !cookie.name.bytes().any(|b| unsafe_byte(b) || b == b'=')
        && !cookie.value.bytes().any(unsafe_byte)
}

fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("Session path has no file name"))?
        .to_string_lossy();
    let temporary = path.with_file_name(format!(".{name}.{:016x}.tmp", fastrand::u64(..)));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = options
        .open(&temporary)
        .and_then(|mut file| {
            file.write_all(contents)?;
            file.sync_all()
        })
        .and_then(|()| std::fs::rename(&temporary, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_browser_state_round_trips_storage_and_http_only_cookie() {
        let state = BrowserState {
            cookies: vec![SessionCookie {
                name: "session".to_string(),
                value: "secret".to_string(),
                domain: ".example.com".to_string(),
                path: "/".to_string(),
                secure: true,
                http_only: true,
                same_site: Some("Lax".to_string()),
                expires: None,
            }],
            local_storage: HashMap::from([("token".to_string(), "value".to_string())]),
            session_storage: HashMap::from([("tab".to_string(), "value".to_string())]),
            user_agent: "Mozilla/5.0".to_string(),
            url: "https://example.com/account".to_string(),
        };

        let json = serde_json::to_string(&state).unwrap();
        let restored: BrowserState = serde_json::from_str(&json).unwrap();

        assert!(restored.cookies[0].http_only);
        assert_eq!(restored.local_storage["token"], "value");
        assert_eq!(restored.session_storage["tab"], "value");
        assert_eq!(restored.url, "https://example.com/account");
    }

    #[test]
    fn test_cookies_for_domain() {
        let session = BrowserSession {
            cookies: vec![
                SessionCookie {
                    name: "site1".to_string(),
                    value: "v1".to_string(),
                    domain: "example.com".to_string(),
                    path: "/".to_string(),
                    secure: false,
                    http_only: false,
                    same_site: None,
                    expires: None,
                },
                SessionCookie {
                    name: "site2".to_string(),
                    value: "v2".to_string(),
                    domain: "other.com".to_string(),
                    path: "/".to_string(),
                    secure: false,
                    http_only: false,
                    same_site: None,
                    expires: None,
                },
                SessionCookie {
                    name: "sub".to_string(),
                    value: "v3".to_string(),
                    domain: ".example.com".to_string(),
                    path: "/".to_string(),
                    secure: false,
                    http_only: false,
                    same_site: None,
                    expires: None,
                },
            ],
            user_agent: String::new(),
            url: String::new(),
            extra_headers: HashMap::new(),
        };

        // Exact host match
        let example_cookies = session.cookies_for_domain("example.com");
        assert_eq!(example_cookies.len(), 2); // "example.com" + ".example.com"

        // Subdomain: only the domain cookie ".example.com" matches
        let sub_cookies = session.cookies_for_domain("sub.example.com");
        assert_eq!(sub_cookies.len(), 1); // only ".example.com"

        // Must NOT match "badexample.com" (no dot boundary)
        let bad_cookies = session.cookies_for_domain("badexample.com");
        assert_eq!(bad_cookies.len(), 0);

        // other.com only matches itself
        let other_cookies = session.cookies_for_domain("other.com");
        assert_eq!(other_cookies.len(), 1);
    }

    fn cookie(name: &str, value: &str, domain: &str, path: &str) -> SessionCookie {
        SessionCookie {
            name: name.to_string(),
            value: value.to_string(),
            domain: domain.to_string(),
            path: path.to_string(),
            secure: false,
            http_only: false,
            same_site: None,
            expires: None,
        }
    }

    fn session(cookies: Vec<SessionCookie>) -> BrowserSession {
        BrowserSession::new(cookies, String::new(), String::new())
    }

    #[test]
    fn test_cookie_header_never_includes_other_sites() {
        let session = session(vec![
            cookie("bank", "secret", "bank.test", "/"),
            cookie("shop", "1", "shop.test", "/"),
        ]);
        assert_eq!(session.cookie_header_for("https://shop.test/"), "shop=1");
        assert_eq!(
            session.cookie_header_for("https://bank.test/"),
            "bank=secret"
        );
        assert_eq!(session.cookie_header_for("https://other.test/"), "");
    }

    #[test]
    fn test_cookie_header_preserves_values_exactly() {
        for value in ["tok%3Dabc", "100%", "a b", "a,b", "\"quoted\"", "x=y", ""] {
            let session = session(vec![cookie("k", value, "x.test", "/")]);
            assert_eq!(
                session.cookie_header_for("https://x.test/"),
                format!("k={value}")
            );
        }
    }

    #[test]
    fn test_cookie_header_drops_cookies_that_would_alter_the_header() {
        for (name, value) in [
            ("sid", "x\r\nX-Injected: 1"),
            ("sid", "a;admin=1"),
            ("a\nb", "1"),
            ("a;b", "1"),
            ("a=b", "1"),
            ("", "1"),
        ] {
            let session = session(vec![
                cookie(name, value, "x.test", "/"),
                cookie("ok", "1", "x.test", "/"),
            ]);
            assert_eq!(session.cookie_header_for("https://x.test/"), "ok=1");
        }
    }

    #[test]
    fn test_cookies_for_url_applies_domain_rules() {
        let session = session(vec![
            cookie("host", "1", "example.test", "/"),
            cookie("dom", "2", ".example.test", "/"),
        ]);
        let names = |url| -> Vec<&str> {
            session
                .cookies_for_url(url)
                .iter()
                .map(|c| c.name.as_str())
                .collect()
        };
        assert_eq!(names("https://example.test/"), ["host", "dom"]);
        assert_eq!(names("https://EXAMPLE.test:8443/x?y#z"), ["host", "dom"]);
        assert_eq!(names("https://example.test./"), ["host", "dom"]);
        assert_eq!(names("https://sub.example.test/"), ["dom"]);
        assert!(names("https://badexample.test/").is_empty());
    }

    #[test]
    fn test_cookies_for_url_does_not_treat_ips_as_subdomains() {
        let session = session(vec![
            cookie("suffix", "1", ".0.0.1", "/"),
            cookie("exact", "2", "127.0.0.1", "/"),
            cookie("six", "3", "::1", "/"),
        ]);
        assert_eq!(
            session.cookie_header_for("http://127.0.0.1:8080/"),
            "exact=2"
        );
        assert_eq!(session.cookie_header_for("http://[::1]:8080/"), "six=3");
    }

    #[test]
    fn test_cookies_for_url_applies_path_rules_longest_first() {
        let session = session(vec![
            cookie("root", "1", "x.test", "/"),
            cookie("deep", "2", "x.test", "/a/b"),
            cookie("a", "3", "x.test", "/a"),
        ]);
        assert_eq!(
            session.cookie_header_for("https://x.test/a/b/c"),
            "deep=2; a=3; root=1"
        );
        assert_eq!(session.cookie_header_for("https://x.test/a"), "a=3; root=1");
        assert_eq!(session.cookie_header_for("https://x.test/ab"), "root=1");
        assert_eq!(session.cookie_header_for("https://x.test"), "root=1");
    }

    #[test]
    fn test_cookies_for_url_requires_secure_transport() {
        let mut secure = cookie("s", "1", "x.test", "/");
        secure.secure = true;
        let session = session(vec![secure]);
        assert_eq!(session.cookie_header_for("https://x.test/"), "s=1");
        assert_eq!(session.cookie_header_for("wss://x.test/"), "s=1");
        assert_eq!(session.cookie_header_for("http://x.test/"), "");
        assert_eq!(session.cookie_header_for("ws://x.test/"), "");
    }

    #[test]
    fn test_cookies_for_url_skips_expired() {
        let with_expiry = |name: &str, expires: f64| {
            let mut c = cookie(name, "1", "x.test", "/");
            c.expires = Some(expires);
            c
        };
        let session = session(vec![
            with_expiry("old", 1.0),
            with_expiry("future", 32_503_680_000.0),
            with_expiry("session", -1.0),
        ]);
        assert_eq!(
            session.cookie_header_for("https://x.test/"),
            "future=1; session=1"
        );
    }

    #[test]
    fn test_ambiguous_urls_select_no_cookies() {
        let session = session(vec![
            cookie("bank", "secret", "bank.test", "/"),
            cookie("evil", "1", "evil.test", "/"),
        ]);
        for url in [
            "bank.test",
            "",
            "ftp://bank.test/",
            "file:///bank.test",
            "https://",
            "https://evil.test\\@bank.test/",
            "https://bank.test:notaport/",
            "https://bank.test:99999/",
            "https://bank%2etest/",
            "https://b\u{430}nk.test/",
            "https://bank.test\r\nX: 1/",
            "https://[::1/",
        ] {
            assert_eq!(session.cookie_header_for(url), "", "{url:?}");
        }
        // Userinfo is not the host: this request goes to evil.test.
        assert_eq!(
            session.cookie_header_for("https://bank.test@evil.test/"),
            "evil=1"
        );
    }

    #[test]
    fn test_debug_output_omits_secrets() {
        let mut session = session(vec![cookie("sid", "cookie-secret", "x.test", "/")]);
        session.url = "https://x.test/cb?token=url-secret#frag-secret".into();
        session
            .extra_headers
            .insert("Authorization".into(), "header-secret".into());
        let printed = format!("{session:?}");
        assert!(printed.contains("sid") && printed.contains("Authorization"));
        assert!(printed.contains("https://x.test/cb"));

        let state = BrowserState {
            cookies: session.cookies.clone(),
            local_storage: HashMap::from([("key".into(), "local-secret".into())]),
            session_storage: HashMap::from([("key".into(), "session-secret".into())]),
            user_agent: String::new(),
            url: session.url.clone(),
        };
        let printed = format!("{printed}{state:?}{:?}", session.cookies[0]);
        for secret in [
            "cookie-secret",
            "url-secret",
            "frag-secret",
            "header-secret",
            "local-secret",
            "session-secret",
        ] {
            assert!(!printed.contains(secret), "{secret} leaked: {printed}");
        }
    }

    #[test]
    fn test_domain_matching_ignores_case() {
        let session = session(vec![cookie("k", "1", ".Example.TEST", "/")]);
        assert_eq!(session.cookies_for_domain("sub.EXAMPLE.test").len(), 1);
    }

    #[test]
    fn test_save_round_trips_and_leaves_no_temporary_file() {
        let directory = std::env::temp_dir().join(format!("eoka-save-{:016x}", fastrand::u64(..)));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("session.json");
        let session = session(vec![cookie("k", "v", "x.test", "/")]);
        session.save(path.to_str().unwrap()).unwrap();
        let loaded = BrowserSession::load(path.to_str().unwrap()).unwrap();
        assert_eq!(loaded.cookies[0].value, "v");
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn test_save_is_owner_only_even_when_replacing_a_readable_file() {
        use std::os::unix::fs::PermissionsExt;
        let directory = std::env::temp_dir().join(format!("eoka-save-{:016x}", fastrand::u64(..)));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("session.json");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        session(vec![]).save(path.to_str().unwrap()).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn test_failed_save_keeps_the_existing_file() {
        let directory = std::env::temp_dir().join(format!("eoka-save-{:016x}", fastrand::u64(..)));
        std::fs::create_dir(&directory).unwrap();
        // Renaming a file over a non-empty directory fails.
        let path = directory.join("occupied");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("keep"), "keep").unwrap();
        assert!(session(vec![]).save(path.to_str().unwrap()).is_err());
        assert_eq!(std::fs::read_to_string(path.join("keep")).unwrap(), "keep");
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn test_cookies_for_domain_host_only_vs_domain() {
        let session = BrowserSession {
            cookies: vec![
                // Host-only cookie: stored domain has no leading dot.
                SessionCookie {
                    name: "host".to_string(),
                    value: "1".to_string(),
                    domain: "example.com".to_string(),
                    path: "/".to_string(),
                    secure: false,
                    http_only: false,
                    same_site: None,
                    expires: None,
                },
                // Domain cookie: stored domain has a leading dot.
                SessionCookie {
                    name: "dom".to_string(),
                    value: "2".to_string(),
                    domain: ".example.com".to_string(),
                    path: "/".to_string(),
                    secure: false,
                    http_only: false,
                    same_site: None,
                    expires: None,
                },
            ],
            user_agent: String::new(),
            url: String::new(),
            extra_headers: HashMap::new(),
        };

        let names = |domain: &str| -> Vec<String> {
            session
                .cookies_for_domain(domain)
                .iter()
                .map(|c| c.name.clone())
                .collect()
        };

        // Exact host: both host-only and domain cookie match.
        let exact = names("example.com");
        assert!(exact.contains(&"host".to_string()));
        assert!(exact.contains(&"dom".to_string()));
        assert_eq!(exact.len(), 2);

        // Subdomain: only the domain cookie matches (host-only does NOT).
        let sub = names("sub.example.com");
        assert_eq!(sub, vec!["dom".to_string()]);

        // Suffix that is not a dot-boundary subdomain matches neither.
        let bad = names("badexample.com");
        assert!(bad.is_empty());
    }
}
