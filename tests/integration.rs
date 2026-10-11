//! Chrome integration scenarios. Run with `cargo test --test integration -- --ignored`.

mod support;

#[path = "integration/document.rs"]
mod document;
#[path = "integration/forms.rs"]
mod forms;
#[path = "integration/held_input.rs"]
mod held_input;
#[path = "integration/input.rs"]
mod input;
#[path = "integration/lifecycle.rs"]
mod lifecycle;
