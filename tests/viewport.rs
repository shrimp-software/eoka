use base64::Engine;
use eoka::{
    Browser, Error, Page, ScreenshotClip, ScreenshotFormat, ScreenshotOptions, StealthConfig,
};
use serde_json::{json, Value};

const FIXTURE: &str = "data:text/html,<meta name='viewport' content='width=device-width,initial-scale=1'><style>html,body{margin:0;overflow:hidden}div{width:100vw;height:100vh}</style><div></div>";
const METRICS: &str = r#"(() => {
    const box = document.querySelector('div').getBoundingClientRect();
    return {
        width: innerWidth, height: innerHeight, scale: devicePixelRatio,
        layoutWidth: box.width, layoutHeight: box.height,
        outerWidth, outerHeight,
        screenWidth: screen.width, screenHeight: screen.height,
        availWidth: screen.availWidth, availHeight: screen.availHeight,
        resolutionMatches: matchMedia(`(resolution: ${devicePixelRatio}dppx)`).matches
    };
})()"#;

async fn metrics(page: &Page) -> Value {
    page.evaluate(METRICS).await.unwrap()
}

async fn assert_native_metrics(page: &Page, exposed: &Value) {
    let tree: Value = page
        .session()
        .send("Page.getFrameTree", &json!({}))
        .await
        .unwrap();
    let world: Value = page
        .session()
        .send(
            "Page.createIsolatedWorld",
            &json!({
                "frameId": tree["frameTree"]["frame"]["id"],
                "worldName": "viewport-native-metrics"
            }),
        )
        .await
        .unwrap();
    let native: Value = page
        .session()
        .send(
            "Runtime.evaluate",
            &json!({
                "contextId": world["executionContextId"],
                "expression": METRICS,
                "returnByValue": true
            }),
        )
        .await
        .unwrap();
    assert_eq!(exposed, &native["result"]["value"]);
    assert_eq!(exposed["resolutionMatches"], true);
}

async fn assert_capture(page: &Page, expected: (u32, u32)) {
    let before = metrics(page).await;
    let png = page.screenshot().await.unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(&png[12..16], b"IHDR");
    let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
    assert_eq!((width, height), expected);
    assert_eq!(
        metrics(page).await,
        before,
        "capture changed viewport metrics"
    );
}

async fn assert_viewport(page: &Page, width: u32, height: u32, scale: f64) {
    let measured = metrics(page).await;
    assert_eq!(measured["width"], width);
    assert_eq!(measured["height"], height);
    assert_eq!(measured["scale"], scale);
    assert_eq!(measured["layoutWidth"].as_f64().unwrap(), f64::from(width));
    assert_eq!(
        measured["layoutHeight"].as_f64().unwrap(),
        f64::from(height)
    );
    assert_native_metrics(page, &measured).await;
    assert_capture(
        page,
        (
            (f64::from(width) * scale).round() as u32,
            (f64::from(height) * scale).round() as u32,
        ),
    )
    .await;
}

