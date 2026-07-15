use super::TextArea;
use super::VimFind;
use super::VimFindKind;
use super::VimIndentDirection;
use super::VimMode;
use super::VimMotion;
use super::VimOperator;
use super::VimOperatorRange;
use super::VimRangeKind;
use crate::key_hint::KeyBindingListExt;
use codex_protocol::user_input::MAX_USER_INPUT_TEXT_CHARS;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use std::ops::Range;

const VIM_INDENT: &str = "  ";

/// Cap on the bytes a single paste may leave in the composer.
///
/// Neither the count nor the register size is bounded on its own, so `1000p`
/// -- or `p` then `1000.`, which replays single pastes -- could otherwise ask
/// for gigabytes. The largest sendable message is `MAX_USER_INPUT_TEXT_CHARS`
/// chars of at most four bytes, so this refuses only pastes that could never be
/// sent regardless, and leaves judging the limit itself to the composer's
/// submit-time check.
const MAX_VIM_PASTE_BYTES: usize = MAX_USER_INPUT_TEXT_CHARS * 4;

impl TextArea {
    pub(super) fn handle_vim_buffer_start(
        &mut self,
        operator: Option<VimOperator>,
        count: Option<usize>,
        event: KeyEvent,
    ) {
        let pressed = operator.map_or_else(
            || self.vim_normal_keymap.move_buffer_start.is_pressed(event),
            |_| {
                self.vim_operator_keymap
                    .motion_buffer_start
                    .is_pressed(event)
            },
        );
        if !pressed {
            return;
        }
        if let Some(operator) = operator {
            self.apply_vim_operator_to_buffer_line(operator, count, /*default_to_last*/ false);
        } else {
            self.move_to_vim_line(count, /*default_to_last*/ false);
        }
    }

    pub(super) fn vim_normal_find_kind(&self, event: KeyEvent) -> Option<VimFindKind> {
        if self.vim_normal_keymap.find_forward.is_pressed(event) {
            Some(VimFindKind::Forward)
        } else if self.vim_normal_keymap.find_backward.is_pressed(event) {
            Some(VimFindKind::Backward)
        } else if self.vim_normal_keymap.till_forward.is_pressed(event) {
            Some(VimFindKind::TillForward)
        } else if self.vim_normal_keymap.till_backward.is_pressed(event) {
            Some(VimFindKind::TillBackward)
        } else {
            None
        }
    }

    pub(super) fn vim_operator_find_kind(&self, event: KeyEvent) -> Option<VimFindKind> {
        if self
            .vim_operator_keymap
            .motion_find_forward
            .is_pressed(event)
        {
            Some(VimFindKind::Forward)
        } else if self
            .vim_operator_keymap
            .motion_find_backward
            .is_pressed(event)
        {
            Some(VimFindKind::Backward)
        } else if self
            .vim_operator_keymap
            .motion_till_forward
            .is_pressed(event)
        {
            Some(VimFindKind::TillForward)
        } else if self
            .vim_operator_keymap
            .motion_till_backward
            .is_pressed(event)
        {
            Some(VimFindKind::TillBackward)
        } else {
            None
        }
    }

    pub(super) fn handle_vim_find_target(
        &mut self,
        operator: Option<VimOperator>,
        kind: VimFindKind,
        count: usize,
        event: KeyEvent,
    ) {
        let Some(target) = vim_target_char(event) else {
            return;
        };
        let find = VimFind { kind, target };
        self.vim_command.last_find = Some(find);
        if let Some(operator) = operator {
            self.apply_vim_operator(operator, VimMotion::Find(find), count);
        } else {
            self.apply_normal_vim_motion(VimMotion::Find(find), count);
        }
    }

    pub(super) fn repeat_vim_find(
        &mut self,
        reverse: bool,
        count: usize,
        operator: Option<VimOperator>,
    ) {
        let Some(mut find) = self.vim_command.last_find else {
            return;
        };
        if reverse {
            find.kind = find.kind.reversed();
        }
        if let Some(operator) = operator {
            self.apply_vim_operator(operator, VimMotion::Find(find), count);
        } else {
            self.apply_normal_vim_motion(VimMotion::Find(find), count);
        }
    }

