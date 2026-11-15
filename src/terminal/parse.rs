use unicode_segmentation::UnicodeSegmentation;

use crate::debug_or_test::DEBUG_OR_TEST;
use crate::terminal::line::Line;
use crate::terminal::span::Span;
use crate::terminal::style::Style;
use crate::terminal::terminal_char::TerminalChar;
use crate::terminal::terminal_str::str::TerminalStr;
use crate::terminal::terminal_str::string::TerminalString;
use crate::terminal::width::Width;

/// Parse command output into lines of styled terminal-char spans.
///
/// SGR color/attribute sequences become span styles. Other escapes are
/// skipped. A final newline does not add a trailing empty line.
pub(crate) fn parse_command_output(input: &[u8]) -> Vec<Line> {
    let text = String::from_utf8_lossy(input);
    let mut parser = Parser::new(&text);
    let mut lines = Vec::new();
    while let Some(line) = parser.next_line() {
        lines.push(line);
    }
    lines
}

/// Pull parser over an input string. Holds the unconsumed input and the
/// SGR style; each `next_line` returns one line and does not keep it.
pub(crate) struct Parser<'a> {
    pub(crate) rem_input: &'a str,
    style: Style,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Parser<'a> {
        Parser {
            rem_input: input,
            style: Style::plain(),
        }
    }

    fn next_line(&mut self) -> Option<Line> {
        let mut line = Line::new();
        loop {
            match self.next_span(line.width()) {
                NextSpan::Span(span) => {
                    // Escapes and other skips split a take_text run. Join
                    // adjacent pieces that still share a style.
                    match line.spans.last_mut() {
                        Some(last) if last.style == span.style => {
                            last.content.push_str(span.content.as_ref());
                        }
                        _ => line.spans.push(span),
                    }
                }
                NextSpan::Newline => return Some(line),
                NextSpan::Eof => {
                    if line.spans.is_empty() {
                        return None;
                    }
                    return Some(line);
                }
            }
        }
    }

    /// Next span on the current line, or a line/input boundary.
    ///
    /// `col` is the width of spans already taken on this line. It is used
    /// to expand tabs. SGR updates `self.style` and does not emit a span.
    fn next_span(&mut self, col: Width) -> NextSpan {
        // Each iteration returns, or consumes input (`rem_input` shrinks).
        loop {
            let before_len = self.rem_input.len();
            match self.lookahead_char() {
                None => return NextSpan::Eof,
                Some('\n') => {
                    self.next_char();
                    return NextSpan::Newline;
                }
                Some('\r') => {
                    // `\r\n` is one line break; a bare CR is not modeled (no cursor).
                    self.next_char();
                }
                Some('\u{1b}') => {
                    self.next_escape();
                }
                Some(ch) if ch.is_control() && ch != '\t' => {
                    self.next_char();
                }
                Some(_) => {
                    let content = self.take_text(col);
                    if !content.is_empty() {
                        return NextSpan::Span(Span {
                            style: self.style,
                            content,
                        });
                    }
                    // Only zero-width chars were skipped before a boundary.
                }
            }
            if self.rem_input.len() < before_len {
                continue;
            }
            if DEBUG_OR_TEST {
                panic!("next_span did not consume input");
            } else {
                // Release: drop one char so a missed consume cannot spin.
                // Eof, if that empties the input, is handled next iteration.
                self.next_char();
            }
        }
    }

    /// Read unstyled text up to the next control (other than tab) or end
    /// of input. `col` is the width of spans already taken on this line.
    /// The stopping control is not consumed.
    fn take_text(&mut self, col: Width) -> TerminalString {
        let mut content = TerminalString::with_capacity(0);
        loop {
            let Some(ch) = self.lookahead_char() else {
                break;
            };
            if ch == '\t' {
                self.push_tab(&mut content, col);
                continue;
            }
            // Newline, CR, ESC, and other controls are classified by `next_span`.
            if ch.is_control() {
                break;
            }
            // Consume one grapheme. Always advance by at least one
            // char so a degenerate empty grapheme cannot spin.
            let n;
            let term_char;
            {
                let Some(grapheme) = self.rem_input.graphemes(true).next() else {
                    self.next_char();
                    continue;
                };
                n = grapheme.len();
                term_char = TerminalChar::new(grapheme);
            }
            if n == 0 {
                self.next_char();
                continue;
            }
            self.rem_input = &self.rem_input[n..];
            let Some(term_char) = term_char else {
                continue;
            };
            content.push(term_char);
        }
        content
    }

    /// Next input char without consuming it.
    fn lookahead_char(&self) -> Option<char> {
        self.rem_input.chars().next()
    }

    /// Consume and return the next input char.
    fn next_char(&mut self) -> Option<char> {
        let ch = self.lookahead_char()?;
        self.rem_input = &self.rem_input[ch.len_utf8()..];
        Some(ch)
    }

    /// If the next input char equals `expected`, consume it.
    fn next_char_if_eq(&mut self, expected: char) -> bool {
        if self.lookahead_char() != Some(expected) {
            return false;
        }
        self.next_char();
        true
    }

    /// Consume the escape at the current position (updating style for SGR).
    /// The next input char must be ESC.
    fn next_escape(&mut self) {
        self.next_char();
        if self.next_char_if_eq('[') {
            // Style is Copy. Pull it out so consume_csi can take `&mut Parser`
            // without borrowing `self.style` and `self` together.
            let mut style = self.style;
            style.consume_csi(self);
            self.style = style;
            return;
        }
        if self.next_char_if_eq(']') {
            self.consume_osc();
            return;
        }
        // Two-character escape, or a lone ESC.
        self.next_char();
    }

    /// Consume a tab and append spaces to `content` up to the next tab stop.
    /// The next input char must be a tab. `col` is the width before `content`.
    fn push_tab(&mut self, content: &mut TerminalString, col: Width) {
        self.next_char();
        let at = col + content.width();
        let spaces = 8 - (at.0 % 8);
        for _ in 0..spaces {
            content.push_str(TerminalStr::SPACE);
        }
    }

    /// OSC body (after `ESC ]`). Consumes it from `rem_input`.
    /// Ends at BEL or at the ST sequence (ESC + backslash).
    fn consume_osc(&mut self) {
        loop {
            if self.next_char_if_eq('\u{7}') {
                return;
            }
            if self.next_char_if_eq('\u{1b}') {
                // ST is ESC followed by backslash. A lone ESC is payload.
                if self.next_char_if_eq('\\') {
                    return;
                }
                continue;
            }
            if self.next_char().is_none() {
                return;
            }
        }
    }
}

