//! Text engine trait for Helix modal editing.
//!
//! This trait defines the interface that text implementations must provide
//! for Helix commands to operate on.

use crate::motion::Motion;
use crate::text_object::{TextObject, TextObjectModifier};

/// A trait for text engines that can be controlled by Helix commands.
///
/// Implementations of this trait provide the actual text manipulation
/// capabilities. The default implementations provide reasonable behaviors
/// but can be overridden for platform-specific optimizations.
pub trait HelixTextEngine {
    // =========================================================================
    // Core text access
    // =========================================================================

    /// Get the full text content.
    fn text(&self) -> &str;

    /// Get the cursor position (byte offset).
    fn cursor_position(&self) -> usize;

    /// Set the cursor position (byte offset).
    fn set_cursor_position(&mut self, position: usize);

    /// Get the selection range as (start, end) byte offsets.
    /// If no selection, start == end == cursor_position.
    fn selection(&self) -> (usize, usize);

    /// Set the selection range.
    fn set_selection(&mut self, start: usize, end: usize);

    // =========================================================================
    // Text modification
    // =========================================================================

    /// Insert text at the cursor position.
    fn insert_text(&mut self, text: &str);

    /// Delete the current selection (or character at cursor if no selection).
    fn delete(&mut self);

    /// Replace the current selection with the given text.
    fn replace_selection(&mut self, text: &str);

    // =========================================================================
    // Movement (with default implementations)
    // =========================================================================

    /// Move cursor left by count characters.
    fn move_left(&mut self, count: usize, extend_selection: bool) {
        let pos = self.cursor_position();
        let text = self.text();
        let new_pos = text[..pos]
            .char_indices()
            .rev()
            .take(count)
            .last()
            .map(|(i, _)| i)
            .unwrap_or(0);

        if extend_selection {
            let (start, end) = self.selection();
            if pos == end {
                self.set_selection(start, new_pos);
            } else {
                self.set_selection(new_pos, end);
            }
        }
        self.set_cursor_position(new_pos);
    }

    /// Move cursor right by count characters.
    fn move_right(&mut self, count: usize, extend_selection: bool) {
        let pos = self.cursor_position();
        let text = self.text();
        let new_pos = text[pos..]
            .char_indices()
            .skip(1)
            .take(count)
            .last()
            .map(|(i, _)| pos + i)
            .unwrap_or_else(|| text.len());

        if extend_selection {
            let (start, end) = self.selection();
            if pos == start {
                self.set_selection(new_pos, end);
            } else {
                self.set_selection(start, new_pos);
            }
        }
        self.set_cursor_position(new_pos);
    }

    /// Move cursor up by count lines.
    fn move_up(&mut self, count: usize, extend_selection: bool) {
        let pos = self.cursor_position();
        let text = self.text();

        // Find start of current line
        let line_start = text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let col = pos - line_start;

        // Move up count lines
        let mut target_line_start = line_start;
        for _ in 0..count {
            if target_line_start == 0 {
                break;
            }
            // Find previous line
            let prev_line_end = target_line_start - 1;
            target_line_start = text[..prev_line_end]
                .rfind('\n')
                .map(|i| i + 1)
                .unwrap_or(0);
        }

        // Find end of target line
        let target_line_end = text[target_line_start..]
            .find('\n')
            .map(|i| target_line_start + i)
            .unwrap_or(text.len());

        // Move to same column (or end of line if shorter)
        let target_line_len = target_line_end - target_line_start;
        let new_pos = target_line_start + col.min(target_line_len);

        if extend_selection {
            let (start, end) = self.selection();
            self.set_selection(start.min(new_pos), end.max(new_pos));
        }
        self.set_cursor_position(new_pos);
    }

    /// Move cursor down by count lines.
    fn move_down(&mut self, count: usize, extend_selection: bool) {
        let pos = self.cursor_position();
        let text_len = self.text().len();

        // Calculate positions using the text
        let (new_pos, sel_update) = {
            let text = self.text();

            // Find start of current line
            let line_start = text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
            let col = pos - line_start;

            // Find end of current line
            let mut line_end = text[pos..]
                .find('\n')
                .map(|i| pos + i)
                .unwrap_or(text.len());

            // Move down count lines
            let mut target_line_start = line_end + 1;
            for _ in 1..count {
                if target_line_start >= text.len() {
                    target_line_start = text.len();
                    break;
                }
                line_end = text[target_line_start..]
                    .find('\n')
                    .map(|i| target_line_start + i)
                    .unwrap_or(text.len());
                target_line_start = line_end + 1;
            }

            if target_line_start > text.len() {
                target_line_start = text.len();
            }

            // Find end of target line
            let target_line_end = text[target_line_start..]
                .find('\n')
                .map(|i| target_line_start + i)
                .unwrap_or(text.len());

            // Move to same column (or end of line if shorter)
            let target_line_len = target_line_end - target_line_start;
            let new_pos = (target_line_start + col.min(target_line_len)).min(text.len());

            let sel_update = if extend_selection {
                let (start, end) = self.selection();
                Some((start.min(new_pos), end.max(new_pos)))
            } else {
                None
            };

            (new_pos, sel_update)
        };

        if let Some((start, end)) = sel_update {
            self.set_selection(start, end);
        }
        self.set_cursor_position(new_pos.min(text_len));
    }

