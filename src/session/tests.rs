use super::*;

#[test]
fn test_browser_state_round_trips_storage_and_http_only_cookie() {
    let state = BrowserState {
        cookies: vec![SessionCookie {
            secure: true,
            http_only: true,
            same_site: Some("Lax".to_string()),
            ..cookie("session", "secret", ".example.com", "/")
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
            cookie("site1", "v1", "example.com", "/"),
            cookie("site2", "v2", "other.com", "/"),
            cookie("sub", "v3", ".example.com", "/"),
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
        ..Default::default()
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
    assert_eq!(
        session.cookie_header_for("https://shop.test/").unwrap(),
        "shop=1"
    );
    assert_eq!(
        session.cookie_header_for("https://bank.test/").unwrap(),
        "bank=secret"
    );
    assert_eq!(
        session.cookie_header_for("https://other.test/").unwrap(),
        ""
    );
}

#[test]
fn test_cookie_header_preserves_values_exactly() {
    for value in ["tok%3Dabc", "100%", "a b", "a,b", "\"quoted\"", "x=y", ""] {
        let session = session(vec![cookie("k", value, "x.test", "/")]);
        assert_eq!(
            session.cookie_header_for("https://x.test/").unwrap(),
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
        assert_eq!(
            session.cookie_header_for("https://x.test/").unwrap(),
            "ok=1"
        );
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
            .unwrap()
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
        session.cookie_header_for("http://127.0.0.1:8080/").unwrap(),
        "exact=2"
    );
    assert_eq!(
        session.cookie_header_for("http://[::1]:8080/").unwrap(),
        "six=3"
    );
}

#[test]
fn test_cookies_for_url_applies_path_rules_longest_first() {
    let session = session(vec![
        cookie("root", "1", "x.test", "/"),
        cookie("deep", "2", "x.test", "/a/b"),
        cookie("a", "3", "x.test", "/a"),
    ]);
    assert_eq!(
        session.cookie_header_for("https://x.test/a/b/c").unwrap(),
        "deep=2; a=3; root=1"
    );
    assert_eq!(
        session.cookie_header_for("https://x.test/a").unwrap(),
        "a=3; root=1"
    );
    assert_eq!(
        session.cookie_header_for("https://x.test/ab").unwrap(),
        "root=1"
    );
    assert_eq!(
        session.cookie_header_for("https://x.test").unwrap(),
        "root=1"
    );
}

#[test]
fn test_cookies_for_url_requires_secure_transport() {
    let mut secure = cookie("s", "1", "x.test", "/");
    secure.secure = true;
    let session = session(vec![secure]);
    assert_eq!(session.cookie_header_for("https://x.test/").unwrap(), "s=1");
    assert_eq!(session.cookie_header_for("wss://x.test/").unwrap(), "s=1");
    assert_eq!(session.cookie_header_for("http://x.test/").unwrap(), "");
    assert_eq!(session.cookie_header_for("ws://x.test/").unwrap(), "");
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
        session.cookie_header_for("https://x.test/").unwrap(),
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
        let error = session.cookie_header_for(url).unwrap_err();
        assert!(matches!(error, Error::InvalidUrl(_)), "{url:?}");
        assert!(!error.to_string().contains("bank"), "{url:?}: {error}");
    }
    assert_eq!(
        session.cookie_header_for("https://other.test/").unwrap(),
        "",
        "a valid URL with no matching cookies is not an error"
    );
    // Userinfo is not the host: this request goes to evil.test.
    assert_eq!(
        session
            .cookie_header_for("https://bank.test@evil.test/")
            .unwrap(),
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
            cookie("host", "1", "example.com", "/"),
            // Domain cookie: stored domain has a leading dot.
            cookie("dom", "2", ".example.com", "/"),
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

#[test]
fn test_session_files_without_partition_key_still_load() {
    let json = r#"{"name":"a","value":"1","domain":"x.test","path":"/","secure":false,"http_only":false,"same_site":null,"expires":null}"#;
    let cookie: SessionCookie = serde_json::from_str(json).unwrap();
    assert_eq!(cookie.partition_key, None);
    assert!(!serde_json::to_string(&cookie)
        .unwrap()
        .contains("partition_key"));
}

/// A partitioned cookie is sent only to a first-party request for its
/// partition's top-level site, never to its host from another site.
#[test]
fn test_partitioned_cookies_need_their_top_level_site() {
    let partitioned = |site: &str, cross: bool| SessionCookie {
        partition_key: Some(CookiePartitionKey {
            top_level_site: site.into(),
            has_cross_site_ancestor: cross,
        }),
        ..cookie("chips", "1", ".site.test", "/")
    };
    for (site, cross, url, expected) in [
        (
            "https://site.test",
            false,
            "https://app.site.test/",
            "chips=1",
        ),
        ("https://site.test", false, "https://site.test/", "chips=1"),
        ("https://top.test", false, "https://app.site.test/", ""),
        ("https://site.test", true, "https://app.site.test/", ""),
        ("http://site.test", false, "https://app.site.test/", ""),
        ("https://othersite.test", false, "https://site.test/", ""),
        ("garbage", false, "https://site.test/", ""),
    ] {
        let header = session(vec![partitioned(site, cross)])
            .cookie_header_for(url)
            .unwrap();
        assert_eq!(header, expected, "{site} cross={cross} {url}");
    }
}
