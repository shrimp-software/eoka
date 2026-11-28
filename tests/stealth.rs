//! Stealth regression checks: launch with the default (shipping) config and
//! read the signals common bot detectors look at from a local page.

mod support;

use serde_json::Value;
use support::{HttpFixture, Response};

const PROBE: &str = r#"(async () => {
  const gl = document.createElement('canvas').getContext('webgl');
  const debug = gl && gl.getExtension('WEBGL_debug_renderer_info');
  const brands = navigator.userAgentData
    ? navigator.userAgentData.brands.map(b => b.brand) : [];
  let permission = null;
  try { permission = (await navigator.permissions.query({ name: 'notifications' })).state; } catch (_) {}
  return {
    webdriver: navigator.webdriver,
    cdcGlobals: Object.keys(window).filter(k => /^\$?cdc_|^\$wdc_/.test(k)),
    chromeKeys: typeof window.chrome === 'object' && window.chrome !== null ? Object.keys(window.chrome).sort() : null,
    plugins: navigator.plugins.length,
    mimeTypes: navigator.mimeTypes.length,
    languages: navigator.languages,
    userAgent: navigator.userAgent,
    appVersion: navigator.appVersion,
    brands,
    notification: typeof Notification === 'undefined' ? null : Notification.permission,
    permission,
    webglVendor: debug ? gl.getParameter(debug.UNMASKED_VENDOR_WEBGL) : null,
    webglRenderer: debug ? gl.getParameter(debug.UNMASKED_RENDERER_WEBGL) : null,
    outer: [window.outerWidth, window.outerHeight],
    inner: [window.innerWidth, window.innerHeight],
    screen: [screen.width, screen.height],
    position: [window.screenX, window.screenY],
    hardwareConcurrency: navigator.hardwareConcurrency,
  };
})()"#;

/// Default (shipping) launch, on the pinned test Chrome when one is configured.
async fn launch(configure: impl FnOnce(&mut eoka::StealthConfig)) -> eoka::Browser {
    eoka::Browser::launch_with(|c| {
        c.chrome_path = std::env::var("EOKA_TEST_CHROME_PATH").ok();
        configure(c);
    })
    .await
    .unwrap()
}

async fn blank_fixture() -> HttpFixture {
    HttpFixture::start(|_| async { Response::html("<!doctype html><title>blank</title>") }).await
}