    /// Move to start of next word.
    fn move_word_forward(&mut self, count: usize, extend_selection: bool) {
        let mut pos = self.cursor_position();
        let text = self.text();

        for _ in 0..count {
            // Skip current word
            while pos < text.len()
                && text[pos..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
            {
                pos += text[pos..]
                    .chars()
                    .next()
                    .map(|c| c.len_utf8())
                    .unwrap_or(1);
            }
            // Skip non-word characters
            while pos < text.len()
                && text[pos..]
                    .starts_with(|c: char| !c.is_alphanumeric() && c != '_' && !c.is_whitespace())
            {
                pos += text[pos..]
                    .chars()
                    .next()
                    .map(|c| c.len_utf8())
                    .unwrap_or(1);
            }
            // Skip whitespace
            while pos < text.len() && text[pos..].starts_with(char::is_whitespace) {
                pos += text[pos..]
                    .chars()
                    .next()
                    .map(|c| c.len_utf8())
                    .unwrap_or(1);
            }
        }

        if extend_selection {
            let (start, _) = self.selection();
            self.set_selection(start, pos);
        }
        self.set_cursor_position(pos);
    }

    /// Move to start of previous word.
    fn move_word_backward(&mut self, count: usize, extend_selection: bool) {
        let mut pos = self.cursor_position();
        let text = self.text();

        for _ in 0..count {
            // Skip whitespace backward
            while pos > 0 {
                let prev_char = text[..pos].chars().last();
                if let Some(c) = prev_char {
                    if !c.is_whitespace() {
                        break;
                    }
                    pos -= c.len_utf8();
                } else {
                    break;
                }
            }
            // Skip to start of word
            while pos > 0 {
                let prev_char = text[..pos].chars().last();
                if let Some(c) = prev_char {
                    if !c.is_alphanumeric() && c != '_' {
                        break;
                    }
                    pos -= c.len_utf8();
                } else {
                    break;
                }
            }
        }

        if extend_selection {
            let (_, end) = self.selection();
            self.set_selection(pos, end);
        }
        self.set_cursor_position(pos);
    }

    /// Move to end of current/next word.
    fn move_word_end(&mut self, count: usize, extend_selection: bool) {
        let mut pos = self.cursor_position();
        let text = self.text();

        for _ in 0..count {
            // Skip whitespace
            while pos < text.len() && text[pos..].starts_with(char::is_whitespace) {
                pos += text[pos..]
                    .chars()
                    .next()
                    .map(|c| c.len_utf8())
                    .unwrap_or(1);
            }
            // Move to end of word
            while pos < text.len()
                && text[pos..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
            {
                pos += text[pos..]
                    .chars()
                    .next()
                    .map(|c| c.len_utf8())
                    .unwrap_or(1);
            }
        }

        if extend_selection {
            let (start, _) = self.selection();
            self.set_selection(start, pos);
        }
        self.set_cursor_position(pos);
    }

    /// Move to start of line.
    fn move_to_line_start(&mut self, extend_selection: bool) {
        let pos = self.cursor_position();
        let text = self.text();
        let new_pos = text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);

        if extend_selection {
            let (_, end) = self.selection();
            self.set_selection(new_pos, end);
        }
        self.set_cursor_position(new_pos);
    }

    /// Move to end of line.
    fn move_to_line_end(&mut self, extend_selection: bool) {
        let pos = self.cursor_position();
        let text = self.text();
        let new_pos = text[pos..]
            .find('\n')
            .map(|i| pos + i)
            .unwrap_or(text.len());

        if extend_selection {
            let (start, _) = self.selection();
            self.set_selection(start, new_pos);
        }
        self.set_cursor_position(new_pos);
    }

    /// Move to first non-blank character on line.
    fn move_to_line_first_non_blank(&mut self, extend_selection: bool) {
        let pos = self.cursor_position();
        let text = self.text();
        let line_start = text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let line_end = text[pos..]
            .find('\n')
            .map(|i| pos + i)
            .unwrap_or(text.len());

        let new_pos = text[line_start..line_end]
            .char_indices()
            .find(|(_, c)| !c.is_whitespace())
            .map(|(i, _)| line_start + i)
            .unwrap_or(line_start);

        if extend_selection {
            let (_, end) = self.selection();
            self.set_selection(new_pos, end);
        }
        self.set_cursor_position(new_pos);
    }

    /// Move to start of document.
    fn move_to_document_start(&mut self, extend_selection: bool) {
        if extend_selection {
            let (_, end) = self.selection();
            self.set_selection(0, end);
        }
        self.set_cursor_position(0);
    }

    /// Move to end of document.
    fn move_to_document_end(&mut self, extend_selection: bool) {
        let len = self.text().len();
        if extend_selection {
            let (start, _) = self.selection();
            self.set_selection(start, len);
        }
        self.set_cursor_position(len);
    }

    // =========================================================================
    // Character finding
    // =========================================================================

    /// Find character forward on line.
    fn find_character(&mut self, char: char, count: usize, extend_selection: bool) {
        let pos = self.cursor_position();
        let text = self.text();
        let line_end = text[pos..]
            .find('\n')
            .map(|i| pos + i)
            .unwrap_or(text.len());

        let mut found_pos = None;
        let mut found_count = 0;
        for (i, c) in text[pos + 1..line_end].char_indices() {
            if c == char {
                found_count += 1;
                if found_count == count {
                    found_pos = Some(pos + 1 + i);
                    break;
                }
            }
        }

        if let Some(new_pos) = found_pos {
            if extend_selection {
                let (start, _) = self.selection();
                self.set_selection(start, new_pos + 1);
            }
            self.set_cursor_position(new_pos);
        }
    }

    /// Find character backward on line.
    fn find_character_backward(&mut self, char: char, count: usize, extend_selection: bool) {
        let pos = self.cursor_position();
        let text = self.text();
        let line_start = text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);

        let mut found_pos = None;
        let mut found_count = 0;
        for (i, c) in text[line_start..pos].char_indices().rev() {
            if c == char {
                found_count += 1;
                if found_count == count {
                    found_pos = Some(line_start + i);
                    break;
                }
            }
        }

        if let Some(new_pos) = found_pos {
            if extend_selection {
                let (_, end) = self.selection();
                self.set_selection(new_pos, end);
            }
            self.set_cursor_position(new_pos);
        }
    }

