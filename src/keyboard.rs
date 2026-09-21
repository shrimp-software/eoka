//! Keyboard key parsing
//!
//! Combo parsing and key-to-code lookup used by `Page::press_key`. Split out of `page.rs`.

pub(crate) fn parse_key_combo(combo: &str) -> (i32, &str) {
    use crate::cdp::types::modifiers;
    let parts: Vec<&str> = combo.split('+').collect();
    let mut mods = 0;
    let mut key = combo;
    for (i, part) in parts.iter().enumerate() {
        match part.to_lowercase().as_str() {
            "ctrl" | "control" => mods |= modifiers::CTRL,
            "alt" | "option" => mods |= modifiers::ALT,
            "shift" => mods |= modifiers::SHIFT,
            "cmd" | "meta" | "command" => mods |= modifiers::META,
            _ => key = parts[i],
        }
    }
    if key.is_empty() {
        key = "+";
    }
    (mods, key)
}

const PRINTABLE_KEYS: &[(&str, &str, &str, i32)] = &[
    ("0", ")", "Digit0", 48),
    ("1", "!", "Digit1", 49),
    ("2", "@", "Digit2", 50),
    ("3", "#", "Digit3", 51),
    ("4", "$", "Digit4", 52),
    ("5", "%", "Digit5", 53),
    ("6", "^", "Digit6", 54),
    ("7", "&", "Digit7", 55),
    ("8", "*", "Digit8", 56),
    ("9", "(", "Digit9", 57),
    (";", ":", "Semicolon", 186),
    ("=", "+", "Equal", 187),
    (",", "<", "Comma", 188),
    ("-", "_", "Minus", 189),
    (".", ">", "Period", 190),
    ("/", "?", "Slash", 191),
    ("`", "~", "Backquote", 192),
    ("[", "{", "BracketLeft", 219),
    ("\\", "|", "Backslash", 220),
    ("]", "}", "BracketRight", 221),
    ("'", "\"", "Quote", 222),
    (" ", " ", "Space", 32),
];

pub(crate) fn key_for_modifiers(key: &str, modifiers: i32) -> String {
    if modifiers & crate::cdp::modifiers::SHIFT == 0 {
        return key.to_owned();
    }
    if key.len() == 1 && key.as_bytes()[0].is_ascii_alphabetic() {
        return key.to_ascii_uppercase();
    }
    PRINTABLE_KEYS
        .iter()
        .find(|(base, _, _, _)| *base == key)
        .map_or(key, |(_, shifted, _, _)| *shifted)
        .to_owned()
}

pub(crate) fn key_text(key: &str) -> Option<&str> {
    if key == "Enter" {
        return Some("\r");
    }
    let mut chars = key.chars();
    let ch = chars.next()?;
    (!ch.is_control() && chars.next().is_none()).then_some(key)
}

