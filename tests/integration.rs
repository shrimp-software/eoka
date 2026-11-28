//! Chrome integration scenarios, built as one test binary so CI links once.
//! Run with `cargo test --test integration -- --ignored --test-threads=1`;
//! select a file with its module path, e.g. `-- --ignored page_coverage::`.

mod support;

#[path = "integration/actions.rs"]
mod actions;
#[path = "integration/binary_post.rs"]
mod binary_post;
#[path = "integration/canvas_consistency.rs"]
mod canvas_consistency;
#[path = "integration/cdp_lifecycle.rs"]
mod cdp_lifecycle;
#[path = "integration/convenience_actions.rs"]
mod convenience_actions;
#[path = "integration/cookie_state.rs"]
mod cookie_state;
#[path = "integration/document.rs"]
mod document;
#[path = "integration/drag.rs"]
mod drag;
#[path = "integration/drag_cancellation.rs"]
mod drag_cancellation;
#[path = "integration/drag_coordination.rs"]
mod drag_coordination;
#[path = "integration/exploration.rs"]
mod exploration;
#[path = "integration/forms.rs"]
mod forms;
#[path = "integration/frame_response_capture.rs"]
mod frame_response_capture;
#[path = "integration/held_input.rs"]
mod held_input;
#[path = "integration/input.rs"]
mod input;
#[path = "integration/keyboard_text.rs"]
mod keyboard_text;
#[path = "integration/lifecycle.rs"]
mod lifecycle;
#[path = "integration/native_debug_port.rs"]
mod native_debug_port;
#[path = "integration/nested_frames.rs"]
mod nested_frames;
#[path = "integration/network_idle.rs"]
mod network_idle;
#[path = "integration/owned_shutdown.rs"]
mod owned_shutdown;
#[path = "integration/page_coverage.rs"]
mod page_coverage;
#[path = "integration/patcher.rs"]
mod patcher;
#[path = "integration/proxy_auth.rs"]
mod proxy_auth;
#[path = "integration/reliability.rs"]
mod reliability;
#[path = "integration/request_capture.rs"]
mod request_capture;
#[path = "integration/runtime_launch.rs"]
mod runtime_launch;
#[path = "integration/shadow_click.rs"]
mod shadow_click;
#[path = "integration/stealth.rs"]
mod stealth;
#[path = "integration/temporary_headers.rs"]
mod temporary_headers;
#[path = "integration/viewport.rs"]
mod viewport;