async fn probe() -> (Value, String) {
    let fixture = HttpFixture::start(|request| async move {
        let accept_language = request
            .headers
            .get("accept-language")
            .cloned()
            .unwrap_or_default();
        Response::html(&format!(
            "<!doctype html><title>probe</title><meta name=al content=\"{accept_language}\">"
        ))
    })
    .await;
    let browser = launch(|_| {}).await;
    let page = browser.new_page(&fixture.url).await.unwrap();
    let signals: Value = page.evaluate(PROBE).await.unwrap();
    let accept_language: String = page
        .evaluate("document.querySelector('meta[name=al]').content")
        .await
        .unwrap();
    browser.close().await.unwrap();
    eprintln!("signals: {signals:#}\naccept-language: {accept_language}");
    (signals, accept_language)
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn default_launch_hides_automation_signals() {
    let (s, accept_language) = probe().await;

    assert_ne!(s["webdriver"], Value::Bool(true), "navigator.webdriver");
    assert_eq!(s["cdcGlobals"], serde_json::json!([]), "driver globals");
    // Real Chrome exposes window.chrome with app/csi/loadTimes on every page.
    let chrome_keys = s["chromeKeys"].as_array().expect("window.chrome missing");
    for key in ["app", "csi", "loadTimes"] {
        assert!(
            chrome_keys.iter().any(|k| k == key),
            "window.chrome.{key}: {chrome_keys:?}"
        );
    }
    assert!(s["plugins"].as_u64().unwrap() > 0, "navigator.plugins");
    assert!(s["mimeTypes"].as_u64().unwrap() > 0, "navigator.mimeTypes");
    assert!(s["hardwareConcurrency"].as_u64().unwrap() > 0);

    for field in ["userAgent", "appVersion"] {
        let value = s[field].as_str().unwrap();
        assert!(!value.contains("Headless"), "{field}: {value}");
    }
    let brands = s["brands"].as_array().unwrap();
    assert!(
        brands
            .iter()
            .all(|b| !b.as_str().unwrap().contains("Headless")),
        "userAgentData brands: {brands:?}"
    );

    // One well-formed quality value per language (Chrome formats it).
    assert!(
        accept_language
            .split(',')
            .all(|part| part.matches(";q=").count() <= 1),
        "malformed Accept-Language: {accept_language}"
    );
    // The JS language list and the Accept-Language header must agree.
    let first_language = s["languages"][0].as_str().unwrap();
    assert!(
        accept_language.starts_with(first_language),
        "navigator.languages {} vs Accept-Language {accept_language}",
        s["languages"]
    );

    // Headless Chrome reports Notification.permission "denied" while the
    // Permissions API says "prompt"; real browsers agree.
    if let (Some(notification), Some(permission)) =
        (s["notification"].as_str(), s["permission"].as_str())
    {
        let expected = if notification == "default" {
            "prompt"
        } else {
            notification
        };
        assert_eq!(permission, expected, "Notification vs permissions.query");
    }

    assert!(
        s["webglVendor"].is_string(),
        "WebGL vendor: {}",
        s["webglVendor"]
    );
    assert!(
        s["webglRenderer"].is_string(),
        "WebGL renderer: {}",
        s["webglRenderer"]
    );
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn outer_window_is_larger_than_viewport() {
    let (s, _) = probe().await;
    let (outer, inner, screen) = (&s["outer"], &s["inner"], &s["screen"]);
    assert_eq!(inner, &serde_json::json!([1920, 1080]), "viewport");
    assert!(
        outer[0].as_u64() >= inner[0].as_u64() && outer[1].as_u64() > inner[1].as_u64(),
        "outer {outer} vs inner {inner}"
    );
    // A window cannot be larger than the screen it is on.
    assert!(
        screen[0].as_u64() >= outer[0].as_u64() && screen[1].as_u64() >= outer[1].as_u64(),
        "screen {screen} vs outer {outer}"
    );
    let position = &s["position"];
    assert!(
        position[0].as_u64().unwrap() + outer[0].as_u64().unwrap() <= screen[0].as_u64().unwrap()
            && position[1].as_u64().unwrap() + outer[1].as_u64().unwrap()
                <= screen[1].as_u64().unwrap(),
        "window at {position} sized {outer} leaves screen {screen}"
    );
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn webgl_renderer_is_not_software() {
    let (s, _) = probe().await;
    let renderer = s["webglRenderer"].as_str().unwrap_or_default();
    assert!(!renderer.is_empty(), "WebGL renderer missing");
    for software in ["SwiftShader", "llvmpipe"] {
        assert!(!renderer.contains(software), "{renderer}");
    }
}

// Regression: timezone.js replaced getTimezoneOffset with a month-based DST
// guess (wrong in early March / early November for US zones, a number for
// invalid dates) and replaced Intl.DateTimeFormat with a non-native function.
// The native Emulation.setTimezoneOverride is exact on its own.
#[tokio::test]
#[ignore = "requires Chrome"]
async fn timezone_is_native_and_exact() {
    let fixture = blank_fixture().await;
    let browser = launch(|c| c.timezone = Some("America/New_York".into())).await;
    let page = browser.new_page(&fixture.url).await.unwrap();
    let v: Value = page
        .evaluate(
            r#"({
            nov15: new Date('2026-11-15T12:00Z').getTimezoneOffset(),
            mar5: new Date('2026-03-05T12:00Z').getTimezoneOffset(),
            mar9: new Date('2026-03-09T12:00Z').getTimezoneOffset(),
            oct30: new Date('2026-10-30T12:00Z').getTimezoneOffset(),
            nov2: new Date('2026-11-02T12:00Z').getTimezoneOffset(),
            nan: String(new Date(NaN).getTimezoneOffset()),
            zone: Intl.DateTimeFormat().resolvedOptions().timeZone,
            dtfLength: Intl.DateTimeFormat.length,
            dtfCtor: Intl.DateTimeFormat.prototype.constructor === Intl.DateTimeFormat,
            dtfSource: Function.prototype.toString.call(Intl.DateTimeFormat),
            tzoSource: Function.prototype.toString.call(Date.prototype.getTimezoneOffset),
        })"#,
        )
        .await
        .unwrap();
    browser.close().await.unwrap();
    assert_eq!(v["nov15"], 300, "{v}");
    assert_eq!(v["mar5"], 300, "EST before the 2026-03-08 transition: {v}");
    assert_eq!(v["mar9"], 240, "{v}");
    assert_eq!(v["oct30"], 240, "{v}");
    assert_eq!(v["nov2"], 300, "EST after the 2026-11-01 transition: {v}");
    assert_eq!(v["nan"], "NaN", "{v}");
    assert_eq!(v["zone"], "America/New_York");
    assert_eq!(v["dtfLength"], 0);
    assert_eq!(v["dtfCtor"], true);
    assert_eq!(
        v["dtfSource"],
        "function DateTimeFormat() { [native code] }"
    );
    assert_eq!(
        v["tzoSource"],
        "function getTimezoneOffset() { [native code] }"
    );
}

/// For each patched member: [name, length, has prototype, `new` throws a
/// TypeError, toString]. Native values were captured from unpatched Chrome 156.
const SHAPE_PROBE: &str = r#"(async () => {
  const shape = (f) => {
    if (typeof f !== 'function') return null;
    let newThrows = false;
    try { new f(); } catch (e) { newThrows = e instanceof TypeError; }
    return [f.name, f.length, 'prototype' in f, newThrows, Function.prototype.toString.call(f)];
  };
  const getter = (o, p) => { const d = Object.getOwnPropertyDescriptor(o, p); return d && shape(d.get); };
  const methods = {
    query: Permissions.prototype.query,
    getVoices: SpeechSynthesis.prototype.getVoices,
    enumerateDevices: MediaDevices.prototype.enumerateDevices,
    check: FontFaceSet.prototype.check,
    getHighEntropyValues: NavigatorUAData.prototype.getHighEntropyValues,
    getParameter: WebGLRenderingContext.prototype.getParameter,
    getParameter2: WebGL2RenderingContext.prototype.getParameter,
    toDataURL: HTMLCanvasElement.prototype.toDataURL,
    toBlob: HTMLCanvasElement.prototype.toBlob,
    getImageData: CanvasRenderingContext2D.prototype.getImageData,
    getChannelData: AudioBuffer.prototype.getChannelData,
    getBattery: Navigator.prototype.getBattery,
    getLayoutMap: Keyboard.prototype.getLayoutMap,
  };
  const out = { methods: {}, getters: {} };
  for (const [k, f] of Object.entries(methods)) out.methods[k] = shape(f);
  for (const p of ['languages', 'platform', 'hardwareConcurrency', 'deviceMemory', 'plugins', 'webdriver'])
    out.getters[p] = getter(Navigator.prototype, p);
  const own = { navigator, permissions: navigator.permissions, speechSynthesis,
    mediaDevices: navigator.mediaDevices, fonts: document.fonts, connection: navigator.connection,
    userAgentData: navigator.userAgentData, keyboard: navigator.keyboard };
  out.ownKeys = {};
  for (const [k, o] of Object.entries(own)) out.ownKeys[k] = Reflect.ownKeys(o).map(String);
  out.languagesStable = navigator.languages === navigator.languages && Object.isFrozen(navigator.languages);
  const first = JSON.stringify(chrome.loadTimes());
  await new Promise(r => setTimeout(r, 50));
  out.loadTimesStable = first === JSON.stringify(chrome.loadTimes());
  out.chromeSource = Function.prototype.toString.call(chrome.loadTimes);
  out.ownerDocument = document.createElement('div').ownerDocument === document;
  out.permission = [Notification.permission, (await navigator.permissions.query({ name: 'notifications' })) instanceof PermissionStatus];
  out.layoutMap = (await navigator.keyboard.getLayoutMap()).get('KeyA');
  let illegal = null;
  try { HTMLCanvasElement.prototype.toDataURL.call({}); } catch (e) { illegal = e.constructor.name; }
  out.illegalInvocation = illegal;
  return out;
})()"#;

