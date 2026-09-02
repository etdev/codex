//! Counts, WORD motions, find repeats, paste-before, join, and indent.
//!
//! Expectations follow Vim 9 (`vim -Nu NONE`). Where the composer deliberately
//! diverges -- atomic elements, commands that ignore counts -- the case says so.

use super::super::KillBufferKind;
use super::super::TextArea;
use crate::keymap::KeyChordMatch;
use crate::keymap::KeyChordMatcher;
use crate::keymap::RuntimeKeymap;
use codex_protocol::user_input::MAX_USER_INPUT_TEXT_CHARS;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use pretty_assertions::assert_eq;
use tokio::time::Instant;

fn normal_textarea(text: &str, cursor: usize) -> TextArea {
    let mut textarea = TextArea::new();
    textarea.insert_str(text);
    textarea.set_cursor(cursor);
    textarea.set_vim_enabled(/*enabled*/ true);
    textarea
}

fn press(textarea: &mut TextArea, keys: &str) {
    let keymap = RuntimeKeymap::defaults();
    let mut matcher = KeyChordMatcher::default();
    for key in keys.chars() {
        let code = match key {
            '\n' => KeyCode::Enter,
            '\x1b' => KeyCode::Esc,
            ch => KeyCode::Char(ch),
        };
        let event = KeyEvent::new(code, KeyModifiers::NONE);
        match matcher.advance(
            event,
            &keymap.chords,
            textarea.keymap_contexts(),
            Instant::now(),
        ) {
            KeyChordMatch::PassThrough => textarea.input(event),
            KeyChordMatch::Completed(event) => textarea.input(event),
            KeyChordMatch::Pending(_) | KeyChordMatch::Cancelled | KeyChordMatch::Ignored => {}
        }
    }
}

