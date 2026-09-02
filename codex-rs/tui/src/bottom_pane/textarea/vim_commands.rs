//! Semantic Vim editing transactions, character find/till motions, and complete-change replay.
//!
//! `f`/`F` land on a matching grapheme; `t`/`T` stop just before/after it. Forward operator
//! motions include the destination, while backward motions exclude the original cursor.
//! All four motions share these boundaries for navigation, `c`/`d`/`y`, and semantic `.` replay.
//!
//! A count typed before a command is stored with the recorded edit, so `3.` can replace it.

use super::KillBufferKind;
use super::TextArea;
use super::VimMode;
use super::VimMotion;
use super::VimOperator;
use super::VimPending;
use super::VimTextObject;
use super::VimTextObjectScope;
use super::vim::VIM_MAX_COUNT;
use super::vim::VimFind;
use super::vim::VimFindMotion;
use super::vim::VimIndentDirection;
use crate::key_hint::KeyBindingListExt;
use crate::vim_search::SearchQuery;
use codex_protocol::user_input::MAX_USER_INPUT_TEXT_CHARS;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use std::ops::Range;
use unicode_segmentation::GraphemeCursor;
use unicode_segmentation::UnicodeSegmentation;

const VIM_INDENT: &str = "  ";

/// Cap on the bytes a single paste may leave in the composer.
///
/// Neither the count nor the register size is bounded on its own, so `1000p`
/// could otherwise ask for gigabytes. The largest sendable message is
/// `MAX_USER_INPUT_TEXT_CHARS` chars of at most four bytes, so this refuses
/// only pastes that could never be sent regardless, and leaves judging the
/// limit itself to the composer's submit-time check.
const MAX_VIM_PASTE_BYTES: usize = MAX_USER_INPUT_TEXT_CHARS * 4;

#[derive(Clone, Debug)]
pub(crate) enum VimEdit {
    Editor(VimEditorEdit),
    Text(String),
}

/// Vim command recording and searches preserved across same-draft restoration.
#[derive(Debug, Default)]
pub(crate) struct VimPersistentState {
    pub(crate) commands: VimCommandState,
    search: crate::vim_search::SearchQuery,
}

#[derive(Clone, Debug)]
pub(crate) struct VimEditorEdit {
    action: VimAction,
    /// Explicit count typed before the command; `None` means the command default.
    count: Option<usize>,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum VimInsertPosition {
    Cursor,
    AfterCursor,
    LineStart,
    LineEnd,
    OpenAbove,
    OpenBelow,
}

#[derive(Clone, Debug)]
pub(super) enum VimEditTarget {
    Character,
    Line,
    LineEnd,
    Motion(VimMotion),
    Search(SearchQuery),
    TextObject {
        scope: VimTextObjectScope,
        object: VimTextObject,
    },
    Find {
        motion: VimFindMotion,
        target: char,
    },
    BufferJump {
        last: bool,
    },
}

#[derive(Clone, Debug)]
pub(super) enum VimAction {
    Insert(VimInsertPosition),
    EnterReplaceMode,
    RestoreReplacedCharacter,
    Delete(VimEditTarget),
    Change(VimEditTarget),
    Replace(char),
    PasteAfter,
    PasteBefore,
    JoinLines,
    Indent(VimIndentDirection),
    DeleteBackward,
    DeleteForward,
    DeleteBackwardWord,
    DeleteForwardWord,
    KillLineStart,
    KillLine,
    KillLineEnd,
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    MoveWordLeft,
    MoveWordRight,
    MoveLineStart { move_up_at_bol: bool },
    MoveLineEnd { move_down_at_eol: bool },
}

impl VimAction {
    /// Whether `N.` should replace this command's count rather than replay the whole change.
    const fn accepts_count(&self) -> bool {
        matches!(
            self,
            Self::Delete(_)
                | Self::Change(_)
                | Self::Replace(_)
                | Self::PasteAfter
                | Self::PasteBefore
                | Self::JoinLines
                | Self::Indent(_)
        )
    }
}

#[derive(Debug, Default)]
pub(crate) struct VimCommandState {
    pub(super) pending_change: Vec<VimEdit>,
    pub(crate) last_change: Vec<VimEdit>,
    changed: bool,
    pub(super) replaying: bool,
    replace_steps: Vec<VimReplaceStep>,
    pub(super) last_find: Option<VimFind>,
}

#[derive(Debug)]
struct VimReplaceStep {
    // Backspace also retraces attachments skipped before the replacement.
    cursor_before: usize,
    start: usize,
    inserted_len: usize,
    original: String,
}

pub(super) fn vim_count_digit(event: KeyEvent) -> Option<u8> {
    if !matches!(event.modifiers, KeyModifiers::NONE) {
        return None;
    }
    match event.code {
        KeyCode::Char(ch) if ch.is_ascii_digit() => Some(ch as u8 - b'0'),
        _ => None,
    }
}

pub(super) fn push_vim_count_digit(count: &mut Option<usize>, digit: u8) {
    let next = count
        .unwrap_or(0)
        .saturating_mul(10)
        .saturating_add(usize::from(digit))
        .min(VIM_MAX_COUNT);
    *count = Some(next);
}

/// Multiply an operator count by a motion count (`2d3w` deletes six words).
pub(super) fn combined_vim_count(
    operator_count: Option<usize>,
    motion_count: Option<usize>,
) -> Option<usize> {
    if operator_count.is_none() && motion_count.is_none() {
        return None;
    }
    Some(
        operator_count
            .unwrap_or(1)
            .saturating_mul(motion_count.unwrap_or(1))
            .min(VIM_MAX_COUNT),
    )
}

impl TextArea {
    pub(crate) fn is_vim_replace_mode(&self) -> bool {
        self.vim_enabled && self.vim_mode == VimMode::Replace
    }