// Regression: overrides were installed on instances with constructible
// function expressions of the wrong length, window.chrome was replaced with
// per-call random loadTimes, and navigator.languages returned a new array per
// read. Every patched member must keep its native shape.
#[tokio::test]
#[ignore = "requires Chrome"]
async fn patched_apis_keep_native_shape() {
    let fixture = blank_fixture().await;
    // A Windows identity exercises the keyboard and voice fallbacks too.
    let browser = launch(|c| {
        c.user_agent = Some("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/156.0.0.0 Safari/537.36".into());
    })
    .await;
    let page = browser.new_page(&fixture.url).await.unwrap();
    let v: Value = page.evaluate(SHAPE_PROBE).await.unwrap();
    browser.close().await.unwrap();
    eprintln!("{v:#}");

    let native = |name: &str, length: u64| {
        serde_json::json!([
            name,
            length,
            false,
            true,
            format!("function {name}() {{ [native code] }}")
        ])
    };
    for (key, name, length) in [
        ("query", "query", 1),
        ("getVoices", "getVoices", 0),
        ("enumerateDevices", "enumerateDevices", 0),
        ("check", "check", 1),
        ("getHighEntropyValues", "getHighEntropyValues", 1),
        ("getParameter", "getParameter", 1),
        ("getParameter2", "getParameter", 1),
        ("toDataURL", "toDataURL", 0),
        ("toBlob", "toBlob", 1),
        ("getImageData", "getImageData", 4),
        ("getChannelData", "getChannelData", 1),
        ("getBattery", "getBattery", 0),
        ("getLayoutMap", "getLayoutMap", 0),
    ] {
        assert_eq!(v["methods"][key], native(name, length), "{key}");
    }
    for prop in [
        "languages",
        "platform",
        "hardwareConcurrency",
        "deviceMemory",
        "plugins",
        "webdriver",
    ] {
        let name = format!("get {prop}");
        assert_eq!(v["getters"][prop], native(&name, 0), "getter {prop}");
    }
    for (instance, keys) in v["ownKeys"].as_object().unwrap() {
        assert_eq!(keys, &serde_json::json!([]), "own properties on {instance}");
    }
    assert_eq!(v["languagesStable"], true);
    assert_eq!(v["loadTimesStable"], true);
    assert_eq!(v["chromeSource"], "function () { [native code] }");
    assert_eq!(v["ownerDocument"], true);
    assert_eq!(v["permission"], serde_json::json!(["default", true]));
    assert_eq!(v["layoutMap"], "a");
    assert_eq!(v["illegalInvocation"], "TypeError");
}

