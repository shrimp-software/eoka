mod support;

use serde_json::{json, Value};

#[tokio::test]
#[ignore = "requires Chrome; disposable canvas document"]
async fn pixel_readback_matches_both_png_exports_without_mutating_the_source() {
    for spoof in [true, false] {
        let browser = eoka::Browser::launch_with(|c| {
            c.patch_binary = false;
            c.webgl_spoof = spoof;
            c.canvas_spoof = spoof;
            c.audio_spoof = spoof;
        })
        .await
        .unwrap();
        let page = browser
            .new_page("data:text/html,<body>canvas fixture</body>")
            .await
            .unwrap();
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
                    "frameId":tree["frameTree"]["frame"]["id"],"worldName":"canvas-fixture"
                }),
            )
            .await
            .unwrap();
        for (width, height) in [(4, 1), (13, 7)] {
            let expression = format!(
                r#"(async () => {{
                const c = document.createElement('canvas');
                c.id = 'fixture'; c.width = {width}; c.height = {height};
                document.body.replaceChildren(c);
                const ctx = c.getContext('2d');
                ctx.fillStyle = 'rgb(100,50,25)'; ctx.fillRect(0,0,c.width,c.height);
                if (c.height > 1) {{
                    ctx.clearRect(0,0,1,1);
                    ctx.clearRect(1,0,1,1);
                    ctx.fillStyle = 'rgba(100,50,25,0.5)'; ctx.fillRect(1,0,1,1);
                }}
                const before = Array.from(ctx.getImageData(0,0,c.width,c.height).data);
                const url = c.toDataURL();
                const blob = await new Promise(resolve => c.toBlob(resolve));
                const blobURL = await new Promise(resolve => {{const r = new FileReader();r.onload=()=>resolve(r.result);r.readAsDataURL(blob);}});
                const after = Array.from(ctx.getImageData(0,0,c.width,c.height).data);
                let error;
                try {{ ctx.getImageData(0,0,0,1); }} catch(e) {{ error = e.name; }}
                return {{before, after, url, blobURL, error}};
            }})()"#
            );
            let output: Value = page.evaluate(&expression).await.unwrap();
            assert_eq!(output["before"], output["after"]);
            assert_eq!(output["error"], "IndexSizeError");
            let expression = format!(
                r#"(async () => {{
                const source = document.querySelector('#fixture');
                const original = Array.from(source.getContext('2d').getImageData(0,0,source.width,source.height).data);
                const decode = async url => {{
                    const img = new Image(); img.src = url; await img.decode();
                    const c = document.createElement('canvas'); c.width=source.width;c.height=source.height;
                    const ctx=c.getContext('2d'); ctx.drawImage(img,0,0);
                    return Array.from(ctx.getImageData(0,0,c.width,c.height).data);
                }};
                return {{original, url:await decode({}), blob:await decode({})}};
            }})()"#,
                output["url"], output["blobURL"]
            );
            let native: Value = page.session().send("Runtime.evaluate", &json!({
                "contextId":world["executionContextId"],"expression":expression,"awaitPromise":true,"returnByValue":true
            })).await.unwrap();
            let native = &native["result"]["value"];
            assert_eq!(output["before"], native["url"], "toDataURL, spoof={spoof}");
            assert_eq!(output["before"], native["blob"], "toBlob, spoof={spoof}");
            if spoof {
                assert_ne!(native["original"], output["before"]);
            } else {
                assert_eq!(native["original"], output["before"]);
            }
            let original = native["original"].as_array().unwrap();
            for pixel in original.chunks_exact(4).skip(width as usize) {
                assert_eq!(
                    pixel,
                    &[json!(100), json!(50), json!(25), json!(255)],
                    "export mutated the original canvas"
                );
            }
        }
        browser.close().await.unwrap();
    }
}
