//! Vim-fidelity expectations here were taken from Vim 9 (`vim -Nu NONE`) rather
//! than from this implementation's output. Where the composer deliberately
//! diverges -- Claude Code parity, atomic elements, commands that ignore counts
//! -- the case says so.

use super::*;
use codex_protocol::user_input::MAX_USER_INPUT_TEXT_CHARS;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use pretty_assertions::assert_eq;

fn normal_textarea(text: &str, cursor: usize) -> TextArea {
    let mut textarea = TextArea::new();
    textarea.insert_str(text);
    textarea.set_cursor(cursor);
    textarea.set_vim_enabled(/*enabled*/ true);
    textarea
}

fn press(textarea: &mut TextArea, sequence: &str) {
    for ch in sequence.chars() {
        textarea.input(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }
}

fn escape(textarea: &mut TextArea) {
    textarea.input(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
}

#[test]
fn vim_first_non_blank_space_and_big_word_motions() {
    let mut textarea = normal_textarea("  foo.bar  baz-qux end", 5);

    press(&mut textarea, "^");
    assert_eq!(textarea.cursor(), 2);

    press(&mut textarea, " ");
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
fn vim_find_till_and_repeats_stay_on_the_current_line() {
    let mut textarea = normal_textarea("a1a2a3a\na4a", 0);

    press(&mut textarea, "fa");
    assert_eq!(textarea.cursor(), 2);
    press(&mut textarea, ";");
    assert_eq!(textarea.cursor(), 4);
    press(&mut textarea, ",");
    assert_eq!(textarea.cursor(), 2);

    textarea.set_cursor(/*pos*/ 0);
    press(&mut textarea, "ta");
    assert_eq!(textarea.cursor(), 1);

    textarea.set_cursor(/*pos*/ 6);
    press(&mut textarea, "Fa");
    assert_eq!(textarea.cursor(), 4);
    press(&mut textarea, "Ta");
    assert_eq!(textarea.cursor(), 3);

    textarea.set_cursor(/*pos*/ 6);
    press(&mut textarea, "fz");
    assert_eq!(textarea.cursor(), 6);
}

#[test]
fn vim_find_and_till_work_as_operator_motions() {
    let mut textarea = normal_textarea("abcxdefx", 0);
    press(&mut textarea, "dfx");
    assert_eq!(textarea.text(), "defx");
    assert_eq!(textarea.kill_buffer, "abcx");

    let mut textarea = normal_textarea("abcxdefx", 0);
    press(&mut textarea, "ctx");
    assert_eq!(textarea.text(), "xdefx");
    assert_eq!(textarea.kill_buffer, "abc");
    assert_eq!(textarea.vim_mode_label(), Some("Insert"));

    let mut textarea = normal_textarea("abcxdefx", 7);
    press(&mut textarea, "dFd");
    assert_eq!(textarea.text(), "abcx");
    assert_eq!(textarea.kill_buffer, "defx");

    let mut textarea = normal_textarea("abcxdefx", 7);
    press(&mut textarea, "dTd");
    assert_eq!(textarea.text(), "abcxd");
    assert_eq!(textarea.kill_buffer, "efx");

    let mut textarea = normal_textarea("a1a2a3", 0);
    press(&mut textarea, "fa;0d;");
    assert_eq!(textarea.text(), "2a3");
    assert_eq!(textarea.kill_buffer, "a1a");
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

    let mut textarea = normal_textarea("one\ntwo\nthree\nfour", 0);
    press(&mut textarea, "2dd");
    assert_eq!(textarea.text(), "three\nfour");
    assert_eq!(textarea.kill_buffer, "one\ntwo\n");
    assert_eq!(textarea.kill_buffer_kind, KillBufferKind::Linewise);

    let mut textarea = normal_textarea("abcdef", 0);
    press(&mut textarea, "5x");
    assert_eq!(textarea.text(), "f");
    assert_eq!(textarea.kill_buffer, "abcde");

    let mut textarea = normal_textarea("ab", 0);
    textarea.kill_buffer = "x".to_string();
    press(&mut textarea, "3p");
    assert_eq!(textarea.text(), "axxxb");
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
}

#[test]
fn vim_join_follows_vim_spacing_rules_and_lands_on_the_last_join() {
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
    // rewritten. Rescanning the keys for digits would corrupt the argument.
    let mut textarea = normal_textarea("aaaaaa", 0);
    press(&mut textarea, "3r2l2.");
    assert_eq!(textarea.text(), "22222a");

    let mut textarea = normal_textarea("aaaaaa", 0);
    press(&mut textarea, "3r2l.");
    assert_eq!(textarea.text(), "222222");
}

#[test]
fn vim_counted_dot_repeats_insert_sessions_that_ignore_counts() {
    let mut textarea = normal_textarea("ab", 0);
    press(&mut textarea, "iX");
    escape(&mut textarea);
    press(&mut textarea, "3.");
    assert_eq!(textarea.text(), "XXXXab");
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

    // The whole element is opaque, including its first character.
    let mut textarea = TextArea::new();
    textarea.insert_str("a ");
    textarea.insert_element(" leading space");
    textarea.insert_str(" b");
    textarea.set_cursor(/*pos*/ 0);
    textarea.set_vim_enabled(/*enabled*/ true);
    press(&mut textarea, "W");
    assert_eq!(textarea.cursor(), element_start);
}

#[test]
fn vim_replace_char_leaves_cursor_on_last_replaced_char() {
    let mut textarea = normal_textarea("aaaaaa", 0);
    press(&mut textarea, "3r2");
    assert_eq!(textarea.text(), "222aaa");
    assert_eq!(textarea.cursor(), 2);
}

#[test]
fn vim_counts_are_clamped_so_long_digit_runs_cannot_hang_or_panic() {
    // Without a clamp this allocates a `usize::MAX` vector and aborts.
    let mut textarea = normal_textarea("one\ntwo", 0);
    press(&mut textarea, "99999999999999999999>>");
    assert_eq!(textarea.text(), "  one\n  two");

    let mut textarea = normal_textarea("one two", 0);
    press(&mut textarea, "99999999999999999999w");
    assert_eq!(textarea.cursor(), 7);

    // Operator and motion counts multiply, so both must be clamped after the
    // product too, not just individually.
    let mut textarea = normal_textarea("one two", 0);
    press(&mut textarea, "99999999d99999999w");
    assert_eq!(textarea.text(), "");
}

/// The paste budget bounds allocation; it is not the message-length limit,
/// which the composer reports at submit time where the user can see it.
#[test]
fn vim_counted_paste_cannot_allocate_without_bound() {
    let budget = MAX_USER_INPUT_TEXT_CHARS * 4;

    // This register times this count could never be sent, so the command is
    // refused outright rather than truncated into a partial paste.
    let mut textarea = normal_textarea("ab", 0);
    textarea.kill_buffer = "x".repeat(MAX_USER_INPUT_TEXT_CHARS);
    press(&mut textarea, "5p");
    assert_eq!(textarea.text(), "ab");

    // Dot-repeat replays single pastes, each of which passes a per-paste check
    // on its own, so the budget has to bound the result rather than the paste.
    let mut textarea = normal_textarea("ab", 0);
    textarea.kill_buffer = "x".repeat(MAX_USER_INPUT_TEXT_CHARS / 4);
    press(&mut textarea, "p1000.");
    assert!(textarea.text().len() <= budget);

    let mut textarea = normal_textarea("ab", 0);
    textarea.kill_buffer = "x".to_string();
    press(&mut textarea, "3p");
    assert_eq!(textarea.text(), "axxxb");

    // Multi-byte text is not refused for merely being multi-byte.
    let mut textarea = normal_textarea("ab", 0);
    let wide = "界".repeat(MAX_USER_INPUT_TEXT_CHARS / 4);
    textarea.kill_buffer = wide.clone();
    press(&mut textarea, "2p");
    assert_eq!(textarea.text(), format!("a{wide}{wide}b"));
}

#[test]
fn vim_counted_linewise_paste_preserves_blank_lines() {
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
}

#[test]
fn vim_join_indent_and_dedent_lines() {
    let mut textarea = normal_textarea("one\n  two\nthree", 0);
    press(&mut textarea, "J");
    assert_eq!(textarea.text(), "one two\nthree");
    assert_eq!(textarea.cursor(), 3);

    let mut textarea = normal_textarea("one\ntwo\nthree", 0);
    press(&mut textarea, "2>>");
    assert_eq!(textarea.text(), "  one\n  two\nthree");
    assert_eq!(textarea.cursor(), 2);
    press(&mut textarea, "2<<");
    assert_eq!(textarea.text(), "one\ntwo\nthree");
    assert_eq!(textarea.cursor(), 0);
}

#[test]
fn vim_buffer_operator_motions_and_counted_jumps() {
    let mut textarea = normal_textarea("one\ntwo\nthree\nfour", 4);
    press(&mut textarea, "dG");
    assert_eq!(textarea.text(), "one\n");
    assert_eq!(textarea.kill_buffer, "two\nthree\nfour");

    let mut textarea = normal_textarea("one\ntwo\nthree", 8);
    press(&mut textarea, "dgg");
    assert_eq!(textarea.text(), "");
    assert_eq!(textarea.kill_buffer, "one\ntwo\nthree");

    let mut textarea = normal_textarea("one\ntwo\nthree\nfour", 0);
    press(&mut textarea, "3G");
    assert_eq!(textarea.cursor(), 8);
    press(&mut textarea, "2gg");
    assert_eq!(textarea.cursor(), 4);

    let mut textarea = normal_textarea("one\ntwo\nthree", 4);
    press(&mut textarea, "yG");
    assert_eq!(textarea.text(), "one\ntwo\nthree");
    assert_eq!(textarea.kill_buffer, "two\nthree");
    assert_eq!(textarea.kill_buffer_kind, KillBufferKind::Linewise);
}

#[test]
fn vim_replace_char_supports_counts_unicode_and_atomic_elements() {
    let mut textarea = normal_textarea("abcd", 1);
    press(&mut textarea, "3rx");
    assert_eq!(textarea.text(), "axxx");
    assert_eq!(textarea.cursor(), 3);

    let mut textarea = normal_textarea("a👍c", 1);
    press(&mut textarea, "rλ");
    assert_eq!(textarea.text(), "aλc");

    let mut textarea = TextArea::new();
    textarea.insert_str("a");
    textarea.insert_element("<slot>");
    textarea.insert_str("b");
    textarea.set_cursor(/*pos*/ 1);
    textarea.set_vim_enabled(/*enabled*/ true);
    press(&mut textarea, "rz");
    assert_eq!(textarea.text(), "azb");
    assert!(textarea.text_element_snapshots().is_empty());
}

#[test]
fn vim_dot_repeats_direct_insert_and_change_edits() {
    let mut textarea = normal_textarea("abcdef", 0);
    press(&mut textarea, "xl.");
    assert_eq!(textarea.text(), "bdef");

    let mut textarea = normal_textarea("ab", 0);
    press(&mut textarea, "iXY");
    escape(&mut textarea);
    press(&mut textarea, "$.");
    assert_eq!(textarea.text(), "XYaXYb");

    let mut textarea = normal_textarea("one two", 0);
    press(&mut textarea, "cwX");
    escape(&mut textarea);
    press(&mut textarea, "w.");
    assert_eq!(textarea.text(), "X X");
}

#[test]
fn vim_counted_dot_repeats_the_last_change() {
    let mut textarea = normal_textarea("abcdef", 0);
    press(&mut textarea, "x3.");
    assert_eq!(textarea.text(), "ef");
    press(&mut textarea, "u");
    assert_eq!(textarea.text(), "def");
}