/// Result of `next_span`: a span, a newline, or end of input.
enum NextSpan {
    Span(Span),
    Newline,
    Eof,
}

#[cfg(test)]
mod tests {
    use crate::terminal::color::TerminalColor;
    use crate::terminal::line::Line;
    use crate::terminal::parse::Parser;
    use crate::terminal::parse::parse_command_output;
    use crate::terminal::style::Style;
    use crate::terminal::width::Width;

    fn plain(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                let mut s = String::new();
                for span in &line.spans {
                    s.push_str(span.content.as_ref().as_str());
                }
                s
            })
            .collect()
    }

    #[test]
    fn test_empty() {
        assert_eq!(Vec::<Line>::new(), parse_command_output(b""));
    }

    #[test]
    fn test_single_line_no_newline() {
        let lines = parse_command_output(b"hello");
        assert_eq!(vec!["hello".to_owned()], plain(&lines));
        assert_eq!(1, lines[0].spans.len());
        assert_eq!(Style::plain(), lines[0].spans[0].style);
    }

    #[test]
    fn test_trailing_newline_does_not_add_blank_line() {
        let lines = parse_command_output(b"hello\n");
        assert_eq!(vec!["hello".to_owned()], plain(&lines));
    }

    #[test]
    fn test_two_lines() {
        let lines = parse_command_output(b"a\nb\n");
        assert_eq!(vec!["a".to_owned(), "b".to_owned()], plain(&lines));
    }

    #[test]
    fn test_blank_line() {
        let lines = parse_command_output(b"a\n\nb");
        assert_eq!(
            vec!["a".to_owned(), String::new(), "b".to_owned()],
            plain(&lines)
        );
    }

    #[test]
    fn test_crlf() {
        let lines = parse_command_output(b"a\r\nb");
        assert_eq!(vec!["a".to_owned(), "b".to_owned()], plain(&lines));
    }

    #[test]
    fn test_wide_and_combining() {
        let lines = parse_command_output("a中a\u{0308}".as_bytes());
        assert_eq!(vec!["a中a\u{0308}".to_owned()], plain(&lines));
        let mut chars = lines[0].spans[0].content.as_ref().chars();
        assert_eq!(Width(1), chars.next().unwrap().width());
        assert_eq!(Width(2), chars.next().unwrap().width());
        assert_eq!(Width(1), chars.next().unwrap().width());
        assert_eq!(true, chars.next().is_none());
        assert_eq!(Width(4), lines[0].width());
    }

    #[test]
    fn test_sgr_splits_spans() {
        let lines = parse_command_output(b"a\x1b[31mred\x1b[0m b");
        assert_eq!(vec!["ared b".to_owned()], plain(&lines));
        assert_eq!(3, lines[0].spans.len());
        assert_eq!(Style::plain(), lines[0].spans[0].style);
        assert_eq!(Some(TerminalColor::Red), lines[0].spans[1].style.fg);
        assert_eq!("red", span_text(&lines[0].spans[1]));
        assert_eq!(Style::plain(), lines[0].spans[2].style);
        assert_eq!(" b", span_text(&lines[0].spans[2]));
    }

    #[test]
    fn test_sgr_persists_across_lines() {
        let lines = parse_command_output(b"\x1b[1;32ma\nb");
        assert_eq!(vec!["a".to_owned(), "b".to_owned()], plain(&lines));
        assert_eq!(true, lines[0].spans[0].style.bold);
        assert_eq!(Some(TerminalColor::Green), lines[0].spans[0].style.fg);
        assert_eq!(lines[0].spans[0].style, lines[1].spans[0].style);
    }

    #[test]
    fn test_same_style_is_one_span() {
        let lines = parse_command_output(b"\x1b[31ma\x1b[31mb");
        assert_eq!(1, lines[0].spans.len());
        assert_eq!("ab", span_text(&lines[0].spans[0]));
    }

    #[test]
    fn test_skips_non_sgr_csi_and_osc() {
        let lines = parse_command_output(b"a\x1b[2Kb\x1b]8;;https://example.com\x07c");
        assert_eq!(vec!["abc".to_owned()], plain(&lines));
    }

    #[test]
    fn test_tab_expands_to_stop() {
        let lines = parse_command_output(b"a\tb");
        assert_eq!(vec!["a       b".to_owned()], plain(&lines));
        assert_eq!(Width(9), lines[0].width());
    }

    #[test]
    fn test_zero_width_is_skipped() {
        let lines = parse_command_output("a\u{200B}b\n\u{200B}".as_bytes());
        assert_eq!(vec!["ab".to_owned()], plain(&lines));
    }

    #[test]
    fn test_invalid_utf8_is_lossy() {
        let lines = parse_command_output(&[b'a', 0xff, b'b']);
        assert_eq!(vec!["a\u{fffd}b".to_owned()], plain(&lines));
    }

    #[test]
    fn test_next_line_yields_without_keeping_output() {
        let mut parser = Parser::new("\x1b[31ma\nb");
        let first = parser.next_line().unwrap();
        assert_eq!("a", span_text(&first.spans[0]));
        assert_eq!(Some(TerminalColor::Red), first.spans[0].style.fg);
        let second = parser.next_line().unwrap();
        assert_eq!("b", span_text(&second.spans[0]));
        assert_eq!(Some(TerminalColor::Red), second.spans[0].style.fg);
        assert_eq!(None, parser.next_line());
        assert_eq!("", parser.rem_input);
    }

    fn span_text(span: &crate::terminal::span::Span) -> &str {
        span.content.as_ref().as_str()
    }
}
