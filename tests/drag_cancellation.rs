use std::time::Duration;

use eoka::{Browser, HumanSpeed, Page};
use serde_json::Value;

async fn page(browser: &Browser) -> Page {
    let page = browser.new_page("about:blank").await.unwrap();
    page.execute_sync(r#"document.body.innerHTML='<button style="position:absolute;left:100px;top:100px;width:300px;height:100px">target</button>'; window.events=[]; for(const t of ['mousedown','mouseup','mousemove']) document.addEventListener(t,e=>events.push({t,b:e.buttons}),true)"#).await.unwrap();
    page
}

async fn wait_for_press(page: &Page) {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if page
                .evaluate_sync::<bool>("events.some(e=>e.t==='mousedown')")
                .await
                .unwrap()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "requires local Chrome"]
async fn cancelled_drags_release_before_next_interaction() {
    let browser = Browser::launch().await.unwrap();
    let page = page(&browser).await;
    for horizontal in [true, false] {
        page.execute_sync("events=[]").await.unwrap();
        let human = page.human().with_speed(HumanSpeed::Slow);
        {
            let drag = async {
                if horizontal {
                    human.drag_horizontal_by(120.0, 120.0, 150.0).await
                } else {
                    human.drag_by(120.0, 120.0, 150.0).await
                }
            };
            tokio::pin!(drag);
            tokio::select! {
                _ = &mut drag => panic!("drag finished before cancellation"),
                _ = wait_for_press(&page) => {}
            }
        }
        human.finish_drag_cleanup().await.unwrap();
        let events: Vec<Value> = page.evaluate_sync("events").await.unwrap();
        assert_eq!(events.iter().filter(|e| e["t"] == "mouseup").count(), 1);
        assert_eq!(events.last().unwrap()["b"], 0);
        page.click_at(130.0, 130.0).await.unwrap();
        let count: u64 = page
            .evaluate_sync("events.filter(e=>e.t==='mouseup').length")
            .await
            .unwrap();
        assert_eq!(count, 2);
    }
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local Chrome"]
async fn cancellation_before_press_does_not_release_and_lost_target_is_reported() {
    let browser = Browser::launch().await.unwrap();
    let page = page(&browser).await;
    for horizontal in [true, false] {
        let human = page.human();
        assert!(tokio::time::timeout(Duration::from_millis(1), async {
            if horizontal {
                human.drag_horizontal_by(120.0, 120.0, 150.0).await
            } else {
                human.drag_by(120.0, 120.0, 150.0).await
            }
        })
        .await
        .is_err());
        human.finish_drag_cleanup().await.unwrap();
        let pressed: bool = page
            .evaluate_sync("events.some(e=>e.t!=='mousemove')")
            .await
            .unwrap();
        assert!(!pressed);
    }
    let human = page.human().with_speed(HumanSpeed::Slow);
    {
        let drag = human.drag_horizontal_by(120.0, 120.0, 150.0);
        tokio::pin!(drag);
        tokio::select! {
            _ = &mut drag => panic!("drag ended too early"),
            _ = wait_for_press(&page) => {}
        }
        browser.close_tab(page.target_id()).await.unwrap();
    }
    assert!(
        tokio::time::timeout(Duration::from_secs(4), human.finish_drag_cleanup())
            .await
            .unwrap()
            .is_err()
    );
    browser.new_page("about:blank").await.unwrap();
    browser.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires local Chrome"]
async fn task_abort_schedules_release_even_when_human_is_dropped() {
    let browser = Browser::launch().await.unwrap();
    let page = page(&browser).await;
    for horizontal in [true, false] {
        page.execute_sync("events=[]").await.unwrap();
        let owned_page = page.clone();
        let task = tokio::spawn(async move {
            let human = owned_page.human().with_speed(HumanSpeed::Slow);
            if horizontal {
                human.drag_horizontal_by(120.0, 120.0, 150.0).await
            } else {
                human.drag_by(120.0, 120.0, 150.0).await
            }
        });
        wait_for_press(&page).await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                if page
                    .evaluate_sync::<bool>("events.some(e=>e.t==='mouseup'&&e.b===0)")
                    .await
                    .unwrap()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    browser.close().await.unwrap();
}