    pub(super) fn clear_vim_replace_recovery(&mut self) {
        self.vim_commands.replace_steps.clear();
    }

    pub(super) fn replace_vim_text(&mut self, text: &str) {
        for grapheme in text.graphemes(/*is_extended*/ true) {
            let cursor_before = self.cursor_pos;
            while let Some(element) = self
                .elements
                .iter()
                .find(|element| element.range.start == self.cursor_pos)
            {
                self.set_cursor(element.range.end);
            }
            let start = self.cursor_pos;
            let end = if grapheme == "\n" || start >= self.end_of_current_line() {
                start
            } else {
                self.next_atomic_boundary(start)
            };
            let original = self.text[start..end].to_string();
            self.replace_range_preserving_recovery(start..end, grapheme);
            self.vim_commands.replace_steps.push(VimReplaceStep {
                cursor_before,
                start,
                inserted_len: grapheme.len(),
                original,
            });
        }
    }

    pub(super) fn restore_vim_replaced_character(&mut self) -> bool {
        let steps = &mut self.vim_commands.replace_steps;
        let Some(step) = steps
            .pop()
            .filter(|step| self.cursor_pos == step.start + step.inserted_len)
        else {
            steps.clear();
            return false;
        };
        // Replace skips existing elements, so an overlapping marker was added after typing.
        // Unmark it before restoring one character; never expand recovery to the whole token.
        self.elements.retain(|element| {
            element.range.end <= step.start || element.range.start >= step.start + step.inserted_len
        });
        self.replace_range_preserving_recovery(
            step.start..step.start + step.inserted_len,
            &step.original,
        );
        self.set_cursor(step.cursor_before);
        true
    }

    /// Retract a detected paste prefix without losing overwritten text or crossing attachments.
    pub(crate) fn retract_paste_burst(&mut self, start: usize) -> bool {
        if self.is_vim_replace_mode() {
            let restored_start = self
                .vim_commands
                .replace_steps
                .iter()
                .rev()
                .take_while(|step| step.start >= start)
                .try_fold(self.cursor_pos, |cursor, step| {
                    (step.start + step.inserted_len == cursor && step.cursor_before == step.start)
                        .then_some(step.start)
                });
            if restored_start != Some(start) {
                return false;
            }
            while self.cursor_pos > start {
                self.apply_vim_insert_action(VimAction::RestoreReplacedCharacter);
            }
        } else {
            self.replace_range(start..self.cursor_pos, "");
        }
        true
    }

    pub(crate) fn swap_vim_persistent_state(&mut self, state: &mut VimPersistentState) {
        std::mem::swap(&mut self.vim_commands, &mut state.commands);
        std::mem::swap(&mut self.vim_search.last, &mut state.search);
    }

    pub(crate) fn vim_repeat_actions(&self) -> Option<Vec<VimEdit>> {
        (!self.vim_commands.last_change.is_empty()).then(|| self.vim_commands.last_change.clone())
    }

