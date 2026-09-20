use eoka::{Browser, Page};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Fixture {
    url: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Fixture {
    async fn start(cross_site: bool) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let middle_host = if cross_site { "b.test" } else { "a.test" };
        let inner_host = if cross_site { "c.test" } else { "a.test" };
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut data = [0; 8192];
                    let n = socket.read(&mut data).await.unwrap_or(0);
                    let request = String::from_utf8_lossy(&data[..n]);
                    let path = request.split_whitespace().nth(1).unwrap_or("/");
                    let html = match path {
                        "/" => format!(r#"<style>body{{margin:0}} iframe{{position:absolute;left:60px;top:70px;width:500px;height:400px;border:5px solid black;padding:2px;}}</style><div id="open-host"><div id="closed-host"><iframe id="outer" src="http://{middle_host}:{port}/middle"></iframe></div></div>"#),
                        "/middle" => format!(r#"<style>body{{margin:0}} iframe{{position:absolute;left:30px;top:40px;width:320px;height:200px;border:3px solid black;padding:4px;}}</style><iframe src="http://{inner_host}:{port}/inner"></iframe>"#),
                        _ => r#"<style>body{margin:0}button{position:absolute;left:40px;top:40px;width:80px;height:60px}</style><button id="target">click</button><script>document.querySelector('button').onclick=e=>{document.body.dataset.clicked=JSON.stringify([e.clientX,e.clientY])}</script>"#.to_string(),
                    };
                    let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", html.len(), html);
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
        Self {
            url: format!("http://a.test:{port}/"),
            task,
        }
    }
}

#[tokio::test]
#[ignore = "requires Chrome; uses only disposable local fixtures"]
async fn nested_frame_routing_ancestry_native_quads_and_stale_ids() {
    for isolated in [false, true] {
        let fixture = Fixture::start(isolated).await;
        let browser = Browser::launch_with(|config| {
            config.extra_args.extend([
                "--host-resolver-rules=MAP *.test 127.0.0.1".into(),
                "--no-proxy-server".into(),
            ]);
            if isolated {
                config.extra_args.extend([
                    "--disable-features=AutomationControlled,EnableAutomation".into(),
                    "--site-per-process".into(),
                ]);
            }
        })
        .await
        .unwrap();
        let page = browser.new_page(&fixture.url).await.unwrap();
        let inner = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if let Some(inner) = page
                    .frames()
                    .await
                    .unwrap()
                    .into_iter()
                    .find(|f| f.url.ends_with("/inner"))
                {
                    let ready: bool = page
                        .evaluate_in_frame_id(&inner.id, "!!document.querySelector('#target')")
                        .await
                        .unwrap();
                    if ready {
                        break inner;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("nested frame ready");
        let ancestry = page.frame_ancestor_ids(&inner.id).await.unwrap();
        assert_eq!(ancestry.len(), 2);
        assert!(page
            .frame_ancestor_ids(&ancestry[1])
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            page.frame_ancestor_ids(&ancestry[0]).await.unwrap(),
            vec![ancestry[1].clone()]
        );
        assert!(page.frame_ancestor_ids("unrelated-id").await.is_err());
        let targets: Value = page
            .session()
            .transport()
            .send("Target.getTargets", &json!({}))
            .await
            .unwrap();
        let oopifs = targets["targetInfos"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| t["type"] == "iframe")
            .count();
        if isolated {
            assert!(oopifs >= 2, "fixture must exercise OOPIF routing");
        } else {
            assert_eq!(oopifs, 0, "fixture must exercise same-process routing");
        }
        let host: String = page
            .evaluate_in_frame_id(&inner.id, "location.hostname")
            .await
            .unwrap();
        assert_eq!(host, if isolated { "c.test" } else { "a.test" });
        let quad = page
            .frame_element_content_quad(&inner.id, "#target", 0)
            .await
            .unwrap();
        assert!(quad[2] > quad[0] && quad[5] > quad[3]);
        assert!(page
            .frame_element_content_quad(&inner.id, "#missing", 0)
            .await
            .is_err());
        assert!(page
            .frame_element_content_quad(&inner.id, "#target", 64)
            .await
            .is_err());
        assert!(page
            .frame_element_content_quad(&inner.id, &"x".repeat(4097), 0)
            .await
            .is_err());
        for mode in ["open", "closed"] {
            let script = format!(
                r#"(() => {{
                const element=document.querySelector('#target');
                const host=document.body.appendChild(document.createElement('div'));
                const root=host.attachShadow({{mode:'{mode}'}});
                root.innerHTML='<div style="transform:scaleX(-1)"><slot></slot></div>';
                host.appendChild(element);
                return true;
            }})()"#
            );
            page.evaluate_in_frame_id::<bool>(&inner.id, &script)
                .await
                .unwrap();
            let reflected = page
                .frame_element_content_quad(&inner.id, "#target", 0)
                .await
                .unwrap();
            assert!(
                reflected[2] < reflected[0],
                "{mode} shadow reflection: {reflected:?}"
            );
            page.evaluate_in_frame_id::<bool>(
                &inner.id,
                "(document.body.appendChild(document.querySelector('#target')),true)",
            )
            .await
            .unwrap();
            let supported = page
                .frame_element_content_quad(&inner.id, "#target", 0)
                .await
                .unwrap();
            assert!(supported[2] > supported[0]);
        }
        page.evaluate_in_frame_id::<bool>(
            &inner.id,
            "(document.querySelector('#target').style.display='none',true)",
        )
        .await
        .unwrap();
        assert!(page
            .frame_element_content_quad(&inner.id, "#target", 0)
            .await
            .is_err());
        page.evaluate_in_frame_id::<bool>(
            &inner.id,
            "(document.querySelector('#target').style.display='',true)",
        )
        .await
        .unwrap();
        page.evaluate_in_frame_id::<bool>(&inner.id, "(() => { for(let i=0;i<65;i++)document.body.appendChild(document.createElement('span'));return true; })()").await.unwrap();
        assert!(page
            .frame_element_content_quad(&inner.id, "span", 0)
            .await
            .is_err());
        let null: Option<String> = page.evaluate_in_frame_id(&inner.id, "null").await.unwrap();
        assert_eq!(null, None);
        let null: Option<String> = page.evaluate_sync("null").await.unwrap();
        assert_eq!(null, None);
        assert!(page
            .evaluate_in_frame_id::<Value>(&inner.id, "undefined")
            .await
            .is_err());
        assert_coordinates(&page, &inner.id).await;
        let other = browser.new_page("about:blank").await.unwrap();
        assert!(other
            .frame_point_to_viewport(&inner.id, 60.0, 60.0)
            .await
            .is_err());
        assert!(other
            .frame_point_for_input(&inner.id, 60.0, 60.0)
            .await
            .is_err());
        assert!(other.frame_ancestor_ids(&inner.id).await.is_err());
        assert!(other
            .frame_element_content_quad(&inner.id, "#target", 0)
            .await
            .is_err());
        assert!(other
            .evaluate_in_frame_id::<Value>(&inner.id, "1")
            .await
            .is_err());
        page.evaluate_sync::<bool>("(document.querySelector('iframe').remove(), true)")
            .await
            .unwrap();
        assert!(page
            .evaluate_in_frame_id::<Value>(&inner.id, "1")
            .await
            .is_err());
        assert!(page.frame_ancestor_ids(&inner.id).await.is_err());
        assert!(page
            .frame_element_content_quad(&inner.id, "#target", 0)
            .await
            .is_err());
        assert!(page
            .frame_point_to_viewport(&inner.id, 60.0, 60.0)
            .await
            .is_err());
        assert!(page
            .frame_point_for_input(&inner.id, 60.0, 60.0)
            .await
            .is_err());
        browser.close().await.unwrap();
    }
}

const PAINT: &str =
    "new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(()=>resolve(true))))";

async fn assert_coordinates(page: &Page, frame_id: &str) {
    page.evaluate_in_frame_id::<bool>(frame_id, PAINT)
        .await
        .unwrap();
    let (x, y) = page
        .frame_point_to_viewport(frame_id, 60.0, 60.0)
        .await
        .unwrap();
    assert!((x - 164.0).abs() < 1.0, "x={x}");
    assert!((y - 184.0).abs() < 1.0, "y={y}");
    assert_eq!(
        page.frame_point_for_input(frame_id, 60.0, 60.0)
            .await
            .unwrap(),
        (x, y)
    );
    for ancestor in page.frame_ancestor_ids(frame_id).await.unwrap() {
        for tag in ["div", "iframe"] {
            page.evaluate_in_frame_id::<bool>(&ancestor, &format!("(() => {{const cover=document.createElement('{tag}');cover.id='input-cover';cover.style='position:fixed;inset:0;width:100%;height:100%;border:0;z-index:10000';document.body.appendChild(cover);return true}})()")).await.unwrap();
            assert!(
                page.frame_point_for_input(frame_id, 60.0, 60.0)
                    .await
                    .is_err(),
                "{tag} overlay in {ancestor} accepted"
            );
            assert_eq!(
                page.frame_point_to_viewport(frame_id, 60.0, 60.0)
                    .await
                    .unwrap(),
                (x, y)
            );
            page.evaluate_in_frame_id::<bool>(
                &ancestor,
                "(document.querySelector('#input-cover').remove(),true)",
            )
            .await
            .unwrap();
        }
    }
    assert_eq!(
        page.frame_point_for_input(frame_id, 60.0, 60.0)
            .await
            .unwrap(),
        (x, y)
    );
    page.click_at(x, y).await.unwrap();
    let clicked: [f64; 2] = page
        .evaluate_in_frame_id(frame_id, "JSON.parse(document.body.dataset.clicked)")
        .await
        .expect("click reached inner button");
    assert!((clicked[0] - 60.0).abs() < 1.0);
    assert!((clicked[1] - 60.0).abs() < 1.0);
    let _: bool = page.evaluate_sync("(() => { const e=document.querySelector('iframe'); e.style.transformOrigin='0 0'; e.style.transform='translate(15px,20px) scale(0.8)'; return true; })()").await.unwrap();
    let scaled = page
        .frame_point_to_viewport(frame_id, 60.0, 60.0)
        .await
        .unwrap();
    assert!((scaled.0 - 158.2).abs() < 1.0, "scaled={scaled:?}");
    assert!((scaled.1 - 181.2).abs() < 1.0, "scaled={scaled:?}");
    assert_eq!(
        page.frame_point_for_input(frame_id, 60.0, 60.0)
            .await
            .unwrap(),
        scaled
    );
    page.evaluate_in_frame_id::<bool>(frame_id, "(delete document.body.dataset.clicked, true)")
        .await
        .unwrap();
    page.evaluate_in_frame_id::<bool>(frame_id, PAINT)
        .await
        .unwrap();
    page.click_at(scaled.0, scaled.1).await.unwrap();
    let clicked: [f64; 2] = page
        .evaluate_in_frame_id(frame_id, "JSON.parse(document.body.dataset.clicked)")
        .await
        .expect("scaled click reached inner button");
    assert!((clicked[0] - 60.0).abs() <= 2.0 && (clicked[1] - 60.0).abs() <= 2.0);
    let _: bool = page
        .evaluate_sync("(document.querySelector('iframe').style.transform='none', true)")
        .await
        .unwrap();
    assert!(page
        .frame_point_to_viewport(frame_id, -1.0, 0.0)
        .await
        .is_err());
    let middle = page
        .frames()
        .await
        .unwrap()
        .into_iter()
        .find(|f| f.url.ends_with("/middle"))
        .unwrap();
    let _: bool = page
        .evaluate_in_frame_id(
            &middle.id,
            "(document.body.style.height='1000px', scrollTo(0, 15), true)",
        )
        .await
        .unwrap();
    let (_, scrolled_y) = page
        .frame_point_to_viewport(frame_id, 60.0, 60.0)
        .await
        .unwrap();
    assert!((scrolled_y - 169.0).abs() < 1.0, "scrolled_y={scrolled_y}");
    assert_eq!(
        page.frame_point_for_input(frame_id, 60.0, 60.0)
            .await
            .unwrap()
            .1,
        scrolled_y
    );
    let _: bool = page
        .evaluate_sync("(document.querySelector('iframe').style.display='none', true)")
        .await
        .unwrap();
    assert!(page
        .frame_point_to_viewport(frame_id, 60.0, 60.0)
        .await
        .is_err());
    let _: bool = page
        .evaluate_sync("(document.querySelector('iframe').style.display='block', true)")
        .await
        .unwrap();
    let _: Value = page
        .evaluate_sync("document.querySelector('iframe').style.transform='rotate(15deg)'")
        .await
        .unwrap();
    assert!(page
        .frame_point_to_viewport(frame_id, 60.0, 60.0)
        .await
        .is_err());
    page.evaluate_sync::<bool>("(document.querySelector('iframe').style.transform='none',true)")
        .await
        .unwrap();
    for mode in ["open", "closed"] {
        page.evaluate_sync::<bool>(&format!("(() => {{ const host=document.getElementById('{mode}-host'); window.fixtureRoot=host.attachShadow({{mode:'{mode}'}}); fixtureRoot.innerHTML='<div style=\"transform:scaleX(-1)\"><slot></slot></div>'; return true; }})()")).await.unwrap();
        assert_eq!(
            page.evaluate_in_frame_id::<i32>(frame_id, "1")
                .await
                .unwrap(),
            1
        );
        assert!(
            page.frame_point_to_viewport(frame_id, 60.0, 60.0)
                .await
                .is_err(),
            "{mode} shadow reflection accepted"
        );
        page.evaluate_sync::<bool>("(fixtureRoot.firstElementChild.style.transform='none',true)")
            .await
            .unwrap();
        page.evaluate::<bool>(PAINT).await.unwrap();
        page.frame_point_to_viewport(frame_id, 60.0, 60.0)
            .await
            .unwrap_or_else(|e| panic!("{mode} positive shadow control: {e}"));
        page.evaluate_sync::<bool>("(fixtureRoot.firstElementChild.style.opacity='0',true)")
            .await
            .unwrap();
        assert!(
            page.frame_point_to_viewport(frame_id, 60.0, 60.0)
                .await
                .is_err(),
            "{mode} hidden slot accepted"
        );
        page.evaluate_sync::<bool>("(fixtureRoot.firstElementChild.style.opacity='1',true)")
            .await
            .unwrap();
    }
}