    /// Move till (before) character forward.
    fn till_character(&mut self, char: char, count: usize, extend_selection: bool) {
        let orig_pos = self.cursor_position();
        self.find_character(char, count, extend_selection);
        let new_pos = self.cursor_position();
        if new_pos != orig_pos && new_pos > 0 {
            // Move one back (before the character)
            let text = self.text();
            let prev_char_start = text[..new_pos]
                .char_indices()
                .last()
                .map(|(i, _)| i)
                .unwrap_or(new_pos);
            self.set_cursor_position(prev_char_start);
        }
    }

    /// Move till (after) character backward.
    fn till_character_backward(&mut self, char: char, count: usize, extend_selection: bool) {
        let orig_pos = self.cursor_position();
        self.find_character_backward(char, count, extend_selection);
        let new_pos = self.cursor_position();
        if new_pos != orig_pos {
            // Move one forward (after the character)
            let text = self.text();
            if let Some(c) = text[new_pos..].chars().next() {
                self.set_cursor_position(new_pos + c.len_utf8());
            }
        }
    }

    // =========================================================================
    // Search
    // =========================================================================

    /// Perform a search.
    fn perform_search(&mut self, query: &str, backward: bool) {
        if backward {
            self.search_previous(query, false);
        } else {
            self.search_next(query, false);
        }
    }

    /// Move to next search match.
    fn search_next(&mut self, query: &str, extend_selection: bool) {
        if query.is_empty() {
            return;
        }
        let pos = self.cursor_position();
        let text = self.text();

        // Search forward from cursor
        if let Some(idx) = text[pos + 1..].find(query) {
            let new_pos = pos + 1 + idx;
            if extend_selection {
                let (start, _) = self.selection();
                self.set_selection(start, new_pos + query.len());
            }
            self.set_cursor_position(new_pos);
        } else if let Some(idx) = text[..pos].find(query) {
            // Wrap to beginning
            if extend_selection {
                let (start, _) = self.selection();
                self.set_selection(start, idx + query.len());
            }
            self.set_cursor_position(idx);
        }
    }

    /// Move to previous search match.
    fn search_previous(&mut self, query: &str, extend_selection: bool) {
        if query.is_empty() {
            return;
        }
        let pos = self.cursor_position();
        let text = self.text();

        // Search backward from cursor
        if let Some(idx) = text[..pos].rfind(query) {
            if extend_selection {
                let (_, end) = self.selection();
                self.set_selection(idx, end);
            }
            self.set_cursor_position(idx);
        } else if let Some(idx) = text[pos..].rfind(query) {
            // Wrap to end
            let new_pos = pos + idx;
            if extend_selection {
                let (_, end) = self.selection();
                self.set_selection(new_pos, end);
            }
            self.set_cursor_position(new_pos);
        }
    }

    // =========================================================================
    // Selection
    // =========================================================================

    /// Select the current line.
    fn select_line(&mut self) {
        let pos = self.cursor_position();
        let text = self.text();
        let line_start = text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let line_end = text[pos..]
            .find('\n')
            .map(|i| pos + i + 1)
            .unwrap_or(text.len());
        self.set_selection(line_start, line_end);
    }

    /// Select all text.
    fn select_all(&mut self) {
        self.set_selection(0, self.text().len());
    }

    // =========================================================================
    // Line operations
    // =========================================================================

    /// Open a new line below and position cursor.
    fn open_line_below(&mut self) {
        self.move_to_line_end(false);
        self.insert_text("\n");
    }

    /// Open a new line above and position cursor.
    fn open_line_above(&mut self) {
        self.move_to_line_start(false);
        let pos = self.cursor_position();
        self.insert_text("\n");
        self.set_cursor_position(pos);
    }

    /// Move cursor after current character (for append).
    fn move_after_cursor(&mut self) {
        let pos = self.cursor_position();
        let text = self.text();
        if let Some(c) = text[pos..].chars().next() {
            self.set_cursor_position(pos + c.len_utf8());
        }
    }

    /// Join current line with next line.
    fn join_lines(&mut self) {
        let pos = self.cursor_position();
        let text = self.text();
        let line_end = text[pos..].find('\n').map(|i| pos + i);

        if let Some(newline_pos) = line_end {
            // Find where next line's content starts
            let mut content_start = newline_pos + 1;
            while content_start < text.len()
                && text[content_start..].starts_with(char::is_whitespace)
                && !text[content_start..].starts_with('\n')
            {
                content_start += 1;
            }

            // Replace newline and whitespace with single space
            self.set_selection(newline_pos, content_start);
            self.replace_selection(" ");
        }
    }