#[tokio::test]
#[ignore = "requires Chrome; verifies native layout and PNG physical pixels"]
async fn configured_viewport_change_and_reset_follow_native_metrics() {
    let browser = Browser::launch_with(|config| {
        config.patch_binary = false;
        config.viewport_width = 640;
        config.viewport_height = 360;
        config.device_scale_factor = 2.0;
        config
            .extra_args
            .push("--force-device-scale-factor=1".into());
    })
    .await
    .unwrap();
    let page = browser.new_page(FIXTURE).await.unwrap();
    assert_viewport(&page, 640, 360, 2.0).await;

    page.set_viewport(800, 450, 1.5).await.unwrap();
    assert_viewport(&page, 800, 450, 1.5).await;
    page.goto(FIXTURE).await.unwrap();
    assert_viewport(&page, 800, 450, 1.5).await;

    page.reset_viewport().await.unwrap();
    let native = metrics(&page).await;
    assert_eq!(native["scale"], 1.0);
    assert_native_metrics(&page, &native).await;
    let native_width = native["width"].as_u64().unwrap() as u32;
    let native_height = native["height"].as_u64().unwrap() as u32;
    assert!(native_width > 0 && native_height > 0);
    assert_capture(&page, (native_width, native_height)).await;

    page.set_viewport(320, 200, 2.0).await.unwrap();
    assert_viewport(&page, 320, 200, 2.0).await;
    page.clone().reset_viewport().await.unwrap();
    assert_eq!(metrics(&page).await, native);
    page.goto(FIXTURE).await.unwrap();
    assert_eq!(metrics(&page).await, native);
    assert_capture(&page, (native_width, native_height)).await;
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; verifies live attachment preserves existing emulation"]
async fn live_attachment_preserves_metrics_until_an_explicit_page_change() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let owner = Browser::launch_with(|config| {
        config.patch_binary = false;
        config.live_session = true;
        config
            .extra_args
            .push(format!("--remote-debugging-port={port}"));
    })
    .await
    .unwrap();
    let original = owner.new_page(FIXTURE).await.unwrap();
    let native = metrics(&original).await;
    original.set_viewport(640, 360, 2.0).await.unwrap();
    assert_viewport(&original, 640, 360, 2.0).await;

    let borrowed = Browser::connect_port_with_config(
        port,
        StealthConfig {
            live_session: true,
            viewport_width: 900,
            viewport_height: 700,
            device_scale_factor: 3.0,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let attached = borrowed.attach_page(original.target_id()).await.unwrap();
    assert_viewport(&attached, 640, 360, 2.0).await;
    attached.set_viewport(800, 450, 1.0).await.unwrap();
    assert_viewport(&original, 800, 450, 1.0).await;
    attached.reset_viewport().await.unwrap();
    assert_eq!(metrics(&attached).await, native);
    assert_native_metrics(&attached, &native).await;
    let scale = native["scale"].as_f64().unwrap();
    assert_capture(
        &attached,
        (
            (native["width"].as_f64().unwrap() * scale).round() as u32,
            (native["height"].as_f64().unwrap() * scale).round() as u32,
        ),
    )
    .await;
    borrowed.close().await.unwrap();
    assert_eq!(metrics(&original).await, native);
    owner.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; invalid viewport values must fail without changing metrics or using CDP"]
async fn invalid_viewports_leave_layout_unchanged_and_validate_after_disconnect() {
    let browser = Browser::launch_with(|config| {
        config.patch_binary = false;
        config.viewport_width = 640;
        config.viewport_height = 360;
        config.device_scale_factor = 2.0;
    })
    .await
    .unwrap();
    let page = browser.new_page(FIXTURE).await.unwrap();
    let before = metrics(&page).await;
    let invalid = [
        (0, 360, 1.0),
        (640, 0, 1.0),
        (i32::MAX as u32 + 1, 360, 1.0),
        (640, u32::MAX, 1.0),
        (640, 360, 0.0),
        (640, 360, -1.0),
        (640, 360, f64::NAN),
        (640, 360, f64::INFINITY),
        (640, 360, f64::NEG_INFINITY),
    ];
    for (width, height, scale) in invalid {
        let error = page.set_viewport(width, height, scale).await.unwrap_err();
        assert!(matches!(
            error,
            Error::Cdp {
                method: None,
                code: None,
                ..
            }
        ));
        assert_eq!(metrics(&page).await, before);
    }
    assert_capture(&page, (1280, 720)).await;
    browser.close().await.unwrap();
    for (width, height, scale) in invalid {
        let error = page.set_viewport(width, height, scale).await.unwrap_err();
        assert!(matches!(
            error,
            Error::Cdp {
                method: None,
                code: None,
                ..
            }
        ));
    }
}

#[tokio::test]
#[ignore = "requires Chrome; reset must clear emulation owned by a different CDP session"]
async fn fresh_attachment_reset_clears_another_sessions_override() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let owner = Browser::launch_with(|config| {
        config.patch_binary = false;
        config.live_session = true;
        config
            .extra_args
            .push(format!("--remote-debugging-port={port}"));
    })
    .await
    .unwrap();
    let original = owner.new_page(FIXTURE).await.unwrap();
    let native = metrics(&original).await;
    let _: Value = original
        .session()
        .send(
            "Emulation.setDeviceMetricsOverride",
            &json!({"width":640,"height":360,"deviceScaleFactor":2,"mobile":false}),
        )
        .await
        .unwrap();
    let borrowed = Browser::connect_port(port).await.unwrap();
    let attached = borrowed.attach_page(original.target_id()).await.unwrap();
    let preserved = metrics(&attached).await;
    assert_eq!(preserved["width"], 640);
    assert_eq!(preserved["height"], 360);
    assert_eq!(preserved["scale"], 2.0);
    attached.reset_viewport().await.unwrap();
    assert_eq!(metrics(&attached).await, native);
    assert_eq!(metrics(&original).await, native);
    assert_native_metrics(&attached, &native).await;
    let scale = native["scale"].as_f64().unwrap();
    let expected = (
        (native["width"].as_f64().unwrap() * scale).round() as u32,
        (native["height"].as_f64().unwrap() * scale).round() as u32,
    );
    assert_capture(&attached, expected).await;
    assert_capture(&original, expected).await;
    borrowed.close().await.unwrap();
    owner.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; verifies odd fractional-DPR capture edge pixels"]
async fn fractional_dpr_capture_preserves_right_and_bottom_edge_pixels() {
    for native_scale in [1.0, 1.25, 2.0] {
        let browser = Browser::launch_with(|config| {
            config.patch_binary = false;
            config.canvas_spoof = false;
            config.webgl_spoof = false;
            config.audio_spoof = false;
            config.viewport_width = 801;
            config.viewport_height = 451;
            config.device_scale_factor = 1.5;
            config
                .extra_args
                .push(format!("--force-device-scale-factor={native_scale}"));
        })
        .await
        .unwrap();
        let page = browser.new_page(FIXTURE).await.unwrap();
        page.execute(
            r#"(() => {
                const canvas = document.createElement('canvas');
                canvas.width = 1000; canvas.height = 1000;
                canvas.style.cssText = 'position:absolute;left:0;top:0;width:1000px;height:1000px';
                document.body.append(canvas);
                const context = canvas.getContext('2d');
                context.fillStyle = 'rgb(20,20,240)';
                context.fillRect(0,0,1000,1000);
                context.fillStyle = 'rgb(240,20,20)';
                context.fillRect(790,0,210,1000);
                context.fillStyle = 'rgb(20,240,20)';
                context.fillRect(0,440,1000,560);
            })()"#,
        )
        .await
        .unwrap();
        let observed = metrics(&page).await;
        if native_scale == 1.25 {
            let reported_scale = observed["scale"].as_f64().unwrap();
            assert!((reported_scale - 1.5).abs() <= 1.5 * f64::from(f32::EPSILON));
            assert_native_metrics(&page, &observed).await;
            assert_capture(&page, (1202, 677)).await;
        } else {
            assert_viewport(&page, 801, 451, 1.5).await;
        }
        let before = metrics(&page).await;
        let png = page.screenshot().await.unwrap();
        let encoded = base64::engine::general_purpose::STANDARD.encode(png);
        let pixels: Value = page
            .evaluate(&format!(
                r#"(async () => {{
                    const image = new Image();
                    image.src = 'data:image/png;base64,{encoded}';
                    await image.decode();
                    const canvas = document.createElement('canvas');
                    canvas.width = image.width; canvas.height = image.height;
                    const context = canvas.getContext('2d');
                    context.drawImage(image,0,0);
                    const pixel = (x,y) => Array.from(context.getImageData(x,y,1,1).data);
                    return {{
                        width: image.width, height: image.height,
                        left: pixel(0,100), top: pixel(100,0),
                        right: pixel(image.width-2,100),
                        bottom: pixel(100,image.height-2),
                        corner: pixel(image.width-2,image.height-2)
                    }};
                }})()"#
            ))
            .await
            .unwrap();
        assert_eq!(pixels["width"], 1202, "native DPR {native_scale}");
        assert_eq!(pixels["height"], 677, "native DPR {native_scale}");
        assert_eq!(pixels["left"], json!([20, 20, 240, 255]));
        assert_eq!(pixels["top"], json!([20, 20, 240, 255]));
        assert_eq!(
            pixels["right"],
            json!([240, 20, 20, 255]),
            "native DPR {native_scale}"
        );
        assert_eq!(
            pixels["bottom"],
            json!([20, 240, 20, 255]),
            "native DPR {native_scale}"
        );
        assert_eq!(
            pixels["corner"],
            json!([20, 240, 20, 255]),
            "native DPR {native_scale}"
        );
        assert_eq!(metrics(&page).await, before);
        page.reset_viewport().await.unwrap();
        assert_eq!(metrics(&page).await["scale"], native_scale);
        browser.close().await.unwrap();
    }
}