pub(crate) fn key_to_codes(key: &str) -> (&str, &str, Option<i32>) {
    if let Some((_, _, code, vk)) = PRINTABLE_KEYS
        .iter()
        .find(|(base, shifted, _, _)| *base == key || *shifted == key)
    {
        return (key, code, Some(*vk));
    }
    static KEYS: &[(&str, &str, &str, i32)] = &[
        ("ctrl", "Control", "ControlLeft", 17),
        ("control", "Control", "ControlLeft", 17),
        ("alt", "Alt", "AltLeft", 18),
        ("option", "Alt", "AltLeft", 18),
        ("shift", "Shift", "ShiftLeft", 16),
        ("cmd", "Meta", "MetaLeft", 91),
        ("meta", "Meta", "MetaLeft", 91),
        ("command", "Meta", "MetaLeft", 91),
        ("enter", "Enter", "Enter", 13),
        ("return", "Enter", "Enter", 13),
        ("tab", "Tab", "Tab", 9),
        ("escape", "Escape", "Escape", 27),
        ("esc", "Escape", "Escape", 27),
        ("backspace", "Backspace", "Backspace", 8),
        ("delete", "Delete", "Delete", 46),
        ("arrowup", "ArrowUp", "ArrowUp", 38),
        ("up", "ArrowUp", "ArrowUp", 38),
        ("arrowdown", "ArrowDown", "ArrowDown", 40),
        ("down", "ArrowDown", "ArrowDown", 40),
        ("arrowleft", "ArrowLeft", "ArrowLeft", 37),
        ("left", "ArrowLeft", "ArrowLeft", 37),
        ("arrowright", "ArrowRight", "ArrowRight", 39),
        ("right", "ArrowRight", "ArrowRight", 39),
        ("home", "Home", "Home", 36),
        ("end", "End", "End", 35),
        ("pageup", "PageUp", "PageUp", 33),
        ("pagedown", "PageDown", "PageDown", 34),
        ("space", " ", "Space", 32),
        ("a", "a", "KeyA", 65),
        ("b", "b", "KeyB", 66),
        ("c", "c", "KeyC", 67),
        ("d", "d", "KeyD", 68),
        ("e", "e", "KeyE", 69),
        ("f", "f", "KeyF", 70),
        ("g", "g", "KeyG", 71),
        ("h", "h", "KeyH", 72),
        ("i", "i", "KeyI", 73),
        ("j", "j", "KeyJ", 74),
        ("k", "k", "KeyK", 75),
        ("l", "l", "KeyL", 76),
        ("m", "m", "KeyM", 77),
        ("n", "n", "KeyN", 78),
        ("o", "o", "KeyO", 79),
        ("p", "p", "KeyP", 80),
        ("q", "q", "KeyQ", 81),
        ("r", "r", "KeyR", 82),
        ("s", "s", "KeyS", 83),
        ("t", "t", "KeyT", 84),
        ("u", "u", "KeyU", 85),
        ("v", "v", "KeyV", 86),
        ("w", "w", "KeyW", 87),
        ("x", "x", "KeyX", 88),
        ("y", "y", "KeyY", 89),
        ("z", "z", "KeyZ", 90),
        ("f1", "F1", "F1", 112),
        ("f2", "F2", "F2", 113),
        ("f3", "F3", "F3", 114),
        ("f4", "F4", "F4", 115),
        ("f5", "F5", "F5", 116),
        ("f6", "F6", "F6", 117),
        ("f7", "F7", "F7", 118),
        ("f8", "F8", "F8", 119),
        ("f9", "F9", "F9", 120),
        ("f10", "F10", "F10", 121),
        ("f11", "F11", "F11", 122),
        ("f12", "F12", "F12", 123),
    ];
    let lower = key.to_lowercase();
    KEYS.iter()
        .find(|(name, _, _, _)| *name == lower)
        .map(|(_, k, c, vk)| {
            (
                if key.chars().count() == 1 { key } else { *k },
                *c,
                Some(*vk),
            )
        })
        .unwrap_or((key, if key.chars().count() == 1 { "" } else { key }, None))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_key_combo_simple() {
        let (mods, key) = parse_key_combo("Enter");
        assert_eq!(mods, 0);
        assert_eq!(key, "Enter");
    }

    #[test]
    fn test_parse_key_combo_ctrl() {
        use crate::cdp::types::modifiers;
        let (mods, key) = parse_key_combo("Ctrl+A");
        assert_eq!(mods, modifiers::CTRL);
        assert_eq!(key, "A");
    }

    #[test]
    fn test_parse_key_combo_cmd_shift() {
        use crate::cdp::types::modifiers;
        let (mods, key) = parse_key_combo("Cmd+Shift+S");
        assert_eq!(mods, modifiers::META | modifiers::SHIFT);
        assert_eq!(key, "S");
    }

    #[test]
    fn test_parse_key_combo_all_modifiers() {
        use crate::cdp::types::modifiers;
        let (mods, key) = parse_key_combo("Ctrl+Alt+Shift+Cmd+X");
        assert_eq!(
            mods,
            modifiers::CTRL | modifiers::ALT | modifiers::SHIFT | modifiers::META
        );
        assert_eq!(key, "X");
    }

    #[test]
    fn test_parse_key_combo_case_insensitive() {
        use crate::cdp::types::modifiers;
        let (mods, key) = parse_key_combo("ctrl+a");
        assert_eq!(mods, modifiers::CTRL);
        assert_eq!(key, "a");
    }

    #[test]
    fn test_key_to_codes_enter() {
        let (key, code, vk) = key_to_codes("Enter");
        assert_eq!(key, "Enter");
        assert_eq!(code, "Enter");
        assert_eq!(vk, Some(13));
    }

    #[test]
    fn test_key_to_codes_tab() {
        let (key, code, vk) = key_to_codes("Tab");
        assert_eq!(key, "Tab");
        assert_eq!(code, "Tab");
        assert_eq!(vk, Some(9));
    }

    #[test]
    fn test_key_to_codes_letter() {
        let (key, code, vk) = key_to_codes("a");
        assert_eq!(key, "a");
        assert_eq!(code, "KeyA");
        assert_eq!(vk, Some(65));
    }

    #[test]
    fn test_key_to_codes_arrow() {
        let (key, code, vk) = key_to_codes("ArrowDown");
        assert_eq!(key, "ArrowDown");
        assert_eq!(code, "ArrowDown");
        assert_eq!(vk, Some(40));
    }

    #[test]
    fn test_key_to_codes_case_insensitive() {
        let (key, code, vk) = key_to_codes("ESCAPE");
        assert_eq!(key, "Escape");
        assert_eq!(code, "Escape");
        assert_eq!(vk, Some(27));
    }

    #[test]
    fn test_key_to_codes_alias() {
        // "esc" should work as alias for "Escape"
        let (key, code, vk) = key_to_codes("esc");
        assert_eq!(key, "Escape");
        assert_eq!(code, "Escape");
        assert_eq!(vk, Some(27));

        // "up" should work as alias for "ArrowUp"
        let (key, code, vk) = key_to_codes("up");
        assert_eq!(key, "ArrowUp");
        assert_eq!(code, "ArrowUp");
        assert_eq!(vk, Some(38));
    }

    #[test]
    fn printable_layout_preserves_literals_and_maps_shift() {
        use crate::cdp::modifiers::SHIFT;
        for &(base, shifted, code, vk) in PRINTABLE_KEYS {
            assert_eq!(key_to_codes(base), (base, code, Some(vk)));
            assert_eq!(key_to_codes(shifted), (shifted, code, Some(vk)));
            assert_eq!(key_for_modifiers(base, SHIFT), shifted);
            assert_eq!(key_for_modifiers(shifted, SHIFT), shifted);
        }
        assert_eq!(key_to_codes("O"), ("O", "KeyO", Some(79)));
        assert_eq!(key_to_codes("Shift"), ("Shift", "ShiftLeft", Some(16)));
        assert_eq!(key_to_codes("Ctrl"), ("Control", "ControlLeft", Some(17)));
        for key in ["é", "😀"] {
            assert_eq!(key_to_codes(key), (key, "", None));
            assert_eq!(key_text(key), Some(key));
            assert_eq!(key_for_modifiers(key, SHIFT), key);
        }
        assert_eq!(key_text("some text"), None);
        assert_eq!(key_text("\u{0001}"), None);
    }

    #[test]
    fn test_key_to_codes_unknown() {
        // Unknown keys should pass through
        let (key, code, vk) = key_to_codes("SomeWeirdKey");
        assert_eq!(key, "SomeWeirdKey");
        assert_eq!(code, "SomeWeirdKey");
        assert_eq!(vk, None);
    }

    #[test]
    fn test_parse_key_combo_literal_plus() {
        // A lone "+" is a literal key, not a separator artifact.
        let (mods, key) = parse_key_combo("+");
        assert_eq!(mods, 0);
        assert_eq!(key, "+");
    }

    #[test]
    fn test_parse_key_combo_shift_plus() {
        // "Shift++" is SHIFT modifier plus the literal "+" key.
        use crate::cdp::types::modifiers;
        let (mods, key) = parse_key_combo("Shift++");
        assert_eq!(mods, modifiers::SHIFT);
        assert_eq!(key, "+");
    }
}