    pub(super) fn handle_vim_replace_target(&mut self, count: usize, event: KeyEvent) {
        let Some(replacement) = vim_target_char(event) else {
            return;
        };
        let start = self.cursor_pos;
        let eol = self.end_of_current_line();
        let mut end = start;
        let mut replacements = 0;
        for _ in 0..count {
            if end >= eol {
                break;
            }
            end = self.next_atomic_boundary(end).min(eol);
            replacements += 1;
        }
        if start == end {
            return;
        }
        let replaced = replacement.to_string().repeat(replacements);
        let replaced_end = start + replaced.len();
        self.replace_range(start..end, &replaced);
        // Vim leaves the cursor on the last replaced character.
        self.set_cursor(self.prev_atomic_boundary(replaced_end));
    }

    pub(super) fn handle_vim_indent(
        &mut self,
        direction: VimIndentDirection,
        count: usize,
        event: KeyEvent,
    ) {
        let pressed = match direction {
            VimIndentDirection::Increase => self.vim_normal_keymap.indent_lines.is_pressed(event),
            VimIndentDirection::Decrease => self.vim_normal_keymap.dedent_lines.is_pressed(event),
        };
        if pressed {
            self.indent_vim_lines(direction, count);
        }
    }

    pub(super) fn apply_normal_vim_motion(&mut self, motion: VimMotion, count: usize) {
        // Find consumes the whole count in one search: repeating a one-count
        // search would leave `t` re-matching the character it stopped in front
        // of, so `3ta` would never pass the first match.
        if let VimMotion::Find(find) = motion {
            if let Some(target) = self.vim_find_position(find, count) {
                self.set_cursor(target);
            }
            return;
        }
        for _ in 0..count {
            match motion {
                VimMotion::Left => self.move_cursor_left(),
                VimMotion::Right => self.move_cursor_right(),
                VimMotion::Up => self.move_cursor_up(),
                VimMotion::Down => self.move_cursor_down(),
                VimMotion::WordForward => self.set_cursor(self.beginning_of_next_word()),
                VimMotion::WordBackward => self.set_cursor(self.beginning_of_previous_word()),
                VimMotion::WordEnd => self.set_cursor(self.vim_word_end_cursor()),
                VimMotion::BigWordForward => {
                    self.set_cursor(self.beginning_of_next_big_word());
                }
                VimMotion::BigWordBackward => {
                    self.set_cursor(self.beginning_of_previous_big_word());
                }
                VimMotion::BigWordEnd => self.set_cursor(self.vim_big_word_end_cursor()),
                VimMotion::LineStart => self.set_cursor(self.beginning_of_current_line()),
                VimMotion::FirstNonBlank => {
                    self.set_cursor(self.first_non_blank_of_current_line());
                }
                VimMotion::LineEnd => self.set_cursor(self.vim_line_end_cursor()),
                VimMotion::Find(_) => unreachable!("handled before the motion loop"),
            }
        }
    }

    pub(super) fn range_for_motion(
        &mut self,
        motion: VimMotion,
        count: usize,
    ) -> Option<VimOperatorRange> {
        if let VimMotion::Find(find) = motion {
            let start = self.cursor_pos;
            let target = self.vim_find_position(find, count)?;
            if start == target {
                return None;
            }
            let range = if target < start {
                target..self.next_atomic_boundary(start)
            } else {
                start..self.next_atomic_boundary(target)
            };
            return Some(VimOperatorRange {
                range,
                kind: VimRangeKind::Characterwise,
            });
        }
        if matches!(motion, VimMotion::Up | VimMotion::Down) {
            return self
                .linewise_range_for_motion(motion, count)
                .map(|range| VimOperatorRange {
                    range,
                    kind: VimRangeKind::Linewise,
                });
        }
        let start = self.cursor_pos;
        let target = self.target_for_vim_motion(motion, count)?;
        if start == target {
            return None;
        }
        let (range_start, range_end) = if target < start {
            (target, start)
        } else {
            (start, target)
        };
        Some(VimOperatorRange {
            range: range_start..range_end,
            kind: VimRangeKind::Characterwise,
        })
    }

