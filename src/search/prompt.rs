use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use unicode_width::UnicodeWidthChar;

pub(crate) struct Prompt {
    forward: bool,
    query: String,
    /// Byte offset into `query`, always on a char boundary.
    cursor: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PromptOutcome {
    Editing,
    Submit,
    Cancel,
    Redraw,
    Ignore,
}

impl Prompt {
    pub(crate) fn new(forward: bool) -> Prompt {
        Prompt {
            forward,
            query: String::new(),
            cursor: 0,
        }
    }

    pub(crate) fn forward(&self) -> bool {
        self.forward
    }

    pub(crate) fn status_label(&self) -> String {
        let mut label = String::new();
        if self.forward {
            label.push('/');
        } else {
            label.push('?');
        }
        label.push_str(&self.query);
        label
    }

    pub(crate) fn take_query(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.query)
    }

    /// Display columns from the start of the status label to the caret.
    /// The label begins with `/` or `?`.
    pub(crate) fn caret_cols(&self) -> u32 {
        let mut cols = 1u32;
        for c in self.query[..self.cursor].chars() {
            let w = u32::try_from(c.width_cjk().unwrap_or(0)).unwrap_or(0);
            cols = cols.saturating_add(w);
        }
        cols
    }

    pub(crate) fn on_key(&mut self, key: &KeyEvent) -> PromptOutcome {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            if let KeyCode::Char(c) = key.code {
                match c {
                    'a' | 'A' | '\u{1}' => {
                        self.cursor = 0;
                        return PromptOutcome::Editing;
                    }
                    'e' | 'E' | '\u{5}' => {
                        self.cursor = self.query.len();
                        return PromptOutcome::Editing;
                    }
                    'u' | 'U' | '\u{15}' => {
                        if self.cursor == 0 {
                            return PromptOutcome::Ignore;
                        }
                        self.query.replace_range(0..self.cursor, "");
                        self.cursor = 0;
                        return PromptOutcome::Editing;
                    }
                    'w' | 'W' | '\u{17}' => {
                        if self.kill_word_backward() {
                            return PromptOutcome::Editing;
                        }
                        return PromptOutcome::Ignore;
                    }
                    'c' | 'C' | 'g' | 'G' => return PromptOutcome::Cancel,
                    'l' | 'L' | '\u{c}' => return PromptOutcome::Redraw,
                    _ => {}
                }
            }
        }
        match key.code {
            KeyCode::Enter => PromptOutcome::Submit,
            KeyCode::Esc => PromptOutcome::Cancel,
            KeyCode::Backspace => {
                if self.cursor == 0 {
                    if self.query.is_empty() {
                        PromptOutcome::Cancel
                    } else {
                        PromptOutcome::Ignore
                    }
                } else {
                    let prev = prev_char_boundary(&self.query, self.cursor);
                    self.query.replace_range(prev..self.cursor, "");
                    self.cursor = prev;
                    PromptOutcome::Editing
                }
            }
            KeyCode::Char(c)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT)
                    && !c.is_control() =>
            {
                self.query.insert(self.cursor, c);
                self.cursor += c.len_utf8();
                PromptOutcome::Editing
            }
            _ => PromptOutcome::Ignore,
        }
    }

    /// Readline unix-word-rubout: delete the whitespace and word before the caret.
    fn kill_word_backward(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let boundaries: Vec<(usize, char)> = self.query[..self.cursor].char_indices().collect();
        let mut n = boundaries.len();
        while n > 0 && boundaries[n - 1].1.is_whitespace() {
            n -= 1;
        }
        while n > 0 && !boundaries[n - 1].1.is_whitespace() {
            n -= 1;
        }
        let start = if n == 0 {
            0
        } else {
            let (i, c) = boundaries[n - 1];
            i + c.len_utf8()
        };
        if start == self.cursor {
            return false;
        }
        self.query.replace_range(start..self.cursor, "");
        self.cursor = start;
        true
    }
}