    /// Toggle case of character at cursor (or selection).
    fn toggle_case(&mut self) {
        let (start, end) = self.selection();
        let text = self.text();

        if start == end {
            // Toggle single character
            if let Some(c) = text[start..].chars().next() {
                let toggled: String = if c.is_uppercase() {
                    c.to_lowercase().collect()
                } else {
                    c.to_uppercase().collect()
                };
                self.set_selection(start, start + c.len_utf8());
                self.replace_selection(&toggled);
                self.set_cursor_position(start + toggled.len());
            }
        } else {
            // Toggle selection
            let selected = &text[start..end];
            let toggled: String = selected
                .chars()
                .map(|c| {
                    if c.is_uppercase() {
                        c.to_lowercase().collect::<String>()
                    } else {
                        c.to_uppercase().collect::<String>()
                    }
                })
                .collect();
            self.replace_selection(&toggled);
        }
    }

    /// Indent current line.
    fn indent(&mut self) {
        self.move_to_line_start(false);
        self.insert_text("\t");
    }

    /// Dedent (unindent) current line.
    fn dedent(&mut self) {
        let pos = self.cursor_position();
        let text = self.text();
        let line_start = text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);

        if text[line_start..].starts_with('\t') {
            self.set_selection(line_start, line_start + 1);
            self.replace_selection("");
        } else if text[line_start..].starts_with("    ") {
            self.set_selection(line_start, line_start + 4);
            self.replace_selection("");
        } else if text[line_start..].starts_with(' ') {
            self.set_selection(line_start, line_start + 1);
            self.replace_selection("");
        }
    }

    /// Replace character at cursor with given character.
    fn replace_character(&mut self, char: char) {
        let pos = self.cursor_position();
        let text = self.text();
        if let Some(c) = text[pos..].chars().next() {
            self.set_selection(pos, pos + c.len_utf8());
            self.replace_selection(&char.to_string());
            self.set_cursor_position(pos);
        }
    }

    // =========================================================================
    // Clipboard operations
    // =========================================================================

    /// Yank (copy) the selection. Returns (text, is_linewise).
    fn yank(&self) -> (String, bool) {
        let (start, end) = self.selection();
        let text = self.text();
        if start == end {
            // Yank current line
            let line_start = text[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
            let line_end = text[start..]
                .find('\n')
                .map(|i| start + i + 1)
                .unwrap_or(text.len());
            (text[line_start..line_end].to_string(), true)
        } else {
            (text[start..end].to_string(), false)
        }
    }

    /// Paste text after cursor.
    fn paste_after(&mut self, text: &str) {
        self.move_after_cursor();
        self.insert_text(text);
    }

    /// Paste text before cursor.
    fn paste_before(&mut self, text: &str) {
        self.insert_text(text);
    }

    // =========================================================================
    // Motion and Text Object Range Calculation
    // =========================================================================

    /// Calculate the range affected by a motion.
    ///
    /// Returns `Some((start, end))` as byte offsets, or `None` if the motion
    /// cannot be performed (e.g., already at document boundary).
    fn motion_range(&self, motion: &Motion) -> Option<(usize, usize)> {
        let pos = self.cursor_position();
        let text = self.text();

        match motion {
            Motion::Left(count) => {
                let new_pos = text[..pos]
                    .char_indices()
                    .rev()
                    .take(*count)
                    .last()
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                Some((new_pos, pos))
            }
            Motion::Right(count) => {
                let new_pos = text[pos..]
                    .char_indices()
                    .skip(1)
                    .take(*count)
                    .last()
                    .map(|(i, _)| pos + i)
                    .unwrap_or_else(|| text.len());
                Some((pos, new_pos))
            }
            Motion::Up(count) => {
                // Calculate line range for up motion
                let line_start = text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
                let mut target_line_start = line_start;
                for _ in 0..*count {
                    if target_line_start == 0 {
                        break;
                    }
                    let prev_line_end = target_line_start - 1;
                    target_line_start = text[..prev_line_end]
                        .rfind('\n')
                        .map(|i| i + 1)
                        .unwrap_or(0);
                }
                let line_end = text[pos..]
                    .find('\n')
                    .map(|i| pos + i + 1)
                    .unwrap_or(text.len());
                Some((target_line_start, line_end))
            }
            Motion::Down(count) => {
                // Calculate line range for down motion
                let line_start = text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
                let line_end = text[pos..]
                    .find('\n')
                    .map(|i| pos + i)
                    .unwrap_or(text.len());
                let mut target_line_end = line_end;
                for _ in 0..*count {
                    if target_line_end >= text.len() {
                        break;
                    }
                    target_line_end = text[target_line_end + 1..]
                        .find('\n')
                        .map(|i| target_line_end + 1 + i)
                        .unwrap_or(text.len());
                }
                Some((line_start, (target_line_end + 1).min(text.len())))
            }
            Motion::WordForward(count) => {
                let mut end = pos;
                for _ in 0..*count {
                    while end < text.len()
                        && text[end..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
                    {
                        end += text[end..]
                            .chars()
                            .next()
                            .map(|c| c.len_utf8())
                            .unwrap_or(1);
                    }
                    while end < text.len()
                        && text[end..].starts_with(|c: char| {
                            !c.is_alphanumeric() && c != '_' && !c.is_whitespace()
                        })
                    {
                        end += text[end..]
                            .chars()
                            .next()
                            .map(|c| c.len_utf8())
                            .unwrap_or(1);
                    }
                    while end < text.len() && text[end..].starts_with(char::is_whitespace) {
                        end += text[end..]
                            .chars()
                            .next()
                            .map(|c| c.len_utf8())
                            .unwrap_or(1);
                    }
                }
                Some((pos, end))
            }
            Motion::WordBackward(count) => {
                let mut start = pos;
                for _ in 0..*count {
                    while start > 0 {
                        let prev_char = text[..start].chars().last();
                        if let Some(c) = prev_char {
                            if !c.is_whitespace() {
                                break;
                            }
                            start -= c.len_utf8();
                        } else {
                            break;
                        }
                    }
                    while start > 0 {
                        let prev_char = text[..start].chars().last();
                        if let Some(c) = prev_char {
                            if !c.is_alphanumeric() && c != '_' {
                                break;
                            }
                            start -= c.len_utf8();
                        } else {
                            break;
                        }
                    }
                }
                Some((start, pos))
            }
            Motion::WordEnd(count) => {
                let mut end = pos;
                for _ in 0..*count {
                    while end < text.len() && text[end..].starts_with(char::is_whitespace) {
                        end += text[end..]
                            .chars()
                            .next()
                            .map(|c| c.len_utf8())
                            .unwrap_or(1);
                    }
                    while end < text.len()
                        && text[end..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
                    {
                        end += text[end..]
                            .chars()
                            .next()
                            .map(|c| c.len_utf8())
                            .unwrap_or(1);
                    }
                }
                Some((pos, end))
            }
            Motion::WORDForward(count) => {
                let mut end = pos;
                for _ in 0..*count {
                    while end < text.len() && !text[end..].starts_with(char::is_whitespace) {
                        end += text[end..]
                            .chars()
                            .next()
                            .map(|c| c.len_utf8())
                            .unwrap_or(1);
                    }
                    while end < text.len() && text[end..].starts_with(char::is_whitespace) {
                        end += text[end..]
                            .chars()
                            .next()
                            .map(|c| c.len_utf8())
                            .unwrap_or(1);
                    }
                }
                Some((pos, end))
            }
            Motion::WORDBackward(count) => {
                let mut start = pos;
                for _ in 0..*count {
                    while start > 0 {
                        let prev_char = text[..start].chars().last();
                        if let Some(c) = prev_char {
                            if !c.is_whitespace() {
                                break;
                            }
                            start -= c.len_utf8();
                        } else {
                            break;
                        }
                    }
                    while start > 0 {
                        let prev_char = text[..start].chars().last();
                        if let Some(c) = prev_char {
                            if c.is_whitespace() {
                                break;
                            }
                            start -= c.len_utf8();
                        } else {
                            break;
                        }
                    }
                }
                Some((start, pos))
            }
            Motion::WORDEnd(count) => {
                let mut end = pos;
                for _ in 0..*count {
                    while end < text.len() && text[end..].starts_with(char::is_whitespace) {
                        end += text[end..]
                            .chars()
                            .next()
                            .map(|c| c.len_utf8())
                            .unwrap_or(1);
                    }
                    while end < text.len() && !text[end..].starts_with(char::is_whitespace) {
                        end += text[end..]
                            .chars()
                            .next()
                            .map(|c| c.len_utf8())
                            .unwrap_or(1);
                    }
                }
                Some((pos, end))
            }
            Motion::LineStart | Motion::ToLineStart => {
                let line_start = text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
                Some((line_start, pos))
            }
            Motion::LineEnd | Motion::ToLineEnd => {
                let line_end = text[pos..]
                    .find('\n')
                    .map(|i| pos + i)
                    .unwrap_or(text.len());
                Some((pos, line_end))
            }
            Motion::LineFirstNonBlank => {
                let line_start = text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
                let first_non_blank = text[line_start..]
                    .char_indices()
                    .find(|(_, c)| !c.is_whitespace())
                    .map(|(i, _)| line_start + i)
                    .unwrap_or(line_start);
                Some((first_non_blank.min(pos), first_non_blank.max(pos)))
            }
            Motion::DocumentStart => Some((0, pos)),
            Motion::DocumentEnd => Some((pos, text.len())),
            Motion::GotoLine(line_num) => {
                let mut line_start = 0;
                for _ in 1..*line_num {
                    if let Some(idx) = text[line_start..].find('\n') {
                        line_start = line_start + idx + 1;
                    } else {
                        break;
                    }
                }
                let line_end = text[line_start..]
                    .find('\n')
                    .map(|i| line_start + i + 1)
                    .unwrap_or(text.len());
                Some((line_start, line_end))
            }
            Motion::Line => {
                // Current line including newline
                let line_start = text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
                let line_end = text[pos..]
                    .find('\n')
                    .map(|i| pos + i + 1)
                    .unwrap_or(text.len());
                Some((line_start, line_end))
            }
            Motion::ParagraphForward(count) => {
                let mut end = pos;
                let mut blank_count = 0;
                for (i, c) in text[pos..].char_indices() {
                    if c == '\n' {
                        let next_pos = pos + i + 1;
                        if next_pos < text.len() && text[next_pos..].starts_with('\n') {
                            blank_count += 1;
                            if blank_count >= *count {
                                end = next_pos;
                                break;
                            }
                        }
                    }
                    end = pos + i + c.len_utf8();
                }
                Some((pos, end))
            }
            Motion::ParagraphBackward(count) => {
                let mut start = pos;
                let mut blank_count = 0;
                for (i, c) in text[..pos].char_indices().rev() {
                    if c == '\n' && i > 0 && text[..i].ends_with('\n') {
                        blank_count += 1;
                        if blank_count >= *count {
                            start = i;
                            break;
                        }
                    }
                    start = i;
                }
                Some((start, pos))
            }
            Motion::FindChar(char, count) => {
                let line_end = text[pos..]
                    .find('\n')
                    .map(|i| pos + i)
                    .unwrap_or(text.len());
                let mut found_pos = None;
                let mut found_count = 0;
                for (i, c) in text[pos + 1..line_end].char_indices() {
                    if c == *char {
                        found_count += 1;
                        if found_count == *count {
                            found_pos = Some(pos + 1 + i);
                            break;
                        }
                    }
                }
                found_pos.map(|end| (pos, end + 1)) // Inclusive
            }
            Motion::FindCharBackward(char, count) => {
                let line_start = text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
                let mut found_pos = None;
                let mut found_count = 0;
                for (i, c) in text[line_start..pos].char_indices().rev() {
                    if c == *char {
                        found_count += 1;
                        if found_count == *count {
                            found_pos = Some(line_start + i);
                            break;
                        }
                    }
                }
                found_pos.map(|start| (start, pos))
            }
            Motion::TillChar(char, count) => {
                let line_end = text[pos..]
                    .find('\n')
                    .map(|i| pos + i)
                    .unwrap_or(text.len());
                let mut found_pos = None;
                let mut found_count = 0;
                for (i, c) in text[pos + 1..line_end].char_indices() {
                    if c == *char {
                        found_count += 1;
                        if found_count == *count {
                            found_pos = Some(pos + 1 + i);
                            break;
                        }
                    }
                }
                found_pos.map(|end| (pos, end)) // Exclusive (till, not including)
            }
            Motion::TillCharBackward(char, count) => {
                let line_start = text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
                let mut found_pos = None;
                let mut found_count = 0;
                for (i, c) in text[line_start..pos].char_indices().rev() {
                    if c == *char {
                        found_count += 1;
                        if found_count == *count {
                            found_pos = Some(line_start + i + c.len_utf8());
                            break;
                        }
                    }
                }
                found_pos.map(|start| (start, pos))
            }
            Motion::MatchingBracket => {
                // Find matching bracket
                let current_char = text[pos..].chars().next()?;
                let (open, close, forward) = match current_char {
                    '(' => ('(', ')', true),
                    ')' => ('(', ')', false),
                    '[' => ('[', ']', true),
                    ']' => ('[', ']', false),
                    '{' => ('{', '}', true),
                    '}' => ('{', '}', false),
                    '<' => ('<', '>', true),
                    '>' => ('<', '>', false),
                    _ => return None,
                };

                let mut depth = 1;
                if forward {
                    for (i, c) in text[pos + 1..].char_indices() {
                        if c == close {
                            depth -= 1;
                            if depth == 0 {
                                return Some((pos, pos + 1 + i + 1));
                            }
                        } else if c == open {
                            depth += 1;
                        }
                    }
                } else {
                    for (i, c) in text[..pos].char_indices().rev() {
                        if c == open {
                            depth -= 1;
                            if depth == 0 {
                                return Some((i, pos + 1));
                            }
                        } else if c == close {
                            depth += 1;
                        }
                    }
                }
                None
            }
        }
    }

    /// Calculate the range affected by a text object with modifier.
    ///
    /// Returns `Some((start, end))` as byte offsets, or `None` if the text object
    /// is not found at the cursor position.
    fn text_object_range(
        &self,
        text_object: TextObject,
        modifier: TextObjectModifier,
    ) -> Option<(usize, usize)> {
        let pos = self.cursor_position();
        let text = self.text();

        match text_object {
            TextObject::Word => {
                // Find word boundaries
                let mut start = pos;
                let mut end = pos;

                // Find word start
                while start > 0 {
                    let prev_char = text[..start].chars().last();
                    if let Some(c) = prev_char {
                        if !c.is_alphanumeric() && c != '_' {
                            break;
                        }
                        start -= c.len_utf8();
                    } else {
                        break;
                    }
                }

                // Find word end
                while end < text.len()
                    && text[end..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
                {
                    end += text[end..]
                        .chars()
                        .next()
                        .map(|c| c.len_utf8())
                        .unwrap_or(1);
                }

                if modifier == TextObjectModifier::Around {
                    // Include trailing whitespace
                    while end < text.len()
                        && text[end..].starts_with(char::is_whitespace)
                        && !text[end..].starts_with('\n')
                    {
                        end += text[end..]
                            .chars()
                            .next()
                            .map(|c| c.len_utf8())
                            .unwrap_or(1);
                    }
                }

                Some((start, end))
            }
            TextObject::WORD => {
                // Find WORD boundaries (whitespace-delimited)
                let mut start = pos;
                let mut end = pos;

                while start > 0 {
                    let prev_char = text[..start].chars().last();
                    if let Some(c) = prev_char {
                        if c.is_whitespace() {
                            break;
                        }
                        start -= c.len_utf8();
                    } else {
                        break;
                    }
                }

                while end < text.len() && !text[end..].starts_with(char::is_whitespace) {
                    end += text[end..]
                        .chars()
                        .next()
                        .map(|c| c.len_utf8())
                        .unwrap_or(1);
                }

                if modifier == TextObjectModifier::Around {
                    while end < text.len()
                        && text[end..].starts_with(char::is_whitespace)
                        && !text[end..].starts_with('\n')
                    {
                        end += text[end..]
                            .chars()
                            .next()
                            .map(|c| c.len_utf8())
                            .unwrap_or(1);
                    }
                }

                Some((start, end))
            }
            TextObject::DoubleQuote | TextObject::SingleQuote | TextObject::BacktickQuote => {
                let quote = match text_object {
                    TextObject::DoubleQuote => '"',
                    TextObject::SingleQuote => '\'',
                    TextObject::BacktickQuote => '`',
                    _ => unreachable!(),
                };

                // Find quote boundaries on current line
                let line_start = text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
                let line_end = text[pos..]
                    .find('\n')
                    .map(|i| pos + i)
                    .unwrap_or(text.len());
                let line = &text[line_start..line_end];

                // Find opening quote
                let mut open_pos = None;
                let mut in_quote = false;
                let cursor_in_line = pos - line_start;

                for (i, c) in line.char_indices() {
                    if c == quote {
                        if !in_quote {
                            open_pos = Some(i);
                            in_quote = true;
                        } else {
                            // Found closing quote
                            if cursor_in_line >= open_pos.unwrap_or(0) && cursor_in_line <= i {
                                let start = line_start + open_pos.unwrap();
                                let end = line_start + i + 1;
                                return if modifier == TextObjectModifier::Inner {
                                    Some((start + 1, end - 1))
                                } else {
                                    Some((start, end))
                                };
                            }
                            in_quote = false;
                            open_pos = None;
                        }
                    }
                }

                None
            }
            TextObject::Parentheses
            | TextObject::SquareBrackets
            | TextObject::CurlyBraces
            | TextObject::AngleBrackets => {
                let (open, close) = match text_object {
                    TextObject::Parentheses => ('(', ')'),
                    TextObject::SquareBrackets => ('[', ']'),
                    TextObject::CurlyBraces => ('{', '}'),
                    TextObject::AngleBrackets => ('<', '>'),
                    _ => unreachable!(),
                };

                // Find matching pair
                let mut open_pos = None;
                let mut depth = 0;

                // Search backward for opening
                for (i, c) in text[..=pos.min(text.len().saturating_sub(1))]
                    .char_indices()
                    .rev()
                {
                    if c == close {
                        depth += 1;
                    } else if c == open {
                        if depth == 0 {
                            open_pos = Some(i);
                            break;
                        }
                        depth -= 1;
                    }
                }

                let open_pos = open_pos?;

                // Search forward for closing
                depth = 1;
                for (i, c) in text[open_pos + 1..].char_indices() {
                    if c == open {
                        depth += 1;
                    } else if c == close {
                        depth -= 1;
                        if depth == 0 {
                            let close_pos = open_pos + 1 + i;
                            return if modifier == TextObjectModifier::Inner {
                                Some((open_pos + 1, close_pos))
                            } else {
                                Some((open_pos, close_pos + 1))
                            };
                        }
                    }
                }

                None
            }
            TextObject::Paragraph => {
                // Find paragraph boundaries (blank lines)
                let mut start = pos;
                let mut end = pos;

                // Find paragraph start (previous blank line or document start)
                while start > 0 {
                    let prev_newline = text[..start].rfind('\n');
                    if let Some(nl) = prev_newline {
                        if nl > 0 && text[..nl].ends_with('\n') {
                            start = nl + 1;
                            break;
                        }
                        start = nl;
                    } else {
                        start = 0;
                        break;
                    }
                }

                // Find paragraph end (next blank line or document end)
                while end < text.len() {
                    let next_newline = text[end..].find('\n');
                    if let Some(nl) = next_newline {
                        let nl_pos = end + nl;
                        if nl_pos + 1 < text.len() && text[nl_pos + 1..].starts_with('\n') {
                            end = nl_pos + 1;
                            break;
                        }
                        end = nl_pos + 1;
                    } else {
                        end = text.len();
                        break;
                    }
                }

                if modifier == TextObjectModifier::Around {
                    // Include trailing blank lines
                    while end < text.len() && text[end..].starts_with('\n') {
                        end += 1;
                    }
                }

                Some((start, end))
            }
            TextObject::Sentence => {
                // Simplified sentence detection
                let sentence_ends = ['.', '!', '?'];
                let mut start = pos;
                let mut end = pos;

                // Find sentence start
                while start > 0 {
                    let prev_char = text[..start].chars().last();
                    if let Some(c) = prev_char {
                        if sentence_ends.contains(&c) {
                            break;
                        }
                        start -= c.len_utf8();
                    } else {
                        break;
                    }
                }

                // Skip whitespace at start
                while start < text.len() && text[start..].starts_with(char::is_whitespace) {
                    start += text[start..]
                        .chars()
                        .next()
                        .map(|c| c.len_utf8())
                        .unwrap_or(1);
                }

                // Find sentence end
                for (i, c) in text[pos..].char_indices() {
                    if sentence_ends.contains(&c) {
                        end = pos + i + 1;
                        break;
                    }
                    end = pos + i + c.len_utf8();
                }

                if modifier == TextObjectModifier::Around {
                    while end < text.len()
                        && text[end..].starts_with(char::is_whitespace)
                        && !text[end..].starts_with('\n')
                    {
                        end += text[end..]
                            .chars()
                            .next()
                            .map(|c| c.len_utf8())
                            .unwrap_or(1);
                    }
                }

                Some((start, end))
            }
            // These require language awareness and are not implemented in the basic text engine
            TextObject::Function
            | TextObject::Class
            | TextObject::Comment
            | TextObject::Argument => None,
        }
    }

    // =========================================================================
    // Undo/Redo (must be implemented by concrete types)
    // =========================================================================

    /// Undo the last change.
    fn undo(&mut self);

    /// Redo the last undone change.
    fn redo(&mut self);
}

/// A null text engine that does nothing.
///
/// Used when you want to track editor state without an actual text buffer.
pub struct NullTextEngine;

impl HelixTextEngine for NullTextEngine {
    fn text(&self) -> &str {
        ""
    }

    fn cursor_position(&self) -> usize {
        0
    }

    fn set_cursor_position(&mut self, _position: usize) {}

    fn selection(&self) -> (usize, usize) {
        (0, 0)
    }

    fn set_selection(&mut self, _start: usize, _end: usize) {}

    fn insert_text(&mut self, _text: &str) {}

    fn delete(&mut self) {}

    fn replace_selection(&mut self, _text: &str) {}

    fn undo(&mut self) {}

    fn redo(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simple in-memory text engine for testing.
    struct TestTextEngine {
        text: String,
        cursor: usize,
        selection: (usize, usize),
        undo_stack: Vec<(String, usize)>,
        redo_stack: Vec<(String, usize)>,
    }

    impl TestTextEngine {
        fn new(text: &str) -> Self {
            Self {
                text: text.to_string(),
                cursor: 0,
                selection: (0, 0),
                undo_stack: Vec::new(),
                redo_stack: Vec::new(),
            }
        }

        fn save_undo(&mut self) {
            self.undo_stack.push((self.text.clone(), self.cursor));
            self.redo_stack.clear();
        }
    }

    impl HelixTextEngine for TestTextEngine {
        fn text(&self) -> &str {
            &self.text
        }

        fn cursor_position(&self) -> usize {
            self.cursor
        }

        fn set_cursor_position(&mut self, position: usize) {
            self.cursor = position.min(self.text.len());
            self.selection = (self.cursor, self.cursor);
        }

        fn selection(&self) -> (usize, usize) {
            self.selection
        }

        fn set_selection(&mut self, start: usize, end: usize) {
            self.selection = (start.min(self.text.len()), end.min(self.text.len()));
        }

        fn insert_text(&mut self, text: &str) {
            self.save_undo();
            self.text.insert_str(self.cursor, text);
            self.cursor += text.len();
            self.selection = (self.cursor, self.cursor);
        }

        fn delete(&mut self) {
            self.save_undo();
            let (start, end) = self.selection;
            if start != end {
                self.text.drain(start..end);
                self.cursor = start;
            } else if self.cursor < self.text.len() {
                let char_len = self.text[self.cursor..]
                    .chars()
                    .next()
                    .map(|c| c.len_utf8())
                    .unwrap_or(0);
                self.text.drain(self.cursor..self.cursor + char_len);
            }
            self.selection = (self.cursor, self.cursor);
        }

        fn replace_selection(&mut self, text: &str) {
            self.save_undo();
            let (start, end) = self.selection;
            self.text.drain(start..end);
            self.text.insert_str(start, text);
            self.cursor = start + text.len();
            self.selection = (self.cursor, self.cursor);
        }

        fn undo(&mut self) {
            if let Some((text, cursor)) = self.undo_stack.pop() {
                self.redo_stack.push((self.text.clone(), self.cursor));
                self.text = text;
                self.cursor = cursor;
                self.selection = (self.cursor, self.cursor);
            }
        }

        fn redo(&mut self) {
            if let Some((text, cursor)) = self.redo_stack.pop() {
                self.undo_stack.push((self.text.clone(), self.cursor));
                self.text = text;
                self.cursor = cursor;
                self.selection = (self.cursor, self.cursor);
            }
        }
    }

    #[test]
    fn test_basic_movement() {
        let mut engine = TestTextEngine::new("hello world");
        engine.set_cursor_position(0);

        engine.move_right(5, false);
        assert_eq!(engine.cursor_position(), 5);

        engine.move_left(2, false);
        assert_eq!(engine.cursor_position(), 3);
    }

    #[test]
    fn test_word_movement() {
        let mut engine = TestTextEngine::new("hello world foo");
        engine.set_cursor_position(0);

        engine.move_word_forward(1, false);
        assert_eq!(engine.cursor_position(), 6); // Start of "world"

        engine.move_word_backward(1, false);
        assert_eq!(engine.cursor_position(), 0);
    }

    #[test]
    fn test_line_movement() {
        let mut engine = TestTextEngine::new("line1\nline2\nline3");
        engine.set_cursor_position(7); // In "line2"

        engine.move_to_line_start(false);
        assert_eq!(engine.cursor_position(), 6);

        engine.move_to_line_end(false);
        assert_eq!(engine.cursor_position(), 11);
    }

    #[test]
    fn test_delete() {
        let mut engine = TestTextEngine::new("hello world");
        engine.set_cursor_position(5);
        engine.set_selection(5, 11);

        engine.delete();
        assert_eq!(engine.text(), "hello");
    }

    #[test]
    fn test_undo_redo() {
        let mut engine = TestTextEngine::new("hello");
        engine.set_cursor_position(5);

        engine.insert_text(" world");
        assert_eq!(engine.text(), "hello world");

        engine.undo();
        assert_eq!(engine.text(), "hello");

        engine.redo();
        assert_eq!(engine.text(), "hello world");
    }

    #[test]
    fn test_search() {
        let mut engine = TestTextEngine::new("foo bar foo baz");
        engine.set_cursor_position(0);

        engine.search_next("foo", false);
        assert_eq!(engine.cursor_position(), 8); // Second "foo"

        engine.search_previous("foo", false);
        assert_eq!(engine.cursor_position(), 0); // First "foo"
    }
}