    fn target_for_vim_motion(&mut self, motion: VimMotion, count: usize) -> Option<usize> {
        let original_cursor = self.cursor_pos;
        let original_preferred = self.preferred_col;
        for _ in 0..count {
            match motion {
                VimMotion::Left => self.move_cursor_left(),
                VimMotion::Right => self.move_cursor_right(),
                VimMotion::WordForward => self.set_cursor(self.beginning_of_next_word()),
                VimMotion::WordBackward => self.set_cursor(self.beginning_of_previous_word()),
                VimMotion::WordEnd => self.set_cursor(self.vim_word_end_exclusive()),
                VimMotion::BigWordForward => {
                    self.set_cursor(self.beginning_of_next_big_word());
                }
                VimMotion::BigWordBackward => {
                    self.set_cursor(self.beginning_of_previous_big_word());
                }
                VimMotion::BigWordEnd => self.set_cursor(self.big_word_end_exclusive()),
                VimMotion::LineStart => self.set_cursor(self.beginning_of_current_line()),
                VimMotion::FirstNonBlank => {
                    self.set_cursor(self.first_non_blank_of_current_line());
                }
                VimMotion::LineEnd => self.set_cursor(self.end_of_current_line()),
                VimMotion::Up | VimMotion::Down | VimMotion::Find(_) => {
                    unreachable!("handled before characterwise motion")
                }
            }
        }
        let target = self.cursor_pos;
        self.cursor_pos = original_cursor;
        self.preferred_col = original_preferred;
        Some(target)
    }

    fn linewise_range_for_motion(&self, motion: VimMotion, count: usize) -> Option<Range<usize>> {
        let current_start = self.beginning_of_current_line();
        let target_start = match motion {
            VimMotion::Up => self.step_logical_lines(current_start, count, /*down*/ false),
            VimMotion::Down => self.step_logical_lines(current_start, count, /*down*/ true),
            VimMotion::Left
            | VimMotion::Right
            | VimMotion::WordForward
            | VimMotion::WordBackward
            | VimMotion::WordEnd
            | VimMotion::BigWordForward
            | VimMotion::BigWordBackward
            | VimMotion::BigWordEnd
            | VimMotion::LineStart
            | VimMotion::FirstNonBlank
            | VimMotion::LineEnd
            | VimMotion::Find(_) => return None,
        };
        let first = current_start.min(target_start);
        let last = current_start.max(target_start);
        Some(first..self.line_range_at(last).end)
    }

    pub(super) fn counted_line_range(&self, count: usize) -> Range<usize> {
        let start = self.beginning_of_current_line();
        let last = self.step_logical_lines(start, count.saturating_sub(1), /*down*/ true);
        start..self.line_range_at(last).end
    }

    pub(super) fn move_to_vim_line(&mut self, count: Option<usize>, default_to_last: bool) {
        let line_start = self.vim_line_start(count, default_to_last);
        self.set_cursor(self.first_non_blank_of_line(line_start));
    }

    pub(super) fn apply_vim_operator_to_buffer_line(
        &mut self,
        operator: VimOperator,
        count: Option<usize>,
        default_to_last: bool,
    ) {
        let current = self.beginning_of_current_line();
        let target = self.vim_line_start(count, default_to_last);
        let first = current.min(target);
        let last = current.max(target);
        self.apply_vim_operator_to_range(
            operator,
            VimOperatorRange {
                range: first..self.line_range_at(last).end,
                kind: VimRangeKind::Linewise,
            },
        );
    }

    fn vim_line_start(&self, count: Option<usize>, default_to_last: bool) -> usize {
        let Some(line_number) = count else {
            return if default_to_last {
                self.beginning_of_line(self.text.len())
            } else {
                0
            };
        };
        self.step_logical_lines(
            /*line_start*/ 0,
            line_number.saturating_sub(1),
            /*down*/ true,
        )
    }

    fn line_range_at(&self, line_start: usize) -> Range<usize> {
        let eol = self.end_of_line(line_start);
        line_start..if eol < self.text.len() { eol + 1 } else { eol }
    }

    fn step_logical_lines(&self, mut line_start: usize, count: usize, down: bool) -> usize {
        for _ in 0..count {
            let next = if down {
                let eol = self.end_of_line(line_start);
                if eol >= self.text.len() {
                    line_start
                } else {
                    eol + 1
                }
            } else if line_start == 0 {
                0
            } else {
                self.beginning_of_line(line_start - 1)
            };
            if next == line_start {
                break;
            }
            line_start = next;
        }
        line_start
    }