    pub(super) fn record_vim_inserted_text(&mut self, text: &str) {
        if !self.vim_enabled
            || !matches!(self.vim_mode, VimMode::Insert | VimMode::Replace)
            || self.vim_commands.replaying
            || self.vim_commands.pending_change.is_empty()
            || text.is_empty()
        {
            return;
        }
        if self.vim_mode == VimMode::Insert
            && let Some(VimEdit::Text(pending)) = self.vim_commands.pending_change.last_mut()
        {
            pending.push_str(text);
        } else {
            self.vim_commands
                .pending_change
                .push(VimEdit::Text(text.to_owned()));
        }
        self.vim_commands.changed = true;
    }

    pub(super) fn apply_vim_insert_action(&mut self, action: VimAction) -> bool {
        let recording = self.vim_enabled
            && matches!(self.vim_mode, VimMode::Insert | VimMode::Replace)
            && !self.vim_commands.replaying
            && !self.vim_commands.pending_change.is_empty();
        let prior_len = self.text.len();
        if !self.apply_vim_editor_action(action.clone(), /*count*/ None) {
            return false;
        }
        let changed =
            self.text.len() != prior_len || matches!(action, VimAction::RestoreReplacedCharacter);
        let deletion = matches!(
            action,
            VimAction::DeleteBackward
                | VimAction::DeleteForward
                | VimAction::DeleteBackwardWord
                | VimAction::DeleteForwardWord
                | VimAction::KillLineStart
                | VimAction::KillLine
                | VimAction::KillLineEnd
        );
        if recording && (changed || !deletion) {
            self.vim_commands
                .pending_change
                .push(VimEdit::Editor(VimEditorEdit {
                    action,
                    count: None,
                }));
        }
        self.vim_commands.changed |= recording && changed;
        true
    }

    pub(super) fn start_vim_edit(&mut self, action: VimAction, count: Option<usize>) -> bool {
        let prior_len = self.text.len();
        self.vim_commands.pending_change = vec![VimEdit::Editor(VimEditorEdit {
            action: action.clone(),
            count,
        })];
        self.vim_commands.changed = false;
        if !self.apply_vim_editor_action(action.clone(), count) {
            self.vim_commands.pending_change.clear();
            return false;
        }
        self.vim_commands.changed = self.text.len() != prior_len
            || matches!(
                action,
                VimAction::Replace(_) | VimAction::JoinLines | VimAction::Indent(_)
            );
        if self.vim_mode == VimMode::Normal {
            self.finish_pending_vim_change();
        }
        true
    }

    pub(super) fn finish_pending_vim_change(&mut self) {
        if self.vim_commands.changed {
            self.vim_commands.last_change = std::mem::take(&mut self.vim_commands.pending_change);
        } else {
            self.vim_commands.pending_change.clear();
        }
        self.vim_commands.changed = false;
    }

    pub(crate) fn begin_vim_repeat(&mut self) -> Option<Vec<VimEdit>> {
        let edits = self.vim_repeat_actions()?;
        self.vim_commands.replaying = true;
        Some(edits)
    }

    pub(crate) fn finish_vim_repeat(&mut self) {
        if matches!(self.vim_mode, VimMode::Insert | VimMode::Replace) {
            self.leave_vim_insert_mode();
        }
        self.vim_pending = VimPending::None;
        self.vim_commands.replaying = false;
    }

    /// Replay the last change. `N.` replaces the recorded count of a counted
    /// command, and repeats an insert session `N` times.
    pub(super) fn repeat_last_vim_change(&mut self, count: Option<usize>) {
        let Some(mut edits) = self.begin_vim_repeat() else {
            return;
        };
        let mut times = 1;
        if let Some(n) = count {
            match edits.first_mut() {
                Some(VimEdit::Editor(first)) if first.action.accepts_count() => {
                    first.count = Some(n);
                    self.vim_commands.last_change = edits.clone();
                }
                _ => times = n,
            }
        }
        'replay: for _ in 0..times {
            for edit in &edits {
                if !self.apply_vim_edit(edit) {
                    break 'replay;
                }
            }
        }
        self.finish_vim_repeat();
    }

    pub(crate) fn apply_vim_edit(&mut self, edit: &VimEdit) -> bool {
        match edit {
            VimEdit::Editor(VimEditorEdit { action, count }) => {
                self.apply_vim_editor_action(action.clone(), *count)
            }
            VimEdit::Text(text) => {
                if !matches!(self.vim_mode, VimMode::Insert | VimMode::Replace) {
                    return false;
                }
                self.insert_str(text);
                true
            }
        }
    }