fn prev_char_boundary(s: &str, index: usize) -> usize {
    if index == 0 {
        return 0;
    }
    s.char_indices()
        .map(|(i, _)| i)
        .take_while(|&i| i < index)
        .last()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyEventKind;
    use crossterm::event::KeyEventState;
    use crossterm::event::KeyModifiers;

    use crate::height::Height;
    use crate::render::Renderer;
    use crate::search::prompt::Prompt;
    use crate::search::prompt::PromptOutcome;
    use crate::size::Size;
    use crate::terminal::parse::parse_command_output;
    use crate::terminal::width::Width;
    use crate::terminal_bg::TerminalBg;

    fn size(width: u32, height: u32) -> Size {
        Size {
            width: Width(width),
            height: Height(height),
        }
    }

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn test_status_bar_shows_prompt() {
        let mut renderer = Renderer::new(TerminalBg::Dark);
        let size = size(10, 3);
        renderer.set_lines(parse_command_output(b"a\nb\nc\nd\n"), size);
        renderer.set_status_label(Some("/foo".to_owned()));
        let mut buf = Vec::new();
        renderer.draw(&mut buf, size).unwrap();
        let mut parser = vt100::Parser::new(3, 10, 0);
        parser.process(&buf);
        let mut text = String::new();
        for col in 0..10 {
            let cell = parser.screen().cell(2, col).unwrap();
            if cell.has_contents() {
                text.push_str(cell.contents());
            } else {
                text.push(' ');
            }
        }
        assert_eq!("/foo", text.trim_end());
    }

    #[test]
    fn test_prompt_editing() {
        let mut prompt = Prompt::new(true);
        assert_eq!("/", prompt.status_label());
        assert_eq!(
            PromptOutcome::Editing,
            prompt.on_key(&press(KeyCode::Char('a'), KeyModifiers::NONE))
        );
        assert_eq!(
            PromptOutcome::Editing,
            prompt.on_key(&press(KeyCode::Char('b'), KeyModifiers::NONE))
        );
        assert_eq!("/ab", prompt.status_label());
        assert_eq!(
            PromptOutcome::Editing,
            prompt.on_key(&press(KeyCode::Backspace, KeyModifiers::NONE))
        );
        assert_eq!("/a", prompt.status_label());
        assert_eq!(
            PromptOutcome::Submit,
            prompt.on_key(&press(KeyCode::Enter, KeyModifiers::NONE))
        );
        assert_eq!("a", prompt.take_query());
    }

    #[test]
    fn test_prompt_readline_keys() {
        let mut prompt = Prompt::new(true);
        for c in ['f', 'o', 'o', ' ', 'b', 'a', 'r'] {
            prompt.on_key(&press(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!("/foo bar", prompt.status_label());
        assert_eq!(
            PromptOutcome::Editing,
            prompt.on_key(&press(KeyCode::Char('a'), KeyModifiers::CONTROL))
        );
        assert_eq!(1, prompt.caret_cols());
        assert_eq!(
            PromptOutcome::Editing,
            prompt.on_key(&press(KeyCode::Char('x'), KeyModifiers::NONE))
        );
        assert_eq!("/xfoo bar", prompt.status_label());
        assert_eq!(
            PromptOutcome::Editing,
            prompt.on_key(&press(KeyCode::Char('e'), KeyModifiers::CONTROL))
        );
        assert_eq!(9, prompt.caret_cols());
        assert_eq!(
            PromptOutcome::Editing,
            prompt.on_key(&press(KeyCode::Char('w'), KeyModifiers::CONTROL))
        );
        assert_eq!("/xfoo ", prompt.status_label());
        assert_eq!(
            PromptOutcome::Editing,
            prompt.on_key(&press(KeyCode::Char('u'), KeyModifiers::CONTROL))
        );
        assert_eq!("/", prompt.status_label());
        assert_eq!(1, prompt.caret_cols());
        assert_eq!(
            PromptOutcome::Ignore,
            prompt.on_key(&press(KeyCode::Char('u'), KeyModifiers::CONTROL))
        );
    }

    #[test]
    fn test_prompt_cancel_and_backward_prefix() {
        let mut prompt = Prompt::new(false);
        assert_eq!("?", prompt.status_label());
        assert_eq!(
            PromptOutcome::Cancel,
            prompt.on_key(&press(KeyCode::Backspace, KeyModifiers::NONE))
        );
        assert_eq!(
            PromptOutcome::Cancel,
            prompt.on_key(&press(KeyCode::Esc, KeyModifiers::NONE))
        );
        assert_eq!(
            PromptOutcome::Cancel,
            prompt.on_key(&press(KeyCode::Char('c'), KeyModifiers::CONTROL))
        );
        assert_eq!(
            PromptOutcome::Redraw,
            prompt.on_key(&press(KeyCode::Char('l'), KeyModifiers::CONTROL))
        );
    }
}