fn escape(textarea: &mut TextArea) {
    textarea.input(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
}

#[test]
fn vim_first_non_blank_and_big_word_motions() {
    let mut textarea = normal_textarea("  foo.bar  baz-qux end", 5);

    press(&mut textarea, "^");
    assert_eq!(textarea.cursor(), 2);

    press(&mut textarea, "l");
    assert_eq!(textarea.cursor(), 3);

    press(&mut textarea, "W");
    assert_eq!(textarea.cursor(), 11);

    press(&mut textarea, "E");
    assert_eq!(textarea.cursor(), 17);

    press(&mut textarea, "B");
    assert_eq!(textarea.cursor(), 11);

    press(&mut textarea, "2E");
    assert_eq!(textarea.cursor(), 21);
}

#[test]
fn vim_find_repeats_stay_on_the_current_line() {
    let mut textarea = normal_textarea("a1a2a3a\na4a", 0);

    press(&mut textarea, "fa");
    assert_eq!(textarea.cursor(), 2);
    press(&mut textarea, ";");
    assert_eq!(textarea.cursor(), 4);
    press(&mut textarea, ",");
    assert_eq!(textarea.cursor(), 2);
    press(&mut textarea, "3;");
    assert_eq!(textarea.cursor(), 2, "past the line end, the motion fails");
    press(&mut textarea, "2;");
    assert_eq!(textarea.cursor(), 6);

    // A repeated till skips the match it is already touching.
    textarea.set_cursor(/*pos*/ 0);
    press(&mut textarea, "ta");
    assert_eq!(textarea.cursor(), 1);
    press(&mut textarea, ";");
    assert_eq!(textarea.cursor(), 3);
}

#[test]
fn vim_find_repeats_work_as_operator_motions() {
    let mut textarea = normal_textarea("a1a2a3", 0);
    press(&mut textarea, "fa;0d;");
    assert_eq!(textarea.text(), "2a3");
    assert_eq!(textarea.kill_buffer, "a1a");

    let mut textarea = normal_textarea("a1a2a3", 0);
    press(&mut textarea, "fa$d,");
    assert_eq!(textarea.text(), "a1a23");
}

#[test]
fn vim_counts_apply_to_motions_operators_deletes_and_paste() {
    let mut textarea = normal_textarea("one two three four", 0);
    press(&mut textarea, "3w");
    assert_eq!(textarea.cursor(), 14);

    let mut textarea = normal_textarea("one two three", 0);
    press(&mut textarea, "d2w");
    assert_eq!(textarea.text(), "three");
    assert_eq!(textarea.kill_buffer, "one two ");

    let mut textarea = normal_textarea("one two three four five six seven", 0);
    press(&mut textarea, "2d3w");
    assert_eq!(textarea.text(), "seven");

    let mut textarea = normal_textarea("one\ntwo\nthree\nfour", 0);
    press(&mut textarea, "2dd");
    assert_eq!(textarea.text(), "three\nfour");
    assert_eq!(textarea.kill_buffer, "one\ntwo\n");
    assert_eq!(textarea.kill_buffer_kind, KillBufferKind::Linewise);

    let mut textarea = normal_textarea("one\ntwo\nthree\nfour", 0);
    press(&mut textarea, "2yyjp");
    assert_eq!(textarea.text(), "one\ntwo\none\ntwo\nthree\nfour");

    let mut textarea = normal_textarea("one\ntwo\nthree\nfour", 0);
    press(&mut textarea, "2cc");
    assert_eq!(textarea.text(), "\nthree\nfour");
    assert_eq!(textarea.vim_mode_label(), Some("Insert"));

    let mut textarea = normal_textarea("one\ntwo\nthree\nfour", 0);
    press(&mut textarea, "2dj");
    assert_eq!(textarea.text(), "four");

    let mut textarea = normal_textarea("abcdef", 0);
    press(&mut textarea, "5x");
    assert_eq!(textarea.text(), "f");
    assert_eq!(textarea.kill_buffer, "abcde");

    let mut textarea = normal_textarea("ab\ncd", 0);
    press(&mut textarea, "5x");
    assert_eq!(textarea.text(), "\ncd", "x never crosses the line end");

    let mut textarea = normal_textarea("ab", 0);
    textarea.kill_buffer = "x".to_string();
    press(&mut textarea, "3p");
    assert_eq!(textarea.text(), "axxxb");

    let mut textarea = normal_textarea("one two three", 0);
    press(&mut textarea, "3l");
    assert_eq!(textarea.cursor(), 3);
    press(&mut textarea, "0");
    assert_eq!(textarea.cursor(), 0, "0 without a count is a motion");
    press(&mut textarea, "10l");
    assert_eq!(textarea.cursor(), 10, "0 after a digit extends the count");
}

#[test]
fn vim_counted_change_word_covers_several_words() {
    let mut textarea = normal_textarea("one two three four", 0);
    press(&mut textarea, "c2wX");
    escape(&mut textarea);
    assert_eq!(textarea.text(), "X three four");
}

#[test]
fn vim_counted_till_motion_reaches_the_counted_occurrence() {
    // `3ta` stops before the third `a`; `3fa` lands on it.
    let mut textarea = normal_textarea("a1a2a3a", 0);
    press(&mut textarea, "3ta");
    assert_eq!(textarea.cursor(), 5);

    let mut textarea = normal_textarea("a1a2a3a", 0);
    press(&mut textarea, "3fa");
    assert_eq!(textarea.cursor(), 6);

    let mut textarea = normal_textarea("a1a2a3a", 0);
    press(&mut textarea, "d2fa");
    assert_eq!(textarea.text(), "3a");
}

#[test]
fn vim_counted_linewise_paste_keeps_copies_on_separate_lines() {
    // A register yanked from the final line has no trailing newline; repeating
    // it must not run the copies together.
    let mut textarea = normal_textarea("one\ntwo", 4);
    press(&mut textarea, "yy3p");
    assert_eq!(textarea.text(), "one\ntwo\ntwo\ntwo\ntwo");

    let mut textarea = normal_textarea("one\ntwo\nthree", 0);
    press(&mut textarea, "yy2p");
    assert_eq!(textarea.text(), "one\none\none\ntwo\nthree");

    // A blank-line register is all newlines; trimming them all would collapse
    // the paste to nothing.
    let mut textarea = normal_textarea("\none", 0);
    press(&mut textarea, "yy2p");
    assert_eq!(textarea.text(), "\n\n\none");
}

#[test]
fn vim_paste_before_handles_characterwise_and_linewise_registers() {
    let mut textarea = normal_textarea("ab", 1);
    textarea.kill_buffer = "XY".to_string();
    press(&mut textarea, "P");
    assert_eq!(textarea.text(), "aXYb");

    let mut textarea = normal_textarea("one\ntwo", 4);
    textarea.kill_buffer = "zero\n".to_string();
    textarea.kill_buffer_kind = KillBufferKind::Linewise;
    press(&mut textarea, "P");
    assert_eq!(textarea.text(), "one\nzero\ntwo");
    assert_eq!(textarea.cursor(), 4);

    let mut textarea = normal_textarea("ab", 1);
    textarea.kill_buffer = "x".to_string();
    press(&mut textarea, "2P.");
    assert_eq!(textarea.text(), "axxxxb");
}

#[test]
fn vim_counted_paste_cannot_allocate_without_bound() {
    let budget = MAX_USER_INPUT_TEXT_CHARS * 4;

    // This register times this count could never be sent, so the command is
    // refused outright rather than truncated into a partial paste.
    let mut textarea = normal_textarea("ab", 0);
    textarea.kill_buffer = "x".repeat(MAX_USER_INPUT_TEXT_CHARS);
    press(&mut textarea, "5p");
    assert_eq!(textarea.text(), "ab");

    let mut textarea = normal_textarea("ab", 0);
    textarea.kill_buffer = "x".repeat(MAX_USER_INPUT_TEXT_CHARS / 4);
    press(&mut textarea, "p1000.");
    assert!(textarea.text().len() <= budget);

    // Multi-byte text is not refused for merely being multi-byte.
    let mut textarea = normal_textarea("ab", 0);
    let wide = "界".repeat(MAX_USER_INPUT_TEXT_CHARS / 4);
    textarea.kill_buffer = wide.clone();
    press(&mut textarea, "2p");
    assert_eq!(textarea.text(), format!("a{wide}{wide}b"));
}

#[test]
fn vim_join_follows_vim_spacing_rules_and_lands_on_the_last_join() {
    let mut textarea = normal_textarea("one\n  two\nthree", 0);
    press(&mut textarea, "J");
    assert_eq!(textarea.text(), "one two\nthree");
    assert_eq!(textarea.cursor(), 3);

    // No space is inserted before a closing paren.
    let mut textarea = normal_textarea("foo\n)bar", 0);
    press(&mut textarea, "J");
    assert_eq!(textarea.text(), "foo)bar");
    assert_eq!(textarea.cursor(), 3);

    // A line already ending in whitespace does not gain a second space.
    let mut textarea = normal_textarea("foo \nbar", 0);
    press(&mut textarea, "J");
    assert_eq!(textarea.text(), "foo bar");
    assert_eq!(textarea.cursor(), 4);

    // A counted join leaves the cursor at the final join, not the first.
    let mut textarea = normal_textarea("one\ntwo\nthree", 0);
    press(&mut textarea, "3J");
    assert_eq!(textarea.text(), "one two three");
    assert_eq!(textarea.cursor(), 7);

    // Joining on the last line changes nothing and records nothing.
    let mut textarea = normal_textarea("one\ntwo", 4);
    press(&mut textarea, "xJ.");
    assert_eq!(textarea.text(), "one\no");
}

#[test]
fn vim_indent_and_dedent_lines() {
    let mut textarea = normal_textarea("one\ntwo\nthree", 0);
    press(&mut textarea, "2>>");
    assert_eq!(textarea.text(), "  one\n  two\nthree");
    assert_eq!(textarea.cursor(), 2);
    press(&mut textarea, "2<<");
    assert_eq!(textarea.text(), "one\ntwo\nthree");
    assert_eq!(textarea.cursor(), 0);

    let mut textarea = normal_textarea("one\ntwo\nthree", 0);
    press(&mut textarea, ">>j.");
    assert_eq!(textarea.text(), "  one\n  two\nthree");

    // A mismatched second key cancels without editing.
    let mut textarea = normal_textarea("one", 0);
    press(&mut textarea, "><");
    assert_eq!(textarea.text(), "one");

    // Without a clamp a long digit run would allocate a huge vector.
    let mut textarea = normal_textarea("one\ntwo", 0);
    press(&mut textarea, "99999999999999999999>>");
    assert_eq!(textarea.text(), "  one\n  two");
}

#[test]
fn vim_dot_count_replaces_the_recorded_count() {
    // `2x` then `3.` deletes 2 then 3, not 2 then 2+2+2.
    let mut textarea = normal_textarea("abcdefghij", 0);
    press(&mut textarea, "2x3.");
    assert_eq!(textarea.text(), "fghij");

    // The substituted count lands where the recorded one was, so this repeats
    // as `d3w` rather than `3d2w`.
    let mut textarea = normal_textarea("one two three four five six seven", 0);
    press(&mut textarea, "d2w3.");
    assert_eq!(textarea.text(), "six seven");

    // A bare `.` still replays the recorded count verbatim.
    let mut textarea = normal_textarea("abcdefghij", 0);
    press(&mut textarea, "2x.");
    assert_eq!(textarea.text(), "efghij");

    // `3r2` carries a count digit and an argument digit; only the count may be
    // rewritten.
    let mut textarea = normal_textarea("aaaaaa", 0);
    press(&mut textarea, "3r2l2.");
    assert_eq!(textarea.text(), "22222a");

    let mut textarea = normal_textarea("aaaaaa", 0);
    press(&mut textarea, "3r2l.");
    assert_eq!(textarea.text(), "222222");

    // A counted insert session is repeated whole.
    let mut textarea = normal_textarea("ab", 0);
    press(&mut textarea, "iX");
    escape(&mut textarea);
    press(&mut textarea, "3.");
    assert_eq!(textarea.text(), "XXXXab");
}

#[test]
fn vim_replace_char_supports_counts_and_leaves_cursor_on_last_replaced_char() {
    let mut textarea = normal_textarea("abcd", 1);
    press(&mut textarea, "3rx");
    assert_eq!(textarea.text(), "axxx");
    assert_eq!(textarea.cursor(), 3);

    // Vim refuses a replacement that would run past the line end.
    let mut textarea = normal_textarea("abc\nd", 1);
    press(&mut textarea, "5rx");
    assert_eq!(textarea.text(), "abc\nd");
}

#[test]
fn vim_counted_buffer_jumps_target_line_numbers() {
    let mut textarea = normal_textarea("one\ntwo\nthree\nfour", 0);
    press(&mut textarea, "3G");
    assert_eq!(textarea.cursor(), 8);
    press(&mut textarea, "2gg");
    assert_eq!(textarea.cursor(), 4);
    press(&mut textarea, "99G");
    assert_eq!(textarea.cursor(), 14);

    let mut textarea = normal_textarea("one\ntwo\nthree\nfour", 8);
    press(&mut textarea, "d2G");
    assert_eq!(textarea.text(), "one\nfour");
    assert_eq!(textarea.kill_buffer, "two\nthree\n");
    assert_eq!(textarea.kill_buffer_kind, KillBufferKind::Linewise);

    let mut textarea = normal_textarea("one\ntwo\nthree\nfour", 4);
    press(&mut textarea, "dG");
    assert_eq!(textarea.text(), "one\n");
}

#[test]
fn vim_word_motions_treat_an_atomic_element_as_one_word() {
    // The placeholder holds spaces, which must not read as WORD separators.
    let mut textarea = TextArea::new();
    textarea.insert_str("a ");
    textarea.insert_element("[Pasted Content 12 chars]");
    textarea.insert_str(" b");
    textarea.set_cursor(/*pos*/ 0);
    textarea.set_vim_enabled(/*enabled*/ true);

    let element_start = 2;
    press(&mut textarea, "W");
    assert_eq!(textarea.cursor(), element_start);
    press(&mut textarea, "W");
    assert_eq!(textarea.text().len() - 1, textarea.cursor());
    press(&mut textarea, "B");
    assert_eq!(textarea.cursor(), element_start);

    let mut textarea = TextArea::new();
    textarea.insert_str("a ");
    textarea.insert_element("[Pasted Content 12 chars]");
    textarea.insert_str(" b");
    textarea.set_cursor(/*pos*/ element_start);
    textarea.set_vim_enabled(/*enabled*/ true);
    press(&mut textarea, "dW");
    assert_eq!(textarea.text(), "a b");
    assert!(textarea.text_element_snapshots().is_empty());
}

#[test]
fn vim_count_is_observable_as_a_pending_command() {
    let mut textarea = normal_textarea("one two", 0);
    press(&mut textarea, "2");
    assert!(textarea.is_vim_operator_pending());
    escape(&mut textarea);
    assert!(!textarea.is_vim_operator_pending());
    press(&mut textarea, "w");
    assert_eq!(textarea.cursor(), 4, "escape discards the count");

    // Digits typed after an operator combine with the operator count.
    let mut textarea = normal_textarea("one two", 0);
    press(&mut textarea, "d");
    press(&mut textarea, "2");
    assert!(textarea.is_vim_operator_pending());
    press(&mut textarea, "l");
    assert_eq!(textarea.text(), "e two");
}