    fn vim_find_position(&self, find: VimFind, count: usize) -> Option<usize> {
        let bol = self.beginning_of_current_line();
        let eol = self.end_of_current_line();
        let forward = matches!(find.kind, VimFindKind::Forward | VimFindKind::TillForward);
        let mut cursor = self.cursor_pos;
        let mut remaining = count;
        loop {
            cursor = if forward {
                if cursor >= eol {
                    return None;
                }
                self.next_atomic_boundary(cursor)
            } else {
                if cursor <= bol {
                    return None;
                }
                self.prev_atomic_boundary(cursor)
            };
            if cursor < bol || cursor >= eol {
                return None;
            }
            if self.find_element_containing(cursor).is_none()
                && self.text[cursor..].starts_with(find.target)
            {
                remaining = remaining.saturating_sub(1);
                if remaining == 0 {
                    return Some(match find.kind {
                        VimFindKind::Forward | VimFindKind::Backward => cursor,
                        VimFindKind::TillForward => self.prev_atomic_boundary(cursor),
                        VimFindKind::TillBackward => self.next_atomic_boundary(cursor),
                    });
                }
            }
        }
    }

    /// Whether `pos` begins whitespace separating WORDs.
    ///
    /// An element is one indivisible WORD, so its interior never separates,
    /// even though placeholders like `[Pasted Content 12 chars]` hold spaces.
    fn starts_big_word_separator(&self, pos: usize) -> bool {
        if self.is_inside_element(pos) {
            return false;
        }
        self.text[pos..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
    }

    fn beginning_of_next_big_word(&self) -> usize {
        let mut pos = self.cursor_pos;
        let mut saw_whitespace = self.starts_big_word_separator(pos);
        while pos < self.text.len() {
            pos = self.next_atomic_boundary(pos);
            if pos >= self.text.len() {
                break;
            }
            if self.starts_big_word_separator(pos) {
                saw_whitespace = true;
            } else if saw_whitespace {
                return pos;
            }
        }
        self.text.len()
    }

    fn beginning_of_previous_big_word(&self) -> usize {
        let mut pos = self.cursor_pos;
        loop {
            let prev = self.prev_atomic_boundary(pos);
            if prev == pos {
                return 0;
            }
            pos = prev;
            if !self.starts_big_word_separator(pos) {
                break;
            }
        }
        loop {
            let prev = self.prev_atomic_boundary(pos);
            if prev == pos || self.starts_big_word_separator(prev) {
                return pos;
            }
            pos = prev;
        }
    }

    fn big_word_end_exclusive(&self) -> usize {
        self.big_word_end_exclusive_from(self.cursor_pos)
    }

    fn big_word_end_exclusive_from(&self, cursor: usize) -> usize {
        let mut pos = cursor;
        while pos < self.text.len() && self.starts_big_word_separator(pos) {
            pos = self.next_atomic_boundary(pos);
        }
        while pos < self.text.len() && !self.starts_big_word_separator(pos) {
            pos = self.next_atomic_boundary(pos);
        }
        pos
    }

    fn vim_big_word_end_cursor(&self) -> usize {
        let end = self.big_word_end_exclusive();
        let target = if end > self.cursor_pos {
            self.prev_atomic_boundary(end)
        } else {
            end
        };
        if target == self.cursor_pos && end < self.text.len() {
            self.prev_atomic_boundary(self.big_word_end_exclusive_from(end))
        } else {
            target
        }
    }

    pub(super) fn paste_vim(&mut self, after: bool, count: usize) {
        if self.kill_buffer.is_empty() {
            return;
        }
        // A linewise register yanked from the final line has no trailing
        // newline; repeating it raw would run the copies onto one line.
        let linewise = self.kill_buffer_kind == super::KillBufferKind::Linewise;
        let needs_terminator = linewise && !self.kill_buffer.ends_with('\n');
        // Sized before copying anything, so a refused paste stays O(1) even
        // when `.` replays it against a full buffer.
        let unit_len = self.kill_buffer.len() + usize::from(needs_terminator);
        if self
            .text
            .len()
            .saturating_add(unit_len.saturating_mul(count))
            > MAX_VIM_PASTE_BYTES
        {
            return;
        }
        let mut unit = self.kill_buffer.clone();
        if needs_terminator {
            unit.push('\n');
        }
        if linewise {
            self.paste_vim_lines(after, unit.repeat(count));
            return;
        }
        let repeated = unit.repeat(count);
        let insert_at = if after {
            self.next_atomic_boundary(self.cursor_pos)
        } else {
            self.cursor_pos
        };
        self.set_cursor(insert_at);
        self.insert_str(&repeated);
    }

    fn paste_vim_lines(&mut self, after: bool, repeated: String) {
        let bol = self.beginning_of_current_line();
        let eol = self.end_of_current_line();
        // Strip only the structural terminator: a register of blank lines is
        // all newlines, and trimming them all would collapse the paste.
        let payload = repeated.strip_suffix('\n').unwrap_or(&repeated);
        let (insert_at, text, cursor) = if after {
            if eol < self.text.len() {
                (eol + 1, format!("{payload}\n"), eol + 1)
            } else {
                (eol, format!("\n{payload}"), eol + 1)
            }
        } else {
            (bol, format!("{payload}\n"), bol)
        };
        self.insert_str_at(insert_at, &text);
        self.set_cursor(cursor.min(self.text.len()));
    }

    pub(super) fn join_vim_lines(&mut self, line_count: usize) {
        let original_mode = self.vim_mode;
        self.vim_mode = VimMode::Insert;
        let mut cursor = self.end_of_current_line();
        for _ in 1..line_count {
            let eol = self.end_of_current_line();
            if eol >= self.text.len() {
                break;
            }
            let next_start = eol + 1;
            let next_non_blank = self.first_non_blank_of_line(next_start);
            // Vim inserts one separating space, except when the first line
            // already ends in whitespace, the next line is empty, or it opens
            // with `)`.
            let needs_space = eol > self.beginning_of_current_line()
                && next_non_blank < self.end_of_line(next_start)
                && !self.text[..eol].ends_with(char::is_whitespace)
                && !self.text[next_non_blank..].starts_with(')');
            // The cursor lands on the last join.
            cursor = eol;
            self.replace_range(eol..next_non_blank, if needs_space { " " } else { "" });
        }
        self.vim_mode = original_mode;
        self.set_cursor(cursor.min(self.vim_line_end_cursor()));
        self.commit_vim_undo_session();
    }

    fn indent_vim_lines(&mut self, direction: VimIndentDirection, count: usize) {
        let first = self.beginning_of_current_line();
        let mut starts = Vec::new();
        let mut line = first;
        for _ in 0..count {
            starts.push(line);
            let next = self.step_logical_lines(line, /*count*/ 1, /*down*/ true);
            if next == line {
                break;
            }
            line = next;
        }
        let original_mode = self.vim_mode;
        self.vim_mode = VimMode::Insert;
        for start in starts.into_iter().rev() {
            match direction {
                VimIndentDirection::Increase => {
                    if start < self.end_of_line(start) {
                        self.insert_str_at(start, VIM_INDENT);
                    }
                }
                VimIndentDirection::Decrease => {
                    let eol = self.end_of_line(start);
                    let remove = if self.text[start..eol].starts_with('\t') {
                        '\t'.len_utf8()
                    } else {
                        self.text[start..eol]
                            .char_indices()
                            .take_while(|(_, ch)| *ch == ' ')
                            .take(VIM_INDENT.len())
                            .last()
                            .map_or(0, |(idx, ch)| idx + ch.len_utf8())
                    };
                    if remove > 0 {
                        self.replace_range(start..start + remove, "");
                    }
                }
            }
        }
        self.vim_mode = original_mode;
        self.set_cursor(self.first_non_blank_of_line(first));
        self.commit_vim_undo_session();
    }
}

fn vim_target_char(event: KeyEvent) -> Option<char> {
    if !matches!(event.modifiers, KeyModifiers::NONE | KeyModifiers::SHIFT) {
        return None;
    }
    match event.code {
        KeyCode::Char(ch) => Some(ch),
        KeyCode::Enter => Some('\n'),
        KeyCode::Esc
        | KeyCode::Backspace
        | KeyCode::Left
        | KeyCode::Right
        | KeyCode::Up
        | KeyCode::Down
        | KeyCode::Home
        | KeyCode::End
        | KeyCode::PageUp
        | KeyCode::PageDown
        | KeyCode::Tab
        | KeyCode::BackTab
        | KeyCode::Delete
        | KeyCode::Insert
        | KeyCode::F(_)
        | KeyCode::Null
        | KeyCode::CapsLock
        | KeyCode::ScrollLock
        | KeyCode::NumLock
        | KeyCode::PrintScreen
        | KeyCode::Pause
        | KeyCode::Menu
        | KeyCode::KeypadBegin
        | KeyCode::Media(_)
        | KeyCode::Modifier(_) => None,
    }
}
