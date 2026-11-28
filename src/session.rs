//! Session export for browser-to-HTTP-client handoff
//!
//! After authenticating in the browser, export the session to make
//! direct HTTP requests.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::Path;

use crate::error::{Error, Result};
use crate::Redacted;

mod matching;

use matching::{domain_matches, header_safe, path_matches, RequestTarget};

/// Browser cookie (simplified, serializable). `Debug` omits the value.
///
/// A `domain` with a leading dot is a domain cookie that also covers
/// subdomains; without one the cookie is host-only, as Chrome reports it.
#[derive(Clone, Default, Serialize, Deserialize)]
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
    /// CHIPS partition for a `Partitioned` cookie, or `None` for an
    /// unpartitioned one. Missing in session files written before 0.11.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition_key: Option<CookiePartitionKey>,
}

/// The partition a `Partitioned` (CHIPS) cookie is stored under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CookiePartitionKey {
    /// Top-level site the cookie was set under, such as `https://example.com`.
    pub top_level_site: String,
    /// Whether the cookie was set in a frame with a cross-site ancestor.
    #[serde(default)]
    pub has_cross_site_ancestor: bool,
}

impl CookiePartitionKey {
    /// Whether a top-level request to `target` would use this partition.
    /// Fails closed: anything not provably the same site is a mismatch.
    fn matches_top_level(&self, target: &RequestTarget<'_>) -> bool {
        if self.has_cross_site_ancestor {
            return false;
        }
        let Some((scheme, host)) = self.top_level_site.split_once("://") else {
            return false;
        };
        let secure = match scheme.to_ascii_lowercase().as_str() {
            "https" => true,
            "http" => false,
            _ => return false,
        };
        let host = host.strip_suffix('/').unwrap_or(host);
        secure == target.secure
            && !host.is_empty()
            && !host.contains([':', '/', '@'])
            && domain_matches(&target.host, &format!(".{host}"))
    }
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
            .field("partition_key", &self.partition_key)
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
pub(crate) fn without_query(url: &str) -> &str {
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
    /// A partitioned cookie is included only when its partition is the top-level
    /// site of `url` itself, as for a first-party request.
    ///
    /// Fails with [`Error::InvalidUrl`] for a URL that cannot be parsed safely,
    /// including other schemes and non-ASCII hosts, because a mistaken host would
    /// send credentials to the wrong site. SameSite is not applied, since a direct
    /// HTTP client has no initiating site. Paths are not normalized, so resolve `..`
    /// segments first, and domains are not checked against the public suffix list,
    /// so only load session files you trust.
    pub fn cookies_for_url(&self, url: &str) -> Result<Vec<&SessionCookie>> {
        let target = RequestTarget::parse(url).ok_or_else(|| {
            Error::InvalidUrl(
                "cookie selection needs an absolute http, https, ws or wss URL with an ASCII host"
                    .into(),
            )
        })?;
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
                    && c.partition_key
                        .as_ref()
                        .is_none_or(|key| key.matches_top_level(&target))
            })
            .collect();
        cookies.sort_by_key(|c| std::cmp::Reverse(c.path.len()));
        Ok(cookies)
    }

    /// `Cookie` header value for a request to `url`, empty if no cookie applies, or
    /// [`Error::InvalidUrl`] if the URL cannot be parsed safely.
    ///
    /// Only cookies from [`cookies_for_url`](Self::cookies_for_url) are included, so
    /// credentials are not sent to other sites. Values are byte-for-byte what the
    /// browser would send. A cookie that cannot be represented safely in a header
    /// (an empty name, or a control character, `;`, or `=` in its name) is dropped
    /// rather than altered.
    pub fn cookie_header_for(&self, url: &str) -> Result<String> {
        Ok(self
            .cookies_for_url(url)?
            .into_iter()
            .filter(|c| header_safe(c))
            .map(|c| format!("{}={}", c.name, c.value))
            .collect::<Vec<_>>()
            .join("; "))
    }
}

/// Atomically writes `contents` to `path`, readable only by the owner.
pub(crate) fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("Path has no file name"))?
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
mod tests;
