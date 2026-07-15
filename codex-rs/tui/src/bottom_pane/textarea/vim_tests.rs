use super::*;
use crate::key_hint;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use pretty_assertions::assert_eq;

#[derive(Debug, PartialEq, Eq)]
struct VimState {
    text: String,
    cursor: usize,
    mode: Option<&'static str>,
    register: String,
    register_kind: KillBufferKind,
}

fn normal_textarea(text: &str, cursor: usize) -> TextArea {
    let mut textarea = TextArea::new();
    textarea.insert_str(text);
    textarea.set_cursor(cursor);
    textarea.set_vim_enabled(/*enabled*/ true);
    textarea
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn press(textarea: &mut TextArea, sequence: &str) {
    for ch in sequence.chars() {
        textarea.input(key(KeyCode::Char(ch)));
    }
}

fn escape(textarea: &mut TextArea) {
    textarea.input(key(KeyCode::Esc));
}

fn state(textarea: &TextArea) -> VimState {
    VimState {
        text: textarea.text().to_string(),
        cursor: textarea.cursor(),
        mode: textarea.vim_mode_label(),
        register: textarea.kill_buffer.clone(),
        register_kind: textarea.kill_buffer_kind,
    }
}

#[test]
fn vim_change_supports_all_characterwise_operator_motions() {
    struct Case {
        name: &'static str,
        text: &'static str,
        cursor: usize,
        motion: char,
        expected_text: &'static str,
        expected_cursor: usize,
        expected_register: &'static str,
    }

    let cases = [
        Case {
            name: "line end",
            text: "abc def",
            cursor: 4,
            motion: '$',
            expected_text: "abc ",
            expected_cursor: 4,
            expected_register: "def",
        },
        Case {
            name: "line start",
            text: "abc def",
            cursor: 5,
            motion: '0',
            expected_text: "ef",
            expected_cursor: 0,
            expected_register: "abc d",
        },
        Case {
            name: "word end",
            text: "abc def",
            cursor: 4,
            motion: 'e',
            expected_text: "abc ",
            expected_cursor: 4,
            expected_register: "def",
        },
        Case {
            name: "word backward",
            text: "abc def",
            cursor: 6,
            motion: 'b',
            expected_text: "abc f",
            expected_cursor: 4,
            expected_register: "de",
        },
        Case {
            name: "left",
            text: "abc",
            cursor: 2,
            motion: 'h',
            expected_text: "ac",
            expected_cursor: 1,
            expected_register: "b",
        },
        Case {
            name: "right",
            text: "abc",
            cursor: 1,
            motion: 'l',
            expected_text: "ac",
            expected_cursor: 1,
            expected_register: "b",
        },
    ];

    for case in cases {
        let mut textarea = normal_textarea(case.text, case.cursor);
        press(&mut textarea, &format!("c{}", case.motion));
        assert_eq!(
            state(&textarea),
            VimState {
                text: case.expected_text.to_string(),
                cursor: case.expected_cursor,
                mode: Some("Insert"),
                register: case.expected_register.to_string(),
                register_kind: KillBufferKind::Characterwise,
            },
            "{}",
            case.name
        );
    }
}

#[test]
fn vim_cw_changes_only_the_current_word_piece() {
    let cases = [
        ("middle", "hello", 1, "h", "ello"),
        ("final character", "hello", 4, "hell", "o"),
        ("punctuation", "foo...bar", 3, "foobar", "..."),
        ("whitespace", "foo   bar", 3, "foobar", "   "),
        ("unicode", "héllo", "hé".len(), "hé", "llo"),
    ];

    for (name, text, cursor, expected_text, expected_register) in cases {
        let mut textarea = normal_textarea(text, cursor);
        press(&mut textarea, "cw");
        assert_eq!(textarea.text(), expected_text, "{name}");
        assert_eq!(textarea.kill_buffer, expected_register, "{name}");
        assert_eq!(textarea.vim_mode_label(), Some("Insert"), "{name}");
    }

    let mut textarea = TextArea::new();
    textarea.insert_str("a");
    textarea.insert_element("<slot>");
    textarea.insert_str("b");
    textarea.set_cursor(/*pos*/ 1);
    textarea.set_vim_enabled(/*enabled*/ true);
    press(&mut textarea, "cw");
    assert_eq!(textarea.text(), "ab");
    assert_eq!(textarea.kill_buffer, "<slot>");
}

#[test]
fn vim_line_changes_leave_one_empty_logical_line() {
    let cases = [
        (
            "cc first",
            "one\ntwo\nthree",
            1,
            "cc",
            "\ntwo\nthree",
            0,
            "one\n",
        ),
        (
            "cc middle",
            "one\ntwo\nthree",
            5,
            "cc",
            "one\n\nthree",
            4,
            "two\n",
        ),
        (
            "cc last",
            "one\ntwo\nthree",
            9,
            "cc",
            "one\ntwo\n",
            8,
            "three",
        ),
        ("cc single", "one", 1, "cc", "", 0, "one"),
        ("cc trailing empty", "one\n", 4, "cc", "one\n", 4, ""),
        (
            "cj first",
            "one\ntwo\nthree",
            1,
            "cj",
            "\nthree",
            0,
            "one\ntwo\n",
        ),
        (
            "cj middle",
            "one\ntwo\nthree",
            5,
            "cj",
            "one\n",
            4,
            "two\nthree",
        ),
        (
            "cj last",
            "one\ntwo\nthree",
            9,
            "cj",
            "one\ntwo\n",
            8,
            "three",
        ),
        ("cj single", "one", 1, "cj", "", 0, "one"),
        ("cj trailing empty", "one\n", 4, "cj", "one\n", 4, ""),
        (
            "ck first",
            "one\ntwo\nthree",
            1,
            "ck",
            "\ntwo\nthree",
            0,
            "one\n",
        ),
        (
            "ck middle",
            "one\ntwo\nthree",
            5,
            "ck",
            "\nthree",
            0,
            "one\ntwo\n",
        ),
        (
            "ck last",
            "one\ntwo\nthree",
            9,
            "ck",
            "one\n",
            4,
            "two\nthree",
        ),
        ("ck single", "one", 1, "ck", "", 0, "one"),
        ("ck trailing empty", "one\n", 4, "ck", "", 0, "one\n"),
    ];

    for (name, text, cursor, command, expected_text, expected_cursor, expected_register) in cases {
        let mut textarea = normal_textarea(text, cursor);
        press(&mut textarea, command);
        assert_eq!(
            state(&textarea),
            VimState {
                text: expected_text.to_string(),
                cursor: expected_cursor,
                mode: Some("Insert"),
                register: expected_register.to_string(),
                register_kind: KillBufferKind::Linewise,
            },
            "{}",
            name
        );
    }
}

#[test]
fn vertical_delete_and_yank_motions_use_linewise_registers() {
    let mut textarea = normal_textarea("one\ntwo\nthree", 1);
    press(&mut textarea, "dj");
    assert_eq!(textarea.text(), "three");
    assert_eq!(textarea.kill_buffer, "one\ntwo\n");
    assert_eq!(textarea.kill_buffer_kind, KillBufferKind::Linewise);

    let mut textarea = normal_textarea("one\ntwo\nthree", 5);
    press(&mut textarea, "yk");
    assert_eq!(textarea.text(), "one\ntwo\nthree");
    assert_eq!(textarea.kill_buffer, "one\ntwo\n");
    assert_eq!(textarea.kill_buffer_kind, KillBufferKind::Linewise);
}

#[test]
fn vim_buffer_jumps_target_first_non_blank_and_consume_invalid_prefixes() {
    let mut textarea = normal_textarea("  one\n\n  last", 5);
    press(&mut textarea, "gg");
    assert_eq!(textarea.cursor(), 2);
    press(&mut textarea, "G");
    assert_eq!(textarea.cursor(), 9);

    let mut textarea = normal_textarea("first\nsecond", 8);
    press(&mut textarea, "gx");
    assert_eq!(textarea.text(), "first\nsecond");
    assert_eq!(textarea.cursor(), 8);
    press(&mut textarea, "gg");
    assert_eq!(textarea.cursor(), 0);

    let mut trailing = normal_textarea("one\n", 0);
    press(&mut trailing, "G");
    assert_eq!(trailing.cursor(), trailing.text().len());

    let mut empty = normal_textarea("", 0);
    press(&mut empty, "ggG");
    assert_eq!(empty.cursor(), 0);
}

#[test]
fn vim_buffer_jumps_honor_remapped_bindings() {
    let mut textarea = normal_textarea("  first\n  last", 10);
    textarea.vim_normal_keymap.move_buffer_start = vec![key_hint::plain(KeyCode::Char('z'))];
    textarea.vim_normal_keymap.move_buffer_end = vec![key_hint::plain(KeyCode::Char('q'))];

    press(&mut textarea, "zz");
    assert_eq!(textarea.cursor(), 2);
    press(&mut textarea, "q");
    assert_eq!(textarea.cursor(), 10);
}

#[test]
fn vim_undo_groups_insert_and_change_sessions() {
    let mut textarea = normal_textarea("base", 1);
    press(&mut textarea, "iXY");
    escape(&mut textarea);
    press(&mut textarea, "u");
    assert_eq!((textarea.text(), textarea.cursor()), ("base", 1));

    let mut textarea = normal_textarea("hello world", 0);
    press(&mut textarea, "cwnew");
    escape(&mut textarea);
    press(&mut textarea, "u");
    assert_eq!((textarea.text(), textarea.cursor()), ("hello world", 0));
}

#[test]
fn vim_undo_handles_direct_edits_paste_repetition_and_no_ops() {
    let mut textarea = normal_textarea("abc", 0);
    press(&mut textarea, "xx");
    assert_eq!(textarea.text(), "c");
    press(&mut textarea, "u");
    assert_eq!(textarea.text(), "bc");
    press(&mut textarea, "u");
    assert_eq!(textarea.text(), "abc");
    press(&mut textarea, "u");
    assert_eq!(textarea.text(), "abc");

    press(&mut textarea, "x");
    press(&mut textarea, "i");
    escape(&mut textarea);
    press(&mut textarea, "u");
    assert_eq!(textarea.text(), "abc");
    assert_eq!(textarea.kill_buffer, "a");

    let mut textarea = normal_textarea("one\ntwo", 0);
    press(&mut textarea, "Yp");
    assert_eq!(textarea.text(), "one\none\ntwo");
    press(&mut textarea, "u");
    assert_eq!(textarea.text(), "one\ntwo");
    assert_eq!(textarea.kill_buffer, "one\n");
    assert_eq!(textarea.kill_buffer_kind, KillBufferKind::Linewise);
}

#[test]
fn vim_undo_resets_on_buffer_replacement_and_vim_toggles() {
    let mut textarea = normal_textarea("old", 0);
    press(&mut textarea, "x");
    textarea.set_text_clearing_elements("new");
    press(&mut textarea, "u");
    assert_eq!(textarea.text(), "new");

    press(&mut textarea, "x");
    textarea.set_vim_enabled(/*enabled*/ false);
    textarea.set_vim_enabled(/*enabled*/ true);
    press(&mut textarea, "u");
    assert_eq!(textarea.text(), "ew");
}

#[test]
fn vim_undo_restores_atomic_elements_without_rewinding_ids() {
    let mut textarea = TextArea::new();
    textarea.insert_str("a");
    let original_id = textarea.insert_element("<slot>");
    textarea.insert_str("b");
    textarea.set_cursor(/*pos*/ 1);
    let original_elements = textarea.text_element_snapshots();
    textarea.set_vim_enabled(/*enabled*/ true);

    press(&mut textarea, "x");
    assert_eq!(textarea.text(), "ab");
    press(&mut textarea, "u");
    assert_eq!(textarea.text(), "a<slot>b");
    assert_eq!(textarea.text_element_snapshots(), original_elements);

    textarea.set_cursor(textarea.text().len());
    let new_id = textarea.insert_element("<new>");
    assert!(new_id > original_id);
}

#[test]
fn vim_undo_history_is_bounded_to_one_hundred_edits() {
    let mut textarea = normal_textarea(&"x".repeat(101), 0);
    for _ in 0..101 {
        press(&mut textarea, "x");
    }
    for _ in 0..101 {
        press(&mut textarea, "u");
    }
    assert_eq!(textarea.text(), "x".repeat(100));
}
