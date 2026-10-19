mod support;

use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine};
use eoka::{Error, Page};

fn data_url(html: &str) -> String {
    format!("data:text/html;base64,{}", STANDARD.encode(html))
}

async fn wait_for_title(page: &Page, expected: &str, attempts: usize) -> String {
    let mut title = String::new();
    for _ in 0..attempts {
        title = page.title().await.unwrap();
        if title == expected {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    title
}

#[tokio::test]
#[ignore = "browser"]
async fn clicks_inside_nested_closed_shadow_roots() {
    let browser = support::browser().await;
    let page = browser
        .new_page(&data_url(
            r#"<!doctype html><title>start</title><body>
<div id="host"></div>
<script>
  const outer = document.getElementById('host').attachShadow({mode:'closed'});
  outer.innerHTML = '<div id="inner-host"></div>';
  const inner = outer.getElementById('inner-host').attachShadow({mode:'closed'});
  inner.innerHTML = '<button id="deep" style="width:120px;height:40px">go</button>';
  inner.getElementById('deep').addEventListener('click', () => { document.title = 'clicked'; });
</script></body>"#,
        ))
        .await
        .unwrap();
    let receipt = page.click_shadow("button#deep").await.unwrap();
    assert_eq!(receipt.selector, "button#deep");
    assert!(receipt.x.is_finite() && receipt.y.is_finite());
    assert_eq!(wait_for_title(&page, "clicked", 20).await, "clicked");
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "browser"]
async fn clicks_inside_closed_shadow_root_of_child_frame() {
    let inner = data_url(
        r#"<!doctype html><body style="margin:0">
<div id="host"></div>
<script>
  const root = document.getElementById('host').attachShadow({mode:'closed'});
  root.innerHTML = '<button id="inner" style="width:40px;height:20px">go</button>';
  root.getElementById('inner').addEventListener('click', () => { parent.postMessage('clicked', '*'); });
</script></body>"#,
    );
    // The parent has a matching button and the iframe is offset. A search that
    // is not scoped to the child frame clicks the parent button; a box model
    // that is mapped twice misses the child button entirely.
    let outer = format!(
        r#"<!doctype html><title>start</title><body style="margin:0">
<button id="inner" style="position:absolute;left:0;top:0;width:120px;height:40px"
  onclick="document.title='parent-clicked'">parent</button>
<iframe src="{inner}" style="position:absolute;left:300px;top:200px;width:200px;height:100px;border:0"></iframe>
<script>
  addEventListener('message', event => {{ if (event.data === 'clicked') document.title = 'frame-clicked'; }});
</script></body>"#
    );
    let browser = support::browser().await;
    let page = browser.new_page(&data_url(&outer)).await.unwrap();
    let frames = page.frames().await.unwrap();
    let top_id = frames.first().expect("top frame").id.clone();
    let frame = frames
        .into_iter()
        .find(|frame| frame.id != top_id && frame.url.starts_with("data:text/html"))
        .expect("child frame");
    let receipt = page
        .click_shadow_in_frame(&frame.id, "button#inner")
        .await
        .unwrap();
    assert_eq!(receipt.frame_id, frame.id);
    assert_eq!(
        wait_for_title(&page, "frame-clicked", 20).await,
        "frame-clicked"
    );
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "browser"]
async fn missing_shadow_selector_times_out() {
    let browser = support::browser().await;
    let page = browser
        .new_page(&data_url(
            "<!doctype html><title>start</title><body><p>no shadow</p></body>",
        ))
        .await
        .unwrap();
    let error = page
        .click_shadow_with_timeout(None, "#missing", Duration::from_millis(300))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Timeout(_)), "got {error:?}");
    browser.close().await.unwrap();
}