    fn apply_vim_editor_action(&mut self, action: VimAction, count: Option<usize>) -> bool {
        // Editor actions invalidate contiguous Replace offsets, including during replay.
        if !matches!(action, VimAction::RestoreReplacedCharacter) {
            self.clear_vim_replace_recovery();
        }
        let prior_len = self.text.len();
        let n = count.unwrap_or(1).max(1);
        let is_change = matches!(action, VimAction::Change(_));
        match action {
            VimAction::EnterReplaceMode => {
                self.vim_mode = VimMode::Replace;
            }
            VimAction::RestoreReplacedCharacter => {
                return self.restore_vim_replaced_character();
            }
            VimAction::Insert(position) => {
                match position {
                    VimInsertPosition::Cursor => {}
                    VimInsertPosition::AfterCursor => {
                        self.set_cursor(self.next_atomic_boundary(self.cursor_pos));
                    }
                    VimInsertPosition::LineStart => {
                        self.set_cursor(self.first_non_blank_of_current_line());
                    }
                    VimInsertPosition::LineEnd => self.set_cursor(self.end_of_current_line()),
                    VimInsertPosition::OpenAbove => {
                        let bol = self.beginning_of_current_line();
                        self.insert_str_at(bol, "\n");
                        self.set_cursor(bol);
                    }
                    VimInsertPosition::OpenBelow => {
                        let eol = self.end_of_current_line();
                        let insert_at = if eol < prior_len { eol + 1 } else { eol };
                        self.insert_str_at(insert_at, "\n");
                        self.set_cursor(if eol < prior_len {
                            insert_at
                        } else {
                            insert_at + 1
                        });
                    }
                }
                self.vim_mode = VimMode::Insert;
            }
            VimAction::Delete(target) | VimAction::Change(target) => {
                let operator = if !is_change {
                    VimOperator::Delete
                } else {
                    VimOperator::Change
                };
                match target {
                    VimEditTarget::Character => {
                        let eol = self.end_of_current_line();
                        let mut end = self.cursor_pos;
                        for _ in 0..n {
                            if end >= eol {
                                break;
                            }
                            end = self.next_atomic_boundary(end).min(eol);
                        }
                        if end > self.cursor_pos {
                            self.kill_range(self.cursor_pos..end);
                        }
                        if operator == VimOperator::Change {
                            self.vim_mode = VimMode::Insert;
                        }
                    }
                    VimEditTarget::Line => {
                        let range = self.counted_line_range(n);
                        if operator == VimOperator::Delete {
                            self.kill_line_range(range);
                        } else {
                            // `cc` keeps the final newline so one empty line remains.
                            let end = if self.text[..range.end].ends_with('\n') {
                                range.end - 1
                            } else {
                                range.end
                            };
                            self.kill_line_range(range.start..end);
                            self.vim_mode = VimMode::Insert;
                        }
                    }
                    VimEditTarget::LineEnd => {
                        self.vim_kill_to_end_of_line();
                        if operator == VimOperator::Change {
                            self.vim_mode = VimMode::Insert;
                        }
                    }
                    VimEditTarget::Motion(motion) => self.apply_vim_operator(operator, motion, n),
                    VimEditTarget::Search(query) => {
                        if !self.apply_vim_search(&query, Some(operator)) {
                            return false;
                        }
                    }
                    VimEditTarget::TextObject { scope, object } => {
                        let Some(range) = self.text_object_range(object, scope) else {
                            return false;
                        };
                        self.apply_vim_operator_to_range(operator, range);
                    }
                    VimEditTarget::Find { motion, target } => {
                        if !self.find_vim_character(motion, Some(operator), target, n) {
                            return false;
                        }
                    }
                    VimEditTarget::BufferJump { last } => {
                        self.jump_to_vim_buffer_line(last, Some(operator), count);
                    }
                }
                if operator == VimOperator::Change {
                    return self.vim_mode == VimMode::Insert;
                }
                return self.text.len() != prior_len;
            }
            VimAction::Replace(ch) => {
                let start = self.cursor_pos;
                let eol = self.end_of_current_line();
                let mut end = start;
                for _ in 0..n {
                    if end >= eol {
                        // Vim refuses `5rx` when fewer than five characters remain.
                        return false;
                    }
                    end = self.next_atomic_boundary(end).min(eol);
                }
                let replacement = ch.to_string().repeat(n);
                self.replace_range(start..end, &replacement);
                if ch == '\n' {
                    self.set_cursor(start + replacement.len());
                } else {
                    // Vim leaves the cursor on the last replaced character.
                    self.set_cursor(start + replacement.len() - ch.len_utf8());
                }
            }
            VimAction::PasteAfter => {
                self.paste_vim(/*after*/ true, n);
                return self.text.len() != prior_len;
            }
            VimAction::PasteBefore => {
                self.paste_vim(/*after*/ false, n);
                return self.text.len() != prior_len;
            }
            VimAction::JoinLines => {
                return self.join_vim_lines(count.unwrap_or(2).max(2));
            }
            VimAction::Indent(direction) => {
                return self.indent_vim_lines(direction, n);
            }
            VimAction::DeleteBackward => self.delete_backward(/*n*/ 1),
            VimAction::DeleteForward => self.delete_forward(/*n*/ 1),
            VimAction::DeleteBackwardWord => self.delete_backward_word(),
            VimAction::DeleteForwardWord => self.delete_forward_word(),
            VimAction::KillLineStart => self.kill_to_beginning_of_line(),
            VimAction::KillLine => self.kill_current_line(),
            VimAction::KillLineEnd => self.kill_to_end_of_line(),
            VimAction::MoveLeft => self.move_cursor_left(),
            VimAction::MoveRight => self.move_cursor_right(),
            VimAction::MoveUp => self.move_cursor_up(),
            VimAction::MoveDown => self.move_cursor_down(),
            VimAction::MoveWordLeft => self.set_cursor(self.beginning_of_previous_word()),
            VimAction::MoveWordRight => self.set_cursor(self.end_of_next_word()),
            VimAction::MoveLineStart { move_up_at_bol } => {
                self.move_cursor_to_beginning_of_line(move_up_at_bol);
            }
            VimAction::MoveLineEnd { move_down_at_eol } => {
                self.move_cursor_to_end_of_line(move_down_at_eol);
            }
        }
        true
    }

