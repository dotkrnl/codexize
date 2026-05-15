use super::*;
use crate::app::test_support::key;
fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

#[test]
fn insert_and_move() {
    let mut b = String::new();
    let mut c = 0usize;
    for ch in "hello".chars() {
        apply(&mut b, &mut c, key(KeyCode::Char(ch)));
    }
    assert_eq!(b, "hello");
    assert_eq!(c, 5);
    apply(&mut b, &mut c, ctrl('a'));
    assert_eq!(c, 0);
    apply(&mut b, &mut c, ctrl('e'));
    assert_eq!(c, 5);
}

#[test]
fn ctrl_u_kills_to_start() {
    let mut b = "hello world".to_string();
    let mut c = 6usize;
    apply(&mut b, &mut c, ctrl('u'));
    assert_eq!(b, "world");
    assert_eq!(c, 0);
}

#[test]
fn ctrl_k_kills_to_end() {
    let mut b = "hello world".to_string();
    let mut c = 5usize;
    apply(&mut b, &mut c, ctrl('k'));
    assert_eq!(b, "hello");
    assert_eq!(c, 5);
}

#[test]
fn ctrl_w_deletes_word() {
    let mut b = "hello world".to_string();
    let mut c = 11usize;
    apply(&mut b, &mut c, ctrl('w'));
    assert_eq!(b, "hello ");
    assert_eq!(c, 6);
}

#[test]
fn ctrl_d_deletes_forward() {
    let mut b = "abc".to_string();
    let mut c = 1usize;
    apply(&mut b, &mut c, ctrl('d'));
    assert_eq!(b, "ac");
    assert_eq!(c, 1);
}

#[test]
fn insert_str_multiline_normalizes_newlines() {
    let mut b = "ab".to_string();
    let mut c = 1usize;
    insert_str(&mut b, &mut c, "X\r\nY\rZ");
    assert_eq!(b, "aX\nY\nZb");
    assert_eq!(c, 6);
}

#[test]
fn unicode_cursor() {
    let mut b = "日本語".to_string();
    let mut c = 3usize;
    apply(
        &mut b,
        &mut c,
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
    );
    assert_eq!(b, "日本");
    assert_eq!(c, 2);
}
