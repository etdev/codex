use super::TextArea;
use super::split_word_pieces;
use crate::key_hint::KeyBindingListExt;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use std::collections::VecDeque;
use std::ops::Range;

const VIM_UNDO_LIMIT: usize = 100;

/// Bounds counts so a long digit run cannot freeze the composer or exhaust
/// memory. Counts scale loop iterations and `repeat` allocations, and a
/// composer draft is never long enough to address more than this.
pub(super) const VIM_MAX_COUNT: usize = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VimMode {
    /// Normal mode routes printable keys to movement, operators, and mode transitions.
    Normal,
    /// Insert mode routes input through the regular editor keymap until Escape is pressed.
    Insert,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VimOperator {
    Delete,
    Yank,
    Change,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VimPending {
    None,
    BufferStart {
        operator: Option<VimOperator>,
        count: Option<usize>,
    },
    Operator {
        operator: VimOperator,
        operator_count: Option<usize>,
        motion_count: Option<usize>,
    },
    TextObject {
        operator: VimOperator,
        scope: VimTextObjectScope,
        count: usize,
    },
    Find {
        operator: Option<VimOperator>,
        kind: VimFindKind,
        count: usize,
    },
    ReplaceChar {
        count: usize,
    },
    Indent {
        direction: VimIndentDirection,
        count: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VimRangeKind {
    Characterwise,
    Linewise,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct VimOperatorRange {
    pub(super) range: Range<usize>,
    pub(super) kind: VimRangeKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VimUndoSnapshot {
    text: String,
    cursor_pos: usize,
    elements: Vec<super::TextElement>,
}

#[derive(Debug, Default)]
pub(super) struct VimUndoState {
    history: VecDeque<VimUndoSnapshot>,
    active: Option<VimUndoSnapshot>,
}

/// A key captured for dot-repeat, tagged if it was consumed as a count digit.
///
/// Rescanning for digits later cannot recover the tag, since a digit may be an
/// argument instead, as in `r2`.
#[derive(Debug, Clone, Copy)]
struct VimRecordedKey {
    event: KeyEvent,
    is_count: bool,
}

#[derive(Debug, Default)]
pub(super) struct VimCommandState {
    pub(super) count: Option<usize>,
    pub(super) last_find: Option<VimFind>,
    pending_keys: Vec<VimRecordedKey>,
    recording: Option<Vec<VimRecordedKey>>,
    last_change: Vec<VimRecordedKey>,
    replaying: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VimMotion {
    Left,
    Right,
    Up,
    Down,
    WordForward,
    WordBackward,
    WordEnd,
    BigWordForward,
    BigWordBackward,
    BigWordEnd,
    LineStart,
    FirstNonBlank,
    LineEnd,
    Find(VimFind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VimFindKind {
    Forward,
    Backward,
    TillForward,
    TillBackward,
}

impl VimFindKind {
    pub(super) const fn reversed(self) -> Self {
        match self {
            Self::Forward => Self::Backward,
            Self::Backward => Self::Forward,
            Self::TillForward => Self::TillBackward,
            Self::TillBackward => Self::TillForward,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct VimFind {
    pub(super) kind: VimFindKind,
    pub(super) target: char,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VimIndentDirection {
    Increase,
    Decrease,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VimTextObjectScope {
    Inner,
    Around,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VimTextObject {
    Word,
    BigWord,
    Parentheses,
    Brackets,
    Braces,
    DoubleQuote,
    SingleQuote,
    Backtick,
}

impl TextArea {
    pub(super) fn clear_vim_command_state(&mut self) {
        self.vim_command = VimCommandState::default();
        self.vim_pending = VimPending::None;
    }

    pub(super) fn begin_vim_key(&mut self, event: KeyEvent) {
        if self.vim_command.replaying {
            return;
        }
        let key = VimRecordedKey {
            event,
            is_count: false,
        };
        if let Some(recording) = self.vim_command.recording.as_mut() {
            recording.push(key);
        } else {
            self.vim_command.pending_keys.push(key);
        }
    }

    /// Tag the most recently recorded key as a count digit.
    pub(super) fn mark_vim_key_as_count(&mut self) {
        if self.vim_command.replaying {
            return;
        }
        let keys = self
            .vim_command
            .recording
            .as_mut()
            .unwrap_or(&mut self.vim_command.pending_keys);
        if let Some(key) = keys.last_mut() {
            key.is_count = true;
        }
    }

    pub(super) fn finish_vim_key(&mut self) {
        if self.vim_command.replaying
            || self.vim_command.recording.is_some()
            || self.vim_mode != VimMode::Normal
            || !matches!(self.vim_pending, VimPending::None)
            || self.vim_command.count.is_some()
        {
            return;
        }
        self.vim_command.pending_keys.clear();
    }

    fn begin_vim_change_recording(&mut self) {
        if self.vim_command.replaying || self.vim_command.recording.is_some() {
            return;
        }
        self.vim_command.recording = Some(std::mem::take(&mut self.vim_command.pending_keys));
    }

    fn finish_vim_change_recording(&mut self, changed: bool) {
        let Some(recording) = self.vim_command.recording.take() else {
            return;
        };
        if changed && !self.vim_command.replaying && !recording.is_empty() {
            self.vim_command.last_change = recording;
        }
        self.vim_command.pending_keys.clear();
    }

    pub(super) fn repeat_last_vim_change(&mut self, count: Option<usize>) {
        let keys = self.vim_command.last_change.clone();
        if keys.is_empty() {
            return;
        }
        // Vim's `.` substitutes its count for the recorded one, so `2x` then
        // `3.` deletes three characters, not six. With no recorded count there
        // is nothing to substitute into, so repeat instead; that keeps insert
        // sessions, which ignore counts, correct.
        let (events, repeats) = match count {
            Some(count) if keys.iter().any(|key| key.is_count) => {
                (replace_recorded_vim_count(&keys, count), 1)
            }
            Some(count) => (recorded_vim_events(&keys), count),
            None => (recorded_vim_events(&keys), 1),
        };
        self.vim_command.pending_keys.clear();
        self.vim_command.replaying = true;
        for _ in 0..repeats {
            for event in &events {
                self.input(*event);
            }
        }
        self.vim_command.replaying = false;
    }

    pub(super) fn push_vim_count_digit(count: &mut Option<usize>, digit: u8) {
        let next = count
            .unwrap_or(0)
            .saturating_mul(10)
            .saturating_add(usize::from(digit));
        *count = Some(next.clamp(1, VIM_MAX_COUNT));
    }

    pub(super) fn clear_vim_undo(&mut self) {
        self.vim_undo = VimUndoState::default();
    }

    pub(super) fn begin_vim_text_change(&mut self) {
        if !self.vim_enabled || self.vim_undo.active.is_some() {
            return;
        }
        self.vim_undo.active = Some(VimUndoSnapshot {
            text: self.text.clone(),
            cursor_pos: self.cursor_pos,
            elements: self.elements.clone(),
        });
        self.begin_vim_change_recording();
    }

    pub(super) fn finish_vim_text_change(&mut self) {
        if self.vim_enabled && self.vim_mode == VimMode::Normal {
            self.commit_vim_undo_session();
        }
    }

    pub(super) fn commit_vim_undo_session(&mut self) {
        let Some(snapshot) = self.vim_undo.active.take() else {
            return;
        };
        let changed = snapshot.text != self.text || snapshot.elements != self.elements;
        if !changed {
            self.finish_vim_change_recording(/*changed*/ false);
            return;
        }
        if self.vim_undo.history.len() == VIM_UNDO_LIMIT {
            self.vim_undo.history.pop_front();
        }
        self.vim_undo.history.push_back(snapshot);
        self.finish_vim_change_recording(/*changed*/ true);
    }

    pub(super) fn undo_vim_edit(&mut self) {
        let Some(snapshot) = self.vim_undo.history.pop_back() else {
            return;
        };
        self.text = snapshot.text;
        self.cursor_pos = snapshot.cursor_pos;
        self.elements = snapshot.elements;
        self.vim_undo.active = None;
        self.vim_mode = VimMode::Normal;
        self.vim_pending = VimPending::None;
        self.wrap_cache.replace(None);
        self.preferred_col = None;
    }

    pub(super) fn vim_text_object_scope_for_event(
        &self,
        event: KeyEvent,
    ) -> Option<VimTextObjectScope> {
        if self
            .vim_operator_keymap
            .select_inner_text_object
            .is_pressed(event)
        {
            return Some(VimTextObjectScope::Inner);
        }
        if self
            .vim_operator_keymap
            .select_around_text_object
            .is_pressed(event)
        {
            return Some(VimTextObjectScope::Around);
        }
        None
    }

    pub(super) fn vim_text_object_for_event(&self, event: KeyEvent) -> Option<VimTextObject> {
        if self.vim_text_object_keymap.word.is_pressed(event) {
            return Some(VimTextObject::Word);
        }
        if self.vim_text_object_keymap.big_word.is_pressed(event) {
            return Some(VimTextObject::BigWord);
        }
        if self.vim_text_object_keymap.parentheses.is_pressed(event) {
            return Some(VimTextObject::Parentheses);
        }
        if self.vim_text_object_keymap.brackets.is_pressed(event) {
            return Some(VimTextObject::Brackets);
        }
        if self.vim_text_object_keymap.braces.is_pressed(event) {
            return Some(VimTextObject::Braces);
        }
        if self.vim_text_object_keymap.double_quote.is_pressed(event) {
            return Some(VimTextObject::DoubleQuote);
        }
        if self.vim_text_object_keymap.single_quote.is_pressed(event) {
            return Some(VimTextObject::SingleQuote);
        }
        if self.vim_text_object_keymap.backtick.is_pressed(event) {
            return Some(VimTextObject::Backtick);
        }
        None
    }

    pub(super) fn text_object_range(
        &self,
        object: VimTextObject,
        scope: VimTextObjectScope,
    ) -> Option<Range<usize>> {
        match object {
            VimTextObject::Word => self.word_text_object_range(scope, /*big_word*/ false),
            VimTextObject::BigWord => self.word_text_object_range(scope, /*big_word*/ true),
            VimTextObject::Parentheses => self.paired_text_object_range(scope, '(', ')'),
            VimTextObject::Brackets => self.paired_text_object_range(scope, '[', ']'),
            VimTextObject::Braces => self.paired_text_object_range(scope, '{', '}'),
            VimTextObject::DoubleQuote => self.quoted_text_object_range(scope, '"'),
            VimTextObject::SingleQuote => self.quoted_text_object_range(scope, '\''),
            VimTextObject::Backtick => self.quoted_text_object_range(scope, '`'),
        }
    }

    fn word_text_object_range(
        &self,
        scope: VimTextObjectScope,
        big_word: bool,
    ) -> Option<Range<usize>> {
        let inner = if big_word {
            self.big_word_range_at_cursor()?
        } else {
            self.small_word_range_at_cursor()?
        };
        Some(match scope {
            VimTextObjectScope::Inner => inner,
            VimTextObjectScope::Around => self.expand_word_around(inner),
        })
    }

    fn big_word_range_at_cursor(&self) -> Option<Range<usize>> {
        self.non_ws_runs()
            .into_iter()
            .find(|range| self.cursor_overlaps_range(range) || self.cursor_is_at_range_end(range))
    }

    pub(super) fn small_word_range_at_cursor(&self) -> Option<Range<usize>> {
        for run in self.non_ws_runs() {
            if !self.cursor_overlaps_range(&run) && !self.cursor_is_at_range_end(&run) {
                continue;
            }
            let mut last_piece = None;
            for (piece_start, piece) in split_word_pieces(&self.text[run.clone()]) {
                let piece = run.start + piece_start..run.start + piece_start + piece.len();
                if self.cursor_overlaps_range(&piece) {
                    return Some(piece);
                }
                last_piece = Some(piece);
            }
            if self.cursor_is_at_range_end(&run) {
                return last_piece.or(Some(run));
            }
            return Some(run);
        }
        None
    }

    fn non_ws_runs(&self) -> Vec<Range<usize>> {
        let mut runs = Vec::new();
        let mut start = None;
        for (idx, ch) in self.text.char_indices() {
            if ch.is_whitespace() {
                if let Some(run_start) = start.take() {
                    runs.push(run_start..idx);
                }
            } else if start.is_none() {
                start = Some(idx);
            }
        }
        if let Some(run_start) = start {
            runs.push(run_start..self.text.len());
        }
        runs
    }

    fn cursor_overlaps_range(&self, range: &Range<usize>) -> bool {
        range.start <= self.cursor_pos && self.cursor_pos < range.end
    }

    fn cursor_is_at_range_end(&self, range: &Range<usize>) -> bool {
        range.start < range.end && self.cursor_pos == range.end
    }

    fn expand_word_around(&self, inner: Range<usize>) -> Range<usize> {
        let following = self.following_whitespace_end(inner.end);
        if following > inner.end {
            return inner.start..following;
        }
        self.preceding_whitespace_start(inner.start)..inner.end
    }

    fn following_whitespace_end(&self, start: usize) -> usize {
        let mut end = start;
        for (offset, ch) in self.text[start..].char_indices() {
            if !ch.is_whitespace() {
                break;
            }
            end = start + offset + ch.len_utf8();
        }
        end
    }

    fn preceding_whitespace_start(&self, end: usize) -> usize {
        let mut start = end;
        for (idx, ch) in self.text[..end].char_indices().rev() {
            if !ch.is_whitespace() {
                break;
            }
            start = idx;
        }
        start
    }

    fn paired_text_object_range(
        &self,
        scope: VimTextObjectScope,
        open: char,
        close: char,
    ) -> Option<Range<usize>> {
        let mut stack: Vec<usize> = Vec::new();
        let mut best: Option<Range<usize>> = None;
        for (idx, ch) in self.text.char_indices() {
            if self.is_inside_element(idx) {
                continue;
            }
            if ch == open {
                stack.push(idx);
            } else if ch == close {
                let Some(open_idx) = stack.pop() else {
                    continue;
                };
                let close_end = idx + ch.len_utf8();
                if open_idx <= self.cursor_pos && self.cursor_pos <= idx {
                    let candidate = match scope {
                        VimTextObjectScope::Inner => open_idx + open.len_utf8()..idx,
                        VimTextObjectScope::Around => open_idx..close_end,
                    };
                    if candidate.start <= candidate.end
                        && best
                            .as_ref()
                            .is_none_or(|current| candidate.len() < current.len())
                    {
                        best = Some(candidate);
                    }
                }
            }
        }
        best
    }

    fn quoted_text_object_range(
        &self,
        scope: VimTextObjectScope,
        quote: char,
    ) -> Option<Range<usize>> {
        let line = self.beginning_of_current_line()..self.end_of_current_line();
        let mut open = None;
        let mut best: Option<Range<usize>> = None;
        for (offset, ch) in self.text[line.clone()].char_indices() {
            let idx = line.start + offset;
            if self.is_inside_element(idx) || ch != quote || self.is_escaped(idx) {
                continue;
            }
            if let Some(open_idx) = open.take() {
                if open_idx <= self.cursor_pos && self.cursor_pos <= idx {
                    let candidate = match scope {
                        VimTextObjectScope::Inner => open_idx + quote.len_utf8()..idx,
                        VimTextObjectScope::Around => idx_range(open_idx, idx, quote),
                    };
                    if candidate.start <= candidate.end
                        && best
                            .as_ref()
                            .is_none_or(|current| candidate.len() < current.len())
                    {
                        best = Some(candidate);
                    }
                }
            } else {
                open = Some(idx);
            }
        }
        best
    }

    pub(super) fn is_inside_element(&self, pos: usize) -> bool {
        self.elements
            .iter()
            .any(|element| pos >= element.range.start && pos < element.range.end)
    }

    fn is_escaped(&self, pos: usize) -> bool {
        let mut backslashes = 0;
        for ch in self.text[..pos].chars().rev() {
            if ch != '\\' {
                break;
            }
            backslashes += 1;
        }
        backslashes % 2 == 1
    }
}

fn idx_range(open_idx: usize, close_idx: usize, quote: char) -> Range<usize> {
    open_idx..close_idx + quote.len_utf8()
}

fn recorded_vim_events(keys: &[VimRecordedKey]) -> Vec<KeyEvent> {
    keys.iter().map(|key| key.event).collect()
}

/// Rewrite `keys` so the count it carries becomes `count`.
///
/// The first digit run is replaced in place, so `d2w` repeats as `d3w`. Later
/// count digits are dropped, collapsing the rare `2d3w` onto one count.
fn replace_recorded_vim_count(keys: &[VimRecordedKey], count: usize) -> Vec<KeyEvent> {
    let mut events = Vec::with_capacity(keys.len());
    let mut replaced = false;
    for key in keys {
        if !key.is_count {
            events.push(key.event);
        } else if !replaced {
            replaced = true;
            events.extend(
                count
                    .to_string()
                    .chars()
                    .map(|ch| KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)),
            );
        }
    }
    events
}