// Regression: the fonts.check hook reported families the page registered
// with @font-face as missing. Page fonts fall through to the native check.
#[tokio::test]
#[ignore = "requires Chrome"]
async fn fonts_check_reports_page_registered_fonts() {
    let font = ["/usr/share/fonts", "/usr/local/share/fonts"]
        .iter()
        .flat_map(|root| walk(std::path::Path::new(root)))
        .find(|path| path.extension().is_some_and(|e| e == "ttf"))
        .expect("a .ttf system font for the fixture");
    let bytes = std::fs::read(font).unwrap();
    let fixture = HttpFixture::start(move |request| {
        let bytes = bytes.clone();
        async move {
            if request.target == "/font.ttf" {
                Response {
                    status: "200 OK",
                    headers: vec![("Content-Type".into(), "font/ttf".into())],
                    body: bytes,
                    body_gate: None,
                }
            } else {
                Response::html("<!doctype html><style>@font-face { font-family: 'Eoka Page Font'; src: url(/font.ttf); }</style><p style=\"font-family: 'Eoka Page Font'\">x</p>")
            }
        }
    })
    .await;
    let browser = launch(|_| {}).await;
    let page = browser.new_page(&fixture.url).await.unwrap();
    let v: Value = page
        .evaluate(
            r#"(async () => {
            await document.fonts.load("16px 'Eoka Page Font'");
            return {
                page: document.fonts.check("16px 'Eoka Page Font'"),
                pageWithGeneric: document.fonts.check("16px 'Eoka Page Font', sans-serif"),
                unclaimed: document.fonts.check('16px "Eoka Unclaimed System Font"'),
            };
        })()"#,
        )
        .await
        .unwrap();
    browser.close().await.unwrap();
    assert_eq!(v["page"], true, "{v}");
    assert_eq!(v["pageWithGeneric"], true, "{v}");
    assert_eq!(v["unclaimed"], false, "{v}");
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(walk(&path));
        } else {
            files.push(path);
        }
    }
    files
}