    /// Normal-mode commands that live outside the core motion/operator dispatch.
    pub(super) fn handle_vim_extra_command(
        &mut self,
        event: KeyEvent,
        count: Option<usize>,
    ) -> bool {
        let n = count.unwrap_or(1);
        if self.vim_normal_keymap.enter_replace_mode.is_pressed(event) {
            self.start_vim_edit(VimAction::EnterReplaceMode, /*count*/ None);
            return true;
        }

        if self.vim_normal_keymap.replace_char.is_pressed(event)
            && self.cursor_pos < self.end_of_current_line()
        {
            self.vim_pending = VimPending::Replace { count };
            return true;
        }
        if self.vim_normal_keymap.repeat_last_change.is_pressed(event) {
            self.repeat_last_vim_change(count);
            return true;
        }
        if self.vim_normal_keymap.paste_before.is_pressed(event) {
            self.start_vim_edit(VimAction::PasteBefore, count);
            return true;
        }
        if self.vim_normal_keymap.join_lines.is_pressed(event) {
            self.start_vim_edit(VimAction::JoinLines, count);
            return true;
        }
        if self.vim_normal_keymap.indent_lines.is_pressed(event) {
            self.vim_pending = VimPending::Indent {
                direction: VimIndentDirection::Increase,
                count,
            };
            return true;
        }
        if self.vim_normal_keymap.dedent_lines.is_pressed(event) {
            self.vim_pending = VimPending::Indent {
                direction: VimIndentDirection::Decrease,
                count,
            };
            return true;
        }
        if self.vim_normal_keymap.find_forward.is_pressed(event) {
            self.start_vim_find(VimFindMotion::Forward, /*operator*/ None, count);
        } else if self.vim_normal_keymap.find_backward.is_pressed(event) {
            self.start_vim_find(VimFindMotion::Backward, /*operator*/ None, count);
        } else if self.vim_normal_keymap.till_forward.is_pressed(event) {
            self.start_vim_find(VimFindMotion::TillForward, /*operator*/ None, count);
        } else if self.vim_normal_keymap.till_backward.is_pressed(event) {
            self.start_vim_find(VimFindMotion::TillBackward, /*operator*/ None, count);
        } else if self.vim_normal_keymap.repeat_find.is_pressed(event) {
            self.repeat_vim_find(/*reverse*/ false, /*operator*/ None, n);
        } else if self.vim_normal_keymap.repeat_find_reverse.is_pressed(event) {
            self.repeat_vim_find(/*reverse*/ true, /*operator*/ None, n);
        } else if self.vim_normal_keymap.jump_top.is_pressed(event) {
            self.jump_to_vim_buffer_line(/*last*/ false, /*operator*/ None, count);
        } else if self.vim_normal_keymap.jump_bottom.is_pressed(event) {
            self.jump_to_vim_buffer_line(/*last*/ true, /*operator*/ None, count);
        } else {
            return false;
        }
        true
    }

