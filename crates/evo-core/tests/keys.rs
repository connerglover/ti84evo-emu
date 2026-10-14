//! Key scripts (`evo_core::keys`).

use evo_core::Key;
use evo_core::keys::{KeyAction, KeyScriptError, TAP_GAP_MS, TAP_HOLD_MS, parse_key_script};

fn act(at_ms: u64, key: Key, pressed: bool) -> KeyAction {
    KeyAction {
        at_ms,
        key,
        pressed,
    }
}

fn m(row: u8, col: u8) -> Key {
    Key::Matrix { row, col }
}

#[test]
fn constants() {
    assert_eq!(TAP_HOLD_MS, 50);
    assert_eq!(TAP_GAP_MS, 50);
}

#[test]
fn parse_tap() {
    assert_eq!(
        parse_key_script("r2c5"),
        Ok(vec![act(0, m(2, 5), true), act(50, m(2, 5), false)])
    );
}

#[test]
fn parse_press_wait_release() {
    assert_eq!(
        parse_key_script("+on wait=200;-on"),
        Ok(vec![act(0, Key::On, true), act(200, Key::On, false)])
    );
}

#[test]
fn parse_sequence_cursor() {
    assert_eq!(
        parse_key_script("on r0c0"),
        Ok(vec![
            act(0, Key::On, true),
            act(50, Key::On, false),
            act(100, m(0, 0), true),
            act(150, m(0, 0), false),
        ])
    );
}

#[test]
fn parse_named_keys() {
    // The Self-Test boot combo, then a tap of 2nd.
    assert_eq!(
        parse_key_script("+alpha +ln +enter wait=10 -alpha;-ln -enter 2nd"),
        Ok(vec![
            act(0, m(2, 7), true),
            act(0, m(2, 2), true),
            act(0, m(6, 0), true),
            act(10, m(2, 7), false),
            act(10, m(2, 2), false),
            act(10, m(6, 0), false),
            act(10, m(1, 5), true),
            act(60, m(1, 5), false),
        ])
    );
    assert_eq!(parse_key_script("  ;; \n"), Ok(vec![]));
}

#[test]
fn parse_errors() {
    let bad = |s: &str, index: usize, token: &str| {
        assert_eq!(
            parse_key_script(s),
            Err(KeyScriptError::BadToken {
                index,
                token: token.to_owned()
            }),
            "{s:?}"
        );
    };
    bad("r8c0", 0, "r8c0");
    bad("wait=x", 0, "wait=x");
    bad("foo", 0, "foo");
    bad("r1c1 bogus", 1, "bogus");
    bad("+", 0, "+");
    bad("wait=99999999999999999999", 0, "wait=99999999999999999999");
    bad("r1c1x", 0, "r1c1x");
    assert_eq!(
        KeyScriptError::BadToken {
            index: 1,
            token: "bogus".into()
        }
        .to_string(),
        "key script token 1 `bogus` is not valid"
    );
}