const WEBRTC_PROBE: &str = r#"(async () => {
  const a = new RTCPeerConnection(), b = new RTCPeerConnection();
  const candidates = [];
  a.onicecandidate = e => { if (e.candidate) { candidates.push(e.candidate.candidate); b.addIceCandidate(e.candidate); } };
  b.onicecandidate = e => { if (e.candidate) a.addIceCandidate(e.candidate); };
  const channel = a.createDataChannel('probe');
  const state = new Promise(r => { channel.onopen = () => r('open'); setTimeout(() => r('timeout'), 5000); });
  await a.setLocalDescription(await a.createOffer());
  await b.setRemoteDescription(a.localDescription);
  await b.setLocalDescription(await b.createAnswer());
  await a.setRemoteDescription(b.localDescription);
  return { state: await state, udp: candidates.filter(c => / udp /i.test(c)).length };
})()"#;

// Regression: webrtc.js removed every STUN/TURN server from the caller's
// config (mutating it) without stopping the leak. With a proxy, Chrome's
// IP-handling policy keeps WebRTC from sending UDP around it; without one,
// WebRTC keeps working with mDNS-obfuscated host candidates.
#[tokio::test]
#[ignore = "requires Chrome"]
async fn webrtc_bypasses_no_configured_proxy() {
    let fixture = blank_fixture().await;
    let direct = launch(|c| c.geo_align = false).await;
    let page = direct.new_page(&fixture.url).await.unwrap();
    let v: Value = page.evaluate(WEBRTC_PROBE).await.unwrap();
    direct.close().await.unwrap();
    assert_eq!(v["state"], "open", "WebRTC must work without a proxy: {v}");

    // Loopback fixtures bypass the (unreachable) proxy, so the page loads.
    let proxied = launch(|c| {
        c.geo_align = false;
        c.proxy = Some("http://127.0.0.1:9".into());
    })
    .await;
    let page = proxied.new_page(&fixture.url).await.unwrap();
    let v: Value = page.evaluate(WEBRTC_PROBE).await.unwrap();
    proxied.close().await.unwrap();
    assert_eq!(v["udp"], 0, "no non-proxied UDP candidates: {v}");
}

// Regression: Chromium keeps only the last --blink-settings/--disable-features,
// so a user's own flag silently dropped eoka's pointer/hover pinning and
// AutomationControlled. User lists are merged into eoka's.
#[tokio::test]
#[ignore = "requires Chrome"]
async fn user_feature_flags_merge_with_stealth_flags() {
    let fixture = blank_fixture().await;
    let browser = launch(|c| {
        c.extra_args = vec![
            "--blink-settings=imagesEnabled=true".into(),
            "--disable-blink-features=NotificationTriggers".into(),
            "--disable-features=Translate".into(),
        ];
    })
    .await;
    let page = browser.new_page(&fixture.url).await.unwrap();
    let v: Value = page
        .evaluate(
            r#"({
            hover: matchMedia('(hover: hover)').matches,
            pointer: matchMedia('(pointer: fine)').matches,
            webdriver: Object.getOwnPropertyDescriptor(Navigator.prototype, 'webdriver').get.call(navigator),
        })"#,
        )
        .await
        .unwrap();
    browser.close().await.unwrap();
    assert_eq!(v["hover"], true, "{v}");
    assert_eq!(v["pointer"], true, "{v}");
    assert_eq!(v["webdriver"], false, "{v}");
}