    pub(super) fn handle_vim_operator_command(
        &mut self,
        operator: VimOperator,
        event: KeyEvent,
        count: Option<usize>,
    ) -> bool {
        let keymap = &self.vim_operator_keymap;
        if keymap.motion_find_forward.is_pressed(event) {
            self.start_vim_find(VimFindMotion::Forward, Some(operator), count);
        } else if keymap.motion_find_backward.is_pressed(event) {
            self.start_vim_find(VimFindMotion::Backward, Some(operator), count);
        } else if keymap.motion_till_forward.is_pressed(event) {
            self.start_vim_find(VimFindMotion::TillForward, Some(operator), count);
        } else if keymap.motion_till_backward.is_pressed(event) {
            self.start_vim_find(VimFindMotion::TillBackward, Some(operator), count);
        } else if keymap.motion_repeat_find.is_pressed(event) {
            self.repeat_vim_find(/*reverse*/ false, Some(operator), count.unwrap_or(1));
        } else if keymap.motion_repeat_find_reverse.is_pressed(event) {
            self.repeat_vim_find(/*reverse*/ true, Some(operator), count.unwrap_or(1));
        } else if keymap.motion_jump_top.is_pressed(event)
            || keymap.motion_jump_bottom.is_pressed(event)
        {
            let last = keymap.motion_jump_bottom.is_pressed(event);
            match operator {
                VimOperator::Delete => {
                    self.start_vim_edit(
                        VimAction::Delete(VimEditTarget::BufferJump { last }),
                        count,
                    );
                }
                VimOperator::Change => {
                    self.start_vim_edit(
                        VimAction::Change(VimEditTarget::BufferJump { last }),
                        count,
                    );
                }
                VimOperator::Yank => self.jump_to_vim_buffer_line(last, Some(operator), count),
            }
        } else {
            return false;
        }
        true
    }

    pub(super) fn handle_vim_pending_command(&mut self, pending: VimPending, event: KeyEvent) {
        match pending {
            VimPending::Replace { count } => {
                if let Some(ch) = vim_command_char(event) {
                    self.start_vim_edit(VimAction::Replace(ch), count);
                }
            }
            VimPending::Find {
                motion,
                operator,
                count,
            } => {
                if let Some(ch) = vim_command_char(event) {
                    self.vim_commands.last_find = Some(VimFind { motion, target: ch });
                    self.run_vim_find(motion, operator, ch, count);
                }
            }
            VimPending::Indent { direction, count } => {
                let pressed = match direction {
                    VimIndentDirection::Increase => {
                        self.vim_normal_keymap.indent_lines.is_pressed(event)
                    }
                    VimIndentDirection::Decrease => {
                        self.vim_normal_keymap.dedent_lines.is_pressed(event)
                    }
                };
                if pressed {
                    self.start_vim_edit(VimAction::Indent(direction), count);
                }
            }
            VimPending::None | VimPending::Operator { .. } | VimPending::TextObject { .. } => {}
        }
    }

    fn start_vim_find(
        &mut self,
        motion: VimFindMotion,
        operator: Option<VimOperator>,
        count: Option<usize>,
    ) {
        self.vim_pending = VimPending::Find {
            motion,
            operator,
            count,
        };
    }

    /// Run a find as a recorded edit (`d`/`c`) or a plain motion (`y`/navigation).
    fn run_vim_find(
        &mut self,
        motion: VimFindMotion,
        operator: Option<VimOperator>,
        target: char,
        count: Option<usize>,
    ) {
        match operator {
            Some(VimOperator::Delete) => {
                self.start_vim_edit(
                    VimAction::Delete(VimEditTarget::Find { motion, target }),
                    count,
                );
            }
            Some(VimOperator::Change) => {
                self.start_vim_edit(
                    VimAction::Change(VimEditTarget::Find { motion, target }),
                    count,
                );
            }
            Some(VimOperator::Yank) | None => {
                self.find_vim_character(motion, operator, target, count.unwrap_or(1));
            }
        }
    }