#[tokio::test]
#[ignore = "requires Chrome; a disconnected emulation controller must not break a surviving owner"]
async fn closed_attachment_controller_allows_owner_viewport_changes() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let owner = Browser::launch_with(|config| {
        config.patch_binary = false;
        config.live_session = true;
        config
            .extra_args
            .push(format!("--remote-debugging-port={port}"));
    })
    .await
    .unwrap();
    let original = owner.new_page(FIXTURE).await.unwrap();
    let native = metrics(&original).await;
    let borrowed = Browser::connect_port(port).await.unwrap();
    let attached = borrowed.attach_page(original.target_id()).await.unwrap();
    attached.set_viewport(640, 360, 2.0).await.unwrap();
    assert_viewport(&original, 640, 360, 2.0).await;
    borrowed.close().await.unwrap();
    original.set_viewport(320, 200, 1.5).await.unwrap();
    assert_viewport(&original, 320, 200, 1.5).await;
    original.reset_viewport().await.unwrap();
    assert_eq!(metrics(&original).await, native);
    owner.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; clipped emulated capture must preserve the visible scrolled viewport"]
async fn emulated_capture_preserves_scroll_position_and_visible_content() {
    let browser = Browser::launch_with(|config| {
        config.patch_binary = false;
        config.canvas_spoof = false;
        config.webgl_spoof = false;
        config.audio_spoof = false;
        config.viewport_width = 640;
        config.viewport_height = 360;
        config.device_scale_factor = 2.0;
    })
    .await
    .unwrap();
    let page = browser.new_page(FIXTURE).await.unwrap();
    page.execute(
        r#"(() => {
            document.querySelector('style').textContent =
                'html,body{margin:0}::-webkit-scrollbar{display:none}div{width:100vw;height:100vh}';
            const canvas = document.createElement('canvas');
            canvas.width = 640; canvas.height = 2000;
            const context = canvas.getContext('2d');
            context.fillStyle = 'rgb(20,20,240)'; context.fillRect(0,0,640,600);
            context.fillStyle = 'rgb(20,240,20)'; context.fillRect(0,600,640,1400);
            canvas.style.cssText = 'position:absolute;top:0;left:0';
            document.body.append(canvas);
            document.body.style.height = '2000px';
            scrollTo(0,500);
        })()"#,
    )
    .await
    .unwrap();
    let position: Value = page.evaluate("[scrollX,scrollY]").await.unwrap();
    assert_eq!(position, json!([0, 500]));
    assert_viewport(&page, 640, 360, 2.0).await;
    let encoded =
        base64::engine::general_purpose::STANDARD.encode(page.screenshot().await.unwrap());
    let pixel: Value = page
        .evaluate(&format!(
            r#"(async () => {{
                const image = new Image();
                image.src = 'data:image/png;base64,{encoded}';
                await image.decode();
                const canvas = document.createElement('canvas');
                canvas.width = image.width; canvas.height = image.height;
                const context = canvas.getContext('2d');
                context.drawImage(image,0,0);
                return Array.from(context.getImageData(640,360,1,1).data);
            }})()"#
        ))
        .await
        .unwrap();
    assert_eq!(pixel, json!([20, 240, 20, 255]));
    assert_eq!(
        page.evaluate::<Value>("[scrollX,scrollY]").await.unwrap(),
        position
    );
    page.reset_viewport().await.unwrap();
    assert_eq!(
        page.evaluate::<Value>("[scrollX,scrollY]").await.unwrap(),
        position
    );
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; reset must synchronize a healthy borrowed background tab without activating it"]
async fn borrowed_background_reset_restores_native_layout_without_foregrounding() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let owner = Browser::launch_with(|config| {
        config.patch_binary = false;
        config.live_session = true;
        config.cdp_timeout = 5;
        config
            .extra_args
            .push(format!("--remote-debugging-port={port}"));
    })
    .await
    .unwrap();
    let original = owner.new_page(FIXTURE).await.unwrap();
    let active = owner
        .new_page("data:text/html,<title>foreground tab</title>")
        .await
        .unwrap();
    owner.activate_tab(active.target_id()).await.unwrap();
    assert!(original.evaluate::<bool>("document.hidden").await.unwrap());
    let native = metrics(&original).await;
    let window: Value = original
        .session()
        .transport()
        .send(
            "Browser.getWindowForTarget",
            &json!({"targetId": original.target_id()}),
        )
        .await
        .unwrap();
    let borrowed = Browser::connect_port_with_config(
        port,
        StealthConfig {
            live_session: true,
            cdp_timeout: 5,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let attached = borrowed.attach_page(original.target_id()).await.unwrap();
    assert_eq!(metrics(&attached).await, native);
    attached.set_viewport(640, 360, 2.0).await.unwrap();
    attached.reset_viewport().await.unwrap();
    let restored = metrics(&attached).await;
    for key in ["width", "height", "scale", "layoutWidth", "layoutHeight"] {
        assert_eq!(
            restored[key].as_f64().unwrap(),
            native[key].as_f64().unwrap(),
            "{key}"
        );
    }
    assert_eq!(metrics(&original).await, restored);
    assert_native_metrics(&attached, &restored).await;
    let after: Value = attached
        .session()
        .transport()
        .send(
            "Browser.getWindowForTarget",
            &json!({"targetId": attached.target_id()}),
        )
        .await
        .unwrap();
    assert_eq!(after, window);
    assert!(original.evaluate::<bool>("document.hidden").await.unwrap());
    assert!(!active.evaluate::<bool>("document.hidden").await.unwrap());
    borrowed.close().await.unwrap();
    owner.close().await.unwrap();
}

async fn screenshot_pixels(page: &Page, bytes: &[u8], mime_type: &str) -> Value {
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    page.evaluate(&format!(
        r#"(async () => {{
            const image = new Image();
            image.src = 'data:{mime_type};base64,{encoded}';
            await image.decode();
            const canvas = document.createElement('canvas');
            canvas.width = image.width; canvas.height = image.height;
            const context = canvas.getContext('2d');
            context.drawImage(image,0,0);
            const pixel = (x,y) => Array.from(context.getImageData(x,y,1,1).data);
            return {{
                width: image.width, height: image.height,
                top: pixel(Math.floor(image.width/2),10),
                bottom: pixel(Math.floor(image.width/2),image.height-10)
            }};
        }})()"#
    ))
    .await
    .unwrap()
}