// Cross-site iframes are out-of-process (the old IsolateOrigins/site-per-process
// feature disables never applied: `site-per-process` is a switch, not a feature),
// and eoka does not auto-attach to them yet, so they run without the evasion
// script, UA metadata or timezone override.
#[tokio::test]
#[ignore = "known gap: cross-site iframes do not receive evasions (OOPIF auto-attach)"]
async fn known_gap_cross_site_iframe_matches_top_frame() {
    let fixture = HttpFixture::start(|request| async move {
        if request.target.starts_with("/child") {
            Response::html("<!doctype html><script>parent.postMessage(JSON.stringify([new Date('2026-07-15T12:00Z').getTimezoneOffset(), navigator.hardwareConcurrency, navigator.userAgentData.brands.map(b => b.brand)]), '*')</script>")
        } else {
            let port = request.headers["host"].rsplit(':').next().unwrap().to_owned();
            Response::html(&format!("<!doctype html><script>window.got = null; addEventListener('message', e => window.got = e.data)</script><iframe src='http://localhost:{port}/child'></iframe>"))
        }
    })
    .await;
    let browser = launch(|c| c.timezone = Some("Asia/Tokyo".into())).await;
    let page = browser.new_page(&fixture.url).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let top: String = page
        .evaluate("JSON.stringify([new Date('2026-07-15T12:00Z').getTimezoneOffset(), navigator.hardwareConcurrency, navigator.userAgentData.brands.map(b => b.brand)])")
        .await
        .unwrap();
    let child: Value = page.evaluate("window.got").await.unwrap();
    browser.close().await.unwrap();
    assert_eq!(child.as_str(), Some(top.as_str()));
}

// Regression: Human::move_to started each path at a random point within 300px
// of the target (teleporting from the previous position), and scroll sent
// wheel events at random coordinates. Both follow the real pointer now.
#[tokio::test]
#[ignore = "requires Chrome"]
async fn human_paths_start_at_the_pointer() {
    let fixture = HttpFixture::start(|_| async {
        Response::html("<!doctype html><body style='height:5000px;margin:0'><script>window.events = []; addEventListener('mousemove', e => events.push(['move', e.clientX, e.clientY])); addEventListener('wheel', e => events.push(['wheel', e.clientX, e.clientY]));</script>")
    })
    .await;
    let browser = launch(|_| {}).await;
    let page = browser.new_page(&fixture.url).await.unwrap();
    let human = page.human().with_speed(eoka::HumanSpeed::Fast);
    human.move_to(100.0, 120.0).await.unwrap();
    page.evaluate::<Value>("window.events = []").await.unwrap();
    human.move_to(500.0, 400.0).await.unwrap();
    let second: Value = page.evaluate("window.events").await.unwrap();
    page.evaluate::<Value>("window.events = []").await.unwrap();
    human.scroll(300.0).await.unwrap();
    let wheel: Value = page.evaluate("window.events").await.unwrap();
    browser.close().await.unwrap();

    let second = second.as_array().unwrap();
    assert_eq!(
        second.first().unwrap(),
        &serde_json::json!(["move", 100, 120]),
        "{second:?}"
    );
    assert_eq!(
        second.last().unwrap(),
        &serde_json::json!(["move", 500, 400]),
        "{second:?}"
    );
    let wheel = wheel.as_array().unwrap();
    assert!(!wheel.is_empty());
    assert!(
        wheel
            .iter()
            .all(|e| e == &serde_json::json!(["wheel", 500, 400])),
        "{wheel:?}"
    );
}