    /// `;` and `,` replay the last `f`/`F`/`t`/`T`; a till repeat skips an adjacent match.
    fn repeat_vim_find(&mut self, reverse: bool, operator: Option<VimOperator>, count: usize) {
        let Some(find) = self.vim_commands.last_find else {
            return;
        };
        let motion = if reverse {
            find.motion.reversed()
        } else {
            find.motion
        };
        let count = if matches!(
            motion,
            VimFindMotion::TillForward | VimFindMotion::TillBackward
        ) && self.vim_till_target_is_adjacent(motion, find.target)
        {
            count.saturating_add(1)
        } else {
            count
        };
        self.run_vim_find(motion, operator, find.target, Some(count));
    }

    fn vim_till_target_is_adjacent(&self, motion: VimFindMotion, target: char) -> bool {
        if motion.is_forward() {
            let next = self.next_atomic_boundary(self.cursor_pos);
            next < self.end_of_current_line()
                && self.is_vim_command_target(next)
                && self.text[next..].starts_with(target)
        } else {
            let bol = self.beginning_of_current_line();
            if self.cursor_pos <= bol {
                return false;
            }
            let prev = self.prev_atomic_boundary(self.cursor_pos);
            prev >= bol && self.is_vim_command_target(prev) && self.text[prev..].starts_with(target)
        }
    }

    fn find_vim_character(
        &mut self,
        motion: VimFindMotion,
        operator: Option<VimOperator>,
        target: char,
        count: usize,
    ) -> bool {
        let origin = self.cursor_pos;
        let count = count.max(1);
        let found = match motion {
            VimFindMotion::Forward | VimFindMotion::TillForward => {
                let line_end = self.end_of_current_line();
                if origin >= line_end {
                    return false;
                }
                let start = self.next_atomic_boundary(origin);
                self.text[start..line_end]
                    .grapheme_indices(/*is_extended*/ true)
                    .filter(|(offset, grapheme)| {
                        grapheme.starts_with(target) && self.is_vim_command_target(start + offset)
                    })
                    .nth(count - 1)
                    .map(|(offset, grapheme)| start + offset..start + offset + grapheme.len())
            }
            VimFindMotion::Backward | VimFindMotion::TillBackward => {
                let line_start = self.beginning_of_current_line();
                self.text[line_start..origin]
                    .grapheme_indices(/*is_extended*/ true)
                    .rev()
                    .filter(|(offset, grapheme)| {
                        grapheme.starts_with(target)
                            && self.is_vim_command_target(line_start + offset)
                    })
                    .nth(count - 1)
                    .map(|(offset, grapheme)| {
                        let start = line_start + offset;
                        start..start + grapheme.len()
                    })
            }
        };
        let Some(position) = found else {
            return false;
        };
        if let Some(operator) = operator {
            let range = match motion {
                VimFindMotion::Forward => origin..position.end,
                VimFindMotion::Backward => position.start..origin,
                VimFindMotion::TillForward => origin..position.start,
                VimFindMotion::TillBackward => position.end..origin,
            };
            if operator == VimOperator::Yank {
                self.set_cursor(range.start);
            }
            self.apply_vim_operator_to_range(operator, range);
        } else {
            let destination = match motion {
                VimFindMotion::Forward | VimFindMotion::Backward => position.start,
                VimFindMotion::TillForward => {
                    let previous = self.text[..position.start]
                        .grapheme_indices(/*is_extended*/ true)
                        .next_back()
                        .map_or(origin, |(offset, _)| offset);
                    self.find_element_containing(previous)
                        .map_or(previous, |idx| self.elements[idx].range.start)
                }
                VimFindMotion::TillBackward => position.end,
            };
            self.set_cursor(destination);
        }
        true
    }

    /// `gg`/`G` without a count target the first/last line; `NG` and `Ngg` target line `N`.
    fn jump_to_vim_buffer_line(
        &mut self,
        last: bool,
        operator: Option<VimOperator>,
        count: Option<usize>,
    ) {
        let target_start = match count {
            Some(line) => self.step_logical_lines(0, line.saturating_sub(1), /*down*/ true),
            None if last => self.beginning_of_line(self.text.len()),
            None => 0,
        };
        if let Some(operator) = operator {
            let current = self.beginning_of_current_line();
            let first = current.min(target_start);
            let last_line = current.max(target_start);
            let range = if count.is_none() && last {
                current..self.text.len()
            } else {
                first..self.line_range_with_newline(last_line).end
            };
            match operator {
                VimOperator::Delete => self.kill_line_range(range),
                VimOperator::Yank => self.yank_line_range(range),
                VimOperator::Change => {
                    self.kill_line_range(range);
                    self.vim_mode = VimMode::Insert;
                }
            }
            return;
        }
        self.set_cursor(target_start);
        self.set_cursor(self.first_non_blank_of_current_line());
    }