fn assert_capture_color(actual: &Value, expected: [u8; 3]) {
    for (channel, expected) in expected.into_iter().enumerate() {
        let actual = actual[channel].as_i64().unwrap();
        assert!(
            (actual - i64::from(expected)).abs() <= 5,
            "wrong color: {actual}"
        );
    }
    assert_eq!(actual[3], 255);
}

#[tokio::test]
#[ignore = "requires Chrome; verifies PNG/JPEG/WebP crop/full-page pixels and DPR without changing layout"]
async fn typed_capture_formats_crop_and_full_page_preserve_document_metrics() {
    let browser = Browser::launch_with(|config| {
        config.patch_binary = false;
        config.canvas_spoof = false;
        config.viewport_width = 240;
        config.viewport_height = 160;
        config.device_scale_factor = 2.0;
    })
    .await
    .unwrap();
    let page = browser.new_page("about:blank").await.unwrap();
    page.execute(
        r#"document.documentElement.innerHTML = '<head><style>html,body{margin:0}::-webkit-scrollbar{display:none}div{width:300px;height:600px;background:linear-gradient(to bottom,rgb(20,20,240) 0px,rgb(20,20,240) 300px,rgb(20,240,20) 300px,rgb(20,240,20) 600px)}</style></head><body><div></div></body>'; scrollTo(0,350);"#,
    )
    .await
    .unwrap();
    let state = "[innerWidth,innerHeight,devicePixelRatio,scrollX,scrollY]";
    let before: Value = page.evaluate(state).await.unwrap();
    assert_eq!(before, json!([240, 160, 2, 0, 350]));
    for (format, mime_type) in [
        (ScreenshotFormat::Png, "image/png"),
        (ScreenshotFormat::Jpeg, "image/jpeg"),
        (ScreenshotFormat::Webp, "image/webp"),
    ] {
        let base = ScreenshotOptions {
            format,
            quality: if format == ScreenshotFormat::Png {
                None
            } else {
                Some(100)
            },
            ..Default::default()
        };
        let viewport = page.screenshot_with_options(&base).await.unwrap();
        assert_eq!(page.evaluate::<Value>(state).await.unwrap(), before);
        let pixels = screenshot_pixels(&page, &viewport, mime_type).await;
        assert_eq!(
            (pixels["width"].as_u64(), pixels["height"].as_u64()),
            (Some(480), Some(320))
        );
        assert_capture_color(&pixels["top"], [20, 240, 20]);
        assert_capture_color(&pixels["bottom"], [20, 240, 20]);

        let crop = page
            .screenshot_with_options(&ScreenshotOptions {
                clip: Some(ScreenshotClip {
                    x: 250.0,
                    y: 20.0,
                    width: 40.0,
                    height: 80.0,
                }),
                ..base
            })
            .await
            .unwrap();
        assert_eq!(page.evaluate::<Value>(state).await.unwrap(), before);
        let pixels = screenshot_pixels(&page, &crop, mime_type).await;
        assert_eq!(
            (pixels["width"].as_u64(), pixels["height"].as_u64()),
            (Some(80), Some(160))
        );
        assert_capture_color(&pixels["top"], [20, 20, 240]);
        assert_capture_color(&pixels["bottom"], [20, 20, 240]);

        let full = page
            .screenshot_with_options(&ScreenshotOptions {
                full_page: true,
                ..base
            })
            .await
            .unwrap();
        let pixels = screenshot_pixels(&page, &full, mime_type).await;
        assert_eq!(
            (pixels["width"].as_u64(), pixels["height"].as_u64()),
            (Some(600), Some(1200))
        );
        assert_capture_color(&pixels["top"], [20, 20, 240]);
        assert_capture_color(&pixels["bottom"], [20, 240, 20]);
        assert_eq!(page.evaluate::<Value>(state).await.unwrap(), before);
    }
    page.set_viewport(240, 160, 1.5).await.unwrap();
    let before: Value = page.evaluate(state).await.unwrap();
    let crop = page
        .screenshot_with_options(&ScreenshotOptions {
            clip: Some(ScreenshotClip {
                x: 10.0,
                y: 20.0,
                width: 80.0,
                height: 40.0,
            }),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(page.evaluate::<Value>(state).await.unwrap(), before);
    let pixels = screenshot_pixels(&page, &crop, "image/png").await;
    assert_eq!(
        (pixels["width"].as_u64(), pixels["height"].as_u64()),
        (Some(120), Some(60))
    );
    assert_capture_color(&pixels["top"], [20, 20, 240]);
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Chrome; invalid capture options fail locally even after disconnection"]
async fn invalid_capture_options_preserve_metrics_and_validate_before_connection() {
    let browser = Browser::launch_with(|config| {
        config.patch_binary = false;
    })
    .await
    .unwrap();
    let page = browser.new_page(FIXTURE).await.unwrap();
    let before = metrics(&page).await;
    let invalid = [
        ScreenshotOptions {
            quality: Some(0),
            ..Default::default()
        },
        ScreenshotOptions {
            format: ScreenshotFormat::Jpeg,
            quality: Some(101),
            ..Default::default()
        },
        ScreenshotOptions {
            clip: Some(ScreenshotClip {
                x: f64::NAN,
                y: 0.0,
                width: 10.0,
                height: 10.0,
            }),
            ..Default::default()
        },
        ScreenshotOptions {
            clip: Some(ScreenshotClip {
                x: 0.0,
                y: -1.0,
                width: 10.0,
                height: 10.0,
            }),
            ..Default::default()
        },
        ScreenshotOptions {
            clip: Some(ScreenshotClip {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 10.0,
            }),
            ..Default::default()
        },
        ScreenshotOptions {
            clip: Some(ScreenshotClip {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: f64::INFINITY,
            }),
            ..Default::default()
        },
        ScreenshotOptions {
            full_page: true,
            clip: Some(ScreenshotClip {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
            }),
            ..Default::default()
        },
    ];
    for options in &invalid {
        assert!(matches!(
            page.screenshot_with_options(options).await,
            Err(Error::InvalidScreenshotOptions(_))
        ));
        assert_eq!(metrics(&page).await, before);
    }
    assert!(matches!(
        page.screenshot_jpeg(101).await,
        Err(Error::InvalidScreenshotOptions(_))
    ));
    browser.close().await.unwrap();
    for options in &invalid {
        assert!(matches!(
            page.screenshot_with_options(options).await,
            Err(Error::InvalidScreenshotOptions(_))
        ));
    }
}
