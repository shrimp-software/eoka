use eoka::{Browser, HumanSpeed, MouseButton};
use serde_json::Value;

#[tokio::test]
#[ignore = "requires Chrome; disposable local fixture"]
async fn drags_preserve_other_buttons_and_do_not_release_an_existing_left_press() {
    let browser = Browser::launch().await.unwrap();
    let page = browser.new_page("about:blank").await.unwrap();
    page.execute_sync("document.body.innerHTML='<div style=\"width:1000px;height:600px\"></div>'; window.events=[]; for(const type of ['mousedown','mouseup','mousemove'])document.addEventListener(type,e=>events.push({type,buttons:e.buttons}),true)").await.unwrap();
    for horizontal in [true, false] {
        page.mouse_down(100.0, 100.0, MouseButton::Right)
            .await
            .unwrap();
        page.execute_sync("events=[]").await.unwrap();
        let human = page.human().with_speed(HumanSpeed::Fast);
        if horizontal {
            human.drag_horizontal_by(120.0, 120.0, 80.0).await.unwrap();
        } else {
            human.drag_by(120.0, 120.0, 80.0).await.unwrap();
        }
        human.finish_drag_cleanup().await.unwrap();
        let events: Vec<Value> = page.evaluate_sync("events").await.unwrap();
        assert!(events
            .iter()
            .any(|e| e["type"] == "mousedown" && e["buttons"] == 3));
        assert_eq!(events.last().unwrap()["buttons"], 2);
        page.mouse_up(200.0, 120.0, MouseButton::Right)
            .await
            .unwrap();
        page.mouse_down(120.0, 120.0, MouseButton::Left)
            .await
            .unwrap();
        page.execute_sync("events=[]").await.unwrap();
        let result = if horizontal {
            human.drag_horizontal_by(120.0, 120.0, 80.0).await
        } else {
            human.drag_by(120.0, 120.0, 80.0).await
        };
        assert!(matches!(result, Err(eoka::Error::InputState(_))));
        human.finish_drag_cleanup().await.unwrap();
        assert_eq!(
            page.evaluate_sync::<usize>("events.length").await.unwrap(),
            0
        );
        page.mouse_move(130.0, 120.0).await.unwrap();
        assert_eq!(
            page.evaluate_sync::<u32>("events.at(-1).buttons")
                .await
                .unwrap(),
            1
        );
        page.mouse_up(130.0, 120.0, MouseButton::Left)
            .await
            .unwrap();
    }
    browser.close().await.unwrap();
}