    pub(super) fn is_vim_command_target(&self, position: usize) -> bool {
        !self
            .elements
            .iter()
            .any(|element| element.range.contains(&position))
            && GraphemeCursor::new(position, self.text.len(), /*is_extended*/ true)
                .is_boundary(&self.text, /*chunk_start*/ 0)
                .unwrap_or(false)
    }

    // ---- Line arithmetic shared by counted linewise commands ----

    /// Byte range of the logical line starting at `line_start`, including its newline.
    pub(super) fn line_range_with_newline(&self, line_start: usize) -> Range<usize> {
        let eol = self.end_of_line(line_start);
        line_start..if eol < self.text.len() { eol + 1 } else { eol }
    }

    /// Walk `count` logical lines from `line_start`, stopping at the buffer edge.
    pub(super) fn step_logical_lines(
        &self,
        mut line_start: usize,
        count: usize,
        down: bool,
    ) -> usize {
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

    /// The current line through `count - 1` lines below it, newlines included.
    pub(super) fn counted_line_range(&self, count: usize) -> Range<usize> {
        let start = self.beginning_of_current_line();
        let last = self.step_logical_lines(start, count.saturating_sub(1), /*down*/ true);
        start..self.line_range_with_newline(last).end
    }

    // ---- WORD motions ----

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

    pub(super) fn beginning_of_next_big_word(&self) -> usize {
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

    pub(super) fn beginning_of_previous_big_word(&self) -> usize {
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

    pub(super) fn big_word_end_exclusive(&self) -> usize {
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

    pub(super) fn vim_big_word_end_cursor(&self) -> usize {
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

    // ---- Paste, join, indent ----

    pub(super) fn paste_vim(&mut self, after: bool, count: usize) {
        if self.kill_buffer.is_empty() {
            return;
        }
        // A linewise register yanked from the final line has no trailing
        // newline; repeating it raw would run the copies onto one line.
        let linewise = self.kill_buffer_kind == KillBufferKind::Linewise;
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
        let repeated = unit.repeat(count);
        if linewise {
            self.paste_vim_lines(after, repeated);
            return;
        }
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

    /// Join `line_count` lines starting at the cursor line; returns whether anything joined.
    fn join_vim_lines(&mut self, line_count: usize) -> bool {
        let mut joined = false;
        let mut cursor = self.cursor_pos;
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
            joined = true;
        }
        if joined {
            self.set_cursor(cursor.min(self.vim_line_end_cursor()));
        }
        joined
    }

    /// Shift `count` lines from the cursor line by one indent level; returns whether any changed.
    fn indent_vim_lines(&mut self, direction: VimIndentDirection, count: usize) -> bool {
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
        let mut changed = false;
        for start in starts.into_iter().rev() {
            match direction {
                VimIndentDirection::Increase => {
                    if start < self.end_of_line(start) {
                        self.insert_str_at(start, VIM_INDENT);
                        changed = true;
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
                        changed = true;
                    }
                }
            }
        }
        self.set_cursor(self.first_non_blank_of_line(first));
        changed
    }

    fn first_non_blank_of_line(&self, line_start: usize) -> usize {
        let eol = self.end_of_line(line_start);
        self.text[line_start..eol]
            .char_indices()
            .find_map(|(offset, ch)| (!ch.is_whitespace()).then_some(line_start + offset))
            .unwrap_or(eol)
    }
}

fn vim_command_char(event: KeyEvent) -> Option<char> {
    if event.code == KeyCode::Enter {
        return Some('\n');
    }
    let KeyCode::Char(ch) = event.code else {
        return None;
    };
    match event.modifiers {
        KeyModifiers::NONE => Some(ch),
        KeyModifiers::SHIFT => Some(if ch.is_ascii_lowercase() {
            ch.to_ascii_uppercase()
        } else {
            ch
        }),
        modifiers if crate::key_hint::is_altgr(modifiers) => Some(ch),
        _ => None,
    }
}

#[cfg(test)]
#[path = "vim_commands_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "vim_extended_tests.rs"]
mod extended_tests;
