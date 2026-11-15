use std::cmp;
use std::io;
use std::io::Write;

use crossterm::cursor::Hide;
use crossterm::cursor::MoveTo;
use crossterm::cursor::Show;
use crossterm::queue;
use crossterm::terminal::Clear;
use crossterm::terminal::ClearType;
use unicode_width::UnicodeWidthChar;

use crate::height::Height;
use crate::size::Size;
use crate::terminal::line::Line;
use crate::terminal::width::Width;
use crate::terminal_bg::TerminalBg;
use crate::theme::search_highlight;

/// Styled view of parsed lines. `row` is the top rendered-row index.
pub(crate) struct Renderer {
    row: u32,
    lines: Vec<Line>,
    drawn: Option<DrawnFrame>,
    terminal_bg: TerminalBg,
    /// Replaces the `(END)` label while a prompt or message is showing.
    status_label: Option<String>,
    /// Caret column in `status_label`. `None` means the end of the label.
    status_caret: Option<u32>,
    /// Literal pattern whose matches are drawn with a search highlight.
    highlight: Option<Highlight>,
}

/// Active search highlight. `current` is the selected match's line and byte offset.
struct Highlight {
    pattern: String,
    current: Option<(usize, usize)>,
    ignore_case: bool,
}

struct DrawnFrame {
    size: Size,
    rows: Vec<String>,
    status: String,
    caret: u32,
}

impl Renderer {
    pub(crate) fn new(terminal_bg: TerminalBg) -> Renderer {
        Renderer {
            row: 0,
            lines: Vec::new(),
            drawn: None,
            terminal_bg,
            status_label: None,
            status_caret: None,
            highlight: None,
        }
    }

    pub(crate) fn set_highlight(
        &mut self,
        pattern: Option<String>,
        current: Option<(usize, usize)>,
        ignore_case: bool,
    ) {
        self.highlight = match pattern {
            Some(pattern) if !pattern.is_empty() => Some(Highlight {
                pattern,
                current,
                ignore_case,
            }),
            _ => None,
        };
    }

    pub(crate) fn lines(&self) -> &[Line] {
        &self.lines
    }

    pub(crate) fn set_status_label(&mut self, label: Option<String>) {
        self.status_label = label;
        self.status_caret = None;
    }

    pub(crate) fn status_label(&self) -> Option<&str> {
        self.status_label.as_deref()
    }

    pub(crate) fn set_prompt_status(&mut self, label: String, caret_cols: u32) {
        self.status_label = Some(label);
        self.status_caret = Some(caret_cols);
    }

    pub(crate) fn set_lines(&mut self, lines: Vec<Line>, size: Size) {
        self.lines = lines;
        self.clamp_row(size);
    }

    pub(crate) fn scroll_down(&mut self, size: Size) {
        self.row = self.row.saturating_add(1);
        self.clamp_row(size);
    }

    pub(crate) fn scroll_up(&mut self) {
        self.row = self.row.saturating_sub(1);
    }

    pub(crate) fn scroll_top(&mut self) {
        self.row = 0;
    }

    pub(crate) fn scroll_bottom(&mut self, size: Size) {
        self.row = u32::MAX;
        self.clamp_row(size);
    }

    pub(crate) fn scroll_page_down(&mut self, size: Size) {
        let step = cmp::max(content_height(size.height.0), 1);
        self.row = self.row.saturating_add(step);
        self.clamp_row(size);
    }

    pub(crate) fn scroll_page_up(&mut self, size: Size) {
        let step = cmp::max(content_height(size.height.0), 1);
        self.row = self.row.saturating_sub(step);
    }

    pub(crate) fn scroll_to_line(&mut self, line_idx: usize, size: Size) {
        self.row = visual_row_of_logical_line(&self.lines, size.width, line_idx);
        self.clamp_row(size);
    }

    pub(crate) fn scroll_to_match(&mut self, line_idx: usize, byte_start: usize, size: Size) {
        let base = visual_row_of_logical_line(&self.lines, size.width, line_idx);
        let offset = match self.lines.get(line_idx) {
            Some(line) => visual_row_offset_for_byte(line, size.width, byte_start),
            None => 0,
        };
        let match_row = base.saturating_add(offset);
        let body = content_height(size.height.0);
        // Leave the page alone when the hit is already visible. If it is
        // off-screen, center it in the content area, as Neovim does for a
        // jump that moves the cursor outside the window.
        if body == 0 {
            self.row = match_row;
            self.clamp_row(size);
            return;
        }
        let top = self.row;
        let bottom = top.saturating_add(body - 1);
        if match_row < top || match_row > bottom {
            self.row = match_row.saturating_sub(body / 2);
        }
        self.clamp_row(size);
    }

    pub(crate) fn top_row(&self) -> u32 {
        self.row
    }

    pub(crate) fn top_line_index(&self, size: Size) -> usize {
        if self.lines.is_empty() {
            return 0;
        }
        logical_line_at_visual_row(&self.lines, size.width, self.row)
    }

    pub(crate) fn bottom_line_index(&self, size: Size) -> usize {
        if self.lines.is_empty() {
            return 0;
        }
        let body = content_height(size.height.0);
        let count = visual_rows(&self.lines, size.width, None, false).len();
        if count == 0 {
            return 0;
        }
        let last = if body == 0 {
            self.row as usize
        } else {
            self.row as usize + body as usize - 1
        };
        let last = cmp::min(last, count - 1);
        logical_line_at_visual_row(&self.lines, size.width, last as u32)
    }

    pub(crate) fn draw(&mut self, out: &mut impl Write, size: Size) -> io::Result<()> {
        self.draw_frame(out, size, false)
    }

    pub(crate) fn redraw(&mut self, out: &mut impl Write, size: Size) -> io::Result<()> {
        self.draw_frame(out, size, true)
    }

    fn draw_frame(&mut self, out: &mut impl Write, size: Size, force: bool) -> io::Result<()> {
        let terminal_width = size.width;
        let body_height = content_height(size.height.0);
        let light_terminal = self.terminal_bg == TerminalBg::Light;
        let all = visual_rows(
            &self.lines,
            terminal_width,
            self.highlight.as_ref(),
            light_terminal,
        );
        self.row = cmp::min(self.row, max_row(all.len(), body_height));
        let start = cmp::min(self.row as usize, all.len());
        let end = cmp::min(start.saturating_add(body_height as usize), all.len());
        let at_end = end == all.len();
        let mut all = all;
        let visible: Vec<VisualRow> = all.drain(start..end).collect();
        let keys: Vec<String> = visible.iter().map(row_fingerprint).collect();
        let show_cursor = self.status_label.is_some();
        let (label, caret) = match &self.status_label {
            Some(label) => {
                let caret = self.status_caret.unwrap_or_else(|| display_width(label).0);
                fit_label_to_caret(label, size.width, caret)
            }
            None if at_end => (take_prefix_cols("(END)", size.width), 0),
            None => (String::new(), 0),
        };
        let status = status_bar_fingerprint(self.terminal_bg, size.width, &label);
        let unchanged = match &self.drawn {
            Some(drawn) => {
                !force
                    && drawn.size == size
                    && drawn.rows == keys
                    && drawn.status == status
                    && drawn.caret == caret
            }
            None => false,
        };
        if unchanged {
            return Ok(());
        }
        let mut stream = Vec::new();
        queue!(&mut stream, MoveTo(0, 0), Clear(ClearType::All))?;
        write_one_line(&visible, terminal_width, &mut stream)?;
        write_status_bar(&mut stream, size, &status)?;
        stream.extend_from_slice(b"\x1b[0m");
        write_cursor(&mut stream, size, caret, show_cursor)?;
        out.write_all(&stream)?;
        out.flush()?;
        self.drawn = Some(DrawnFrame {
            size,
            rows: keys,
            status,
            caret,
        });
        Ok(())
    }

    fn clamp_row(&mut self, size: Size) {
        let count = visual_rows(&self.lines, size.width, None, false).len();
        self.row = cmp::min(self.row, max_row(count, content_height(size.height.0)));
    }
}

/// Rows available for command output. The last terminal row is the status bar.
fn content_height(terminal_height: u32) -> u32 {
    terminal_height.saturating_sub(1)
}

fn max_row(visual_row_count: usize, body_height: u32) -> u32 {
    if body_height == 0 {
        return 0;
    }
    let visual_row_count = u32::try_from(visual_row_count).unwrap_or(u32::MAX);
    visual_row_count.saturating_sub(body_height)
}

struct VisualRow {
    pieces: Vec<Piece>,
    width: Width,
    /// True when this row continues the same logical line after a width wrap.
    soft_wrap: bool,
}

struct Piece {
    sgr: String,
    text: String,
}

fn row_fingerprint(row: &VisualRow) -> String {
    let mut s = String::new();
    for piece in &row.pieces {
        s.push_str(&piece.sgr);
        s.push_str(&piece.text);
    }
    s
}

fn status_bar_fingerprint(terminal_bg: TerminalBg, width: Width, label: &str) -> String {
    if width == 0 {
        return String::new();
    }
    let mut line = String::from(terminal_bg.status_bar_sgr());
    line.push_str(label);
    let mut cols = display_width(label);
    while cols < width {
        line.push(' ');
        cols += Width(1);
    }
    line
}

fn write_cursor(out: &mut impl Write, size: Size, caret: u32, show_cursor: bool) -> io::Result<()> {
    if size.height.0 == 0 || size.width == 0 {
        return Ok(());
    }
    if !show_cursor {
        queue!(out, Hide)?;
        return Ok(());
    }
    let Ok(row) = u16::try_from(size.height.0 - 1) else {
        return Ok(());
    };
    let col = if caret < size.width.0 {
        caret
    } else {
        size.width.0.saturating_sub(1)
    };
    let Ok(col) = u16::try_from(col) else {
        return Ok(());
    };
    queue!(out, MoveTo(col, row), Show)?;
    Ok(())
}

fn display_width(s: &str) -> Width {
    let mut cols = 0u32;
    for c in s.chars() {
        cols = cols.saturating_add(char_width(c));
    }
    Width(cols)
}

fn char_width(c: char) -> u32 {
    u32::try_from(c.width_cjk().unwrap_or(0)).unwrap_or(0)
}

fn take_prefix_cols(s: &str, width: Width) -> String {
    let mut out = String::new();
    let mut cols = Width(0);
    for c in s.chars() {
        let w = Width(char_width(c));
        if cols + w > width {
            break;
        }
        out.push(c);
        cols += w;
    }
    out
}

/// Visible slice of `label` that keeps the caret on screen, and the caret
/// column within that slice.
fn fit_label_to_caret(label: &str, width: Width, caret_cols: u32) -> (String, u32) {
    let total = display_width(label).0;
    if width == 0 {
        return (String::new(), 0);
    }
    if total <= width.0 {
        return (label.to_owned(), caret_cols.min(total));
    }
    let caret = caret_cols.min(total);
    // Keep the caret inside a `width`-column window. Prefer showing the
    // text before the caret (caret near the right edge).
    let mut start = caret.saturating_sub(width.0.saturating_sub(1));
    if start + width.0 > total {
        start = total - width.0;
    }
    let visible = slice_cols(label, start, width.0);
    (visible, caret - start)
}

fn slice_cols(s: &str, skip_cols: u32, max_cols: u32) -> String {
    let mut out = String::new();
    let mut skipped = 0u32;
    let mut taken = 0u32;
    for c in s.chars() {
        let w = char_width(c);
        if skipped < skip_cols {
            // Drop whole characters until we have skipped enough columns.
            // A double-width char that would overshoot the skip is dropped too
            // so the window stays on a character boundary.
            skipped = skipped.saturating_add(w);
            continue;
        }
        if taken + w > max_cols {
            break;
        }
        out.push(c);
        taken += w;
    }
    out
}

fn visual_row_of_logical_line(lines: &[Line], terminal_width: Width, line_idx: usize) -> u32 {
    let mut acc = 0u32;
    for line in lines.iter().take(line_idx) {
        let n = u32::try_from(line_visual_rows(line, terminal_width, None, 0, false).len())
            .unwrap_or(u32::MAX);
        acc = acc.saturating_add(n);
    }
    acc
}

fn logical_line_at_visual_row(lines: &[Line], terminal_width: Width, visual_row: u32) -> usize {
    let mut acc = 0u32;
    for (i, line) in lines.iter().enumerate() {
        let n = u32::try_from(line_visual_rows(line, terminal_width, None, 0, false).len())
            .unwrap_or(u32::MAX);
        // A blank line still occupies one visual row, so n is at least 1
        // unless the terminal width is 0.
        let n = cmp::max(n, 1);
        if visual_row < acc.saturating_add(n) {
            return i;
        }
        acc = acc.saturating_add(n);
    }
    lines.len().saturating_sub(1)
}

fn write_status_bar(out: &mut impl Write, size: Size, status: &str) -> io::Result<()> {
    if size.height.0 == 0 || size.width == 0 || status.is_empty() {
        return Ok(());
    }
    let Ok(row) = u16::try_from(size.height.0 - 1) else {
        return Ok(());
    };
    queue!(out, MoveTo(0, row))?;
    out.write_all(status.as_bytes())?;
    Ok(())
}

fn write_one_line(
    rows: &[VisualRow],
    terminal_width: Width,
    out: &mut impl Write,
) -> io::Result<()> {
    let mut last_sgr: Option<String> = None;
    for (i, row) in rows.iter().enumerate() {
        if i > 0 {
            let prev = &rows[i - 1];
            let soft = row.soft_wrap && prev.width == terminal_width;
            if !soft {
                out.write_all(b"\r\n")?;
            }
        }
        for piece in &row.pieces {
            if last_sgr.as_deref() != Some(piece.sgr.as_str()) {
                out.write_all(piece.sgr.as_bytes())?;
                last_sgr = Some(piece.sgr.clone());
            }
            out.write_all(piece.text.as_bytes())?;
        }
    }
    Ok(())
}

fn visual_rows(
    lines: &[Line],
    terminal_width: Width,
    highlight: Option<&Highlight>,
    light_terminal: bool,
) -> Vec<VisualRow> {
    let mut rows = Vec::new();
    if terminal_width == 0 {
        return rows;
    }
    for (line_idx, line) in lines.iter().enumerate() {
        for part in line_visual_rows(line, terminal_width, highlight, line_idx, light_terminal) {
            rows.push(part);
        }
    }
    rows
}

fn visual_row_offset_for_byte(line: &Line, terminal_width: Width, byte_start: usize) -> u32 {
    if terminal_width == 0 {
        return 0;
    }
    let mut col = Width(0);
    let mut row = 0u32;
    let mut byte_off = 0usize;
    for span in &line.spans {
        for ch in span.content.as_ref().chars() {
            if byte_off >= byte_start {
                return row;
            }
            let text = ch.as_str().as_str();
            byte_off += text.len();
            let w = ch.width();
            if w > terminal_width {
                continue;
            }
            if col + w > terminal_width {
                row = row.saturating_add(1);
                col = Width(0);
            }
            col += w;
        }
    }
    row
}

fn line_visual_rows(
    line: &Line,
    terminal_width: Width,
    highlight: Option<&Highlight>,
    line_idx: usize,
    light_terminal: bool,
) -> Vec<VisualRow> {
    let mut rows = Vec::new();
    if terminal_width == 0 {
        return rows;
    }
    let ranges = match highlight {
        Some(highlight) => line.match_ranges(&highlight.pattern, highlight.ignore_case),
        None => Vec::new(),
    };
    let current_byte = match highlight {
        Some(highlight) => match highlight.current {
            Some((cur_line, byte)) if cur_line == line_idx => Some(byte),
            _ => None,
        },
        None => None,
    };
    let mut pieces: Vec<Piece> = Vec::new();
    let mut col = Width(0);
    let mut soft_wrap = false;
    let mut byte_off = 0usize;
    for span in &line.spans {
        let plain_sgr = span.style.sgr_sequence();
        let highlight_sgr = search_highlight(span.style, light_terminal, false).sgr_sequence();
        let current_sgr = search_highlight(span.style, light_terminal, true).sgr_sequence();
        for ch in span.content.as_ref().chars() {
            let text = ch.as_str().as_str();
            let ch_end = byte_off + text.len();
            let in_match = ranges
                .iter()
                .find(|&&(start, end)| byte_off < end && ch_end > start);
            let sgr = match in_match {
                Some(&(start, _)) if current_byte == Some(start) => &current_sgr,
                Some(_) => &highlight_sgr,
                None => &plain_sgr,
            };
            byte_off = ch_end;
            let w = ch.width();
            if w > terminal_width {
                continue;
            }
            if col + w > terminal_width {
                rows.push(VisualRow {
                    pieces,
                    width: col,
                    soft_wrap,
                });
                pieces = Vec::new();
                col = Width(0);
                soft_wrap = true;
            }
            match pieces.last_mut() {
                Some(last) if last.sgr == *sgr => {
                    last.text.push_str(text);
                }
                _ => {
                    let mut owned = String::new();
                    owned.push_str(text);
                    pieces.push(Piece {
                        sgr: sgr.clone(),
                        text: owned,
                    });
                }
            }
            col += w;
        }
    }
    if !pieces.is_empty() || rows.is_empty() {
        rows.push(VisualRow {
            pieces,
            width: col,
            soft_wrap,
        });
    }
    rows
}

pub(crate) fn terminal_size() -> io::Result<Size> {
    let (cols, rows) = crossterm::terminal::size()?;
    Ok(Size {
        width: Width(u32::from(cols)),
        height: Height(u32::from(rows)),
    })
}

#[cfg(test)]
mod tests {
    use crate::height::Height;
    use crate::render::Renderer;
    use crate::size::Size;
    use crate::terminal::color::TerminalColor;
    use crate::terminal::parse::parse_command_output;
    use crate::terminal::width::Width;
    use crate::terminal_bg::TerminalBg;
    use crate::theme::NVIM_DARK_GREY1;
    use crate::theme::NVIM_DARK_YELLOW;
    use crate::theme::NVIM_LIGHT_YELLOW;

    fn vt_color(color: TerminalColor) -> vt100::Color {
        match color {
            TerminalColor::Rgb(r, g, b) => vt100::Color::Rgb(r, g, b),
            _ => vt100::Color::Default,
        }
    }

    fn size(width: u32, height: u32) -> Size {
        Size {
            width: Width(width),
            height: Height(height),
        }
    }

    fn draw_screen(input: &[u8], width: u16, height: u16, scroll_downs: u32) -> vt100::Parser {
        let size = size(u32::from(width), u32::from(height));
        let mut renderer = Renderer::new(TerminalBg::Dark);
        renderer.set_lines(parse_command_output(input), size);
        for _ in 0..scroll_downs {
            renderer.scroll_down(size);
        }
        let mut buf = Vec::new();
        renderer.draw(&mut buf, size).unwrap();
        let mut parser = vt100::Parser::new(height, width, 0);
        parser.process(&buf);
        parser
    }

    fn row_text(parser: &vt100::Parser, row: u16, width: u16) -> String {
        let mut text = String::new();
        for col in 0..width {
            let Some(cell) = parser.screen().cell(row, col) else {
                break;
            };
            if cell.has_contents() {
                text.push_str(cell.contents());
            } else {
                text.push(' ');
            }
        }
        text.trim_end().to_owned()
    }

    #[test]
    fn test_draw_short_lines() {
        let parser = draw_screen(b"a\nb\nc\n", 10, 5, 0);
        assert_eq!("a", row_text(&parser, 0, 10));
        assert_eq!("b", row_text(&parser, 1, 10));
        assert_eq!("c", row_text(&parser, 2, 10));
        assert_eq!("", row_text(&parser, 3, 10));
        assert_eq!("(END)", row_text(&parser, 4, 10));
    }

    #[test]
    fn test_draw_limits_to_terminal_height() {
        let parser = draw_screen(b"a\nb\nc\nd\n", 10, 3, 0);
        assert_eq!("a", row_text(&parser, 0, 10));
        assert_eq!("b", row_text(&parser, 1, 10));
        assert_eq!("", row_text(&parser, 2, 10));
    }

    #[test]
    fn test_draw_scroll_down() {
        let parser = draw_screen(b"a\nb\nc\nd\n", 10, 3, 1);
        assert_eq!("b", row_text(&parser, 0, 10));
        assert_eq!("c", row_text(&parser, 1, 10));
        assert_eq!("", row_text(&parser, 2, 10));
    }

    #[test]
    fn test_draw_does_not_write_past_last_line() {
        let parser = draw_screen(b"0123456789abcdefghij\nTAIL\n", 10, 3, 0);
        assert_eq!("0123456789", row_text(&parser, 0, 10));
        assert_eq!("abcdefghij", row_text(&parser, 1, 10));
        assert_eq!("", row_text(&parser, 2, 10));
        assert_eq!(
            true,
            parser.screen().contents().contains("0123456789abcdefghij")
        );
        assert_eq!(false, parser.screen().contents().contains("TAIL"));
    }

    #[test]
    fn test_draw_exact_width_line_then_next() {
        let parser = draw_screen(b"0123456789\nnext\n", 10, 3, 0);
        assert_eq!("0123456789", row_text(&parser, 0, 10));
        assert_eq!("next", row_text(&parser, 1, 10));
        assert_eq!("(END)", row_text(&parser, 2, 10));
    }

    #[test]
    fn test_draw_blank_line_consumes_a_row() {
        let parser = draw_screen(b"a\n\nb\n", 10, 5, 0);
        assert_eq!("a", row_text(&parser, 0, 10));
        assert_eq!("", row_text(&parser, 1, 10));
        assert_eq!("b", row_text(&parser, 2, 10));
    }

    #[test]
    fn test_wrapped_line_is_emitted_as_one_line() {
        let mut renderer = Renderer::new(TerminalBg::Dark);
        renderer.set_lines(parse_command_output(b"0123456789abcdefghij"), size(10, 5));
        let mut buf = Vec::new();
        renderer.draw(&mut buf, size(10, 5)).unwrap();
        let text = String::from_utf8_lossy(&buf);
        assert_eq!(true, text.contains("0123456789abcdefghij"));
        let content_at = text.find("0123456789abcdefghij").unwrap();
        let content = &text[content_at..content_at + 20];
        assert_eq!("0123456789abcdefghij", content);
        assert_eq!(false, text.contains('\n'));
        assert_eq!(false, text.contains('\r'));
    }

    #[test]
    fn test_scroll_moves_by_rendered_row() {
        let parser = draw_screen(b"0123456789abcdefghij\nTAIL\n", 10, 3, 1);
        assert_eq!("abcdefghij", row_text(&parser, 0, 10));
        assert_eq!("TAIL", row_text(&parser, 1, 10));
        assert_eq!("(END)", row_text(&parser, 2, 10));
    }

    #[test]
    fn test_scroll_top_and_bottom() {
        let mut renderer = Renderer::new(TerminalBg::Dark);
        renderer.set_lines(parse_command_output(b"a\nb\nc\nd\n"), size(10, 3));
        renderer.scroll_bottom(size(10, 3));
        let mut buf = Vec::new();
        renderer.draw(&mut buf, size(10, 3)).unwrap();
        let mut parser = vt100::Parser::new(3, 10, 0);
        parser.process(&buf);
        assert_eq!("c", row_text(&parser, 0, 10));
        assert_eq!("d", row_text(&parser, 1, 10));
        assert_eq!("(END)", row_text(&parser, 2, 10));
        renderer.scroll_top();
        buf.clear();
        renderer.draw(&mut buf, size(10, 3)).unwrap();
        let mut parser = vt100::Parser::new(3, 10, 0);
        parser.process(&buf);
        assert_eq!("a", row_text(&parser, 0, 10));
        assert_eq!("b", row_text(&parser, 1, 10));
        assert_eq!("", row_text(&parser, 2, 10));
    }

    #[test]
    fn test_scroll_page_down_and_up() {
        let mut renderer = Renderer::new(TerminalBg::Dark);
        renderer.set_lines(parse_command_output(b"a\nb\nc\nd\ne\nf\n"), size(10, 3));
        renderer.scroll_page_down(size(10, 3));
        let mut buf = Vec::new();
        renderer.draw(&mut buf, size(10, 3)).unwrap();
        let mut parser = vt100::Parser::new(3, 10, 0);
        parser.process(&buf);
        assert_eq!("c", row_text(&parser, 0, 10));
        assert_eq!("d", row_text(&parser, 1, 10));
        assert_eq!("", row_text(&parser, 2, 10));
        renderer.scroll_page_up(size(10, 3));
        buf.clear();
        renderer.draw(&mut buf, size(10, 3)).unwrap();
        let mut parser = vt100::Parser::new(3, 10, 0);
        parser.process(&buf);
        assert_eq!("a", row_text(&parser, 0, 10));
        assert_eq!("b", row_text(&parser, 1, 10));
        assert_eq!("", row_text(&parser, 2, 10));
    }

    #[test]
    fn test_scroll_clamps_to_last_page() {
        let parser = draw_screen(b"a\nb\nc\nd\n", 10, 3, 3);
        assert_eq!("c", row_text(&parser, 0, 10));
        assert_eq!("d", row_text(&parser, 1, 10));
        assert_eq!("(END)", row_text(&parser, 2, 10));
    }

    #[test]
    fn test_draw_preserves_fg_and_resets() {
        let parser = draw_screen(b"a\x1b[31mred\x1b[0m b", 10, 2, 0);
        assert_eq!("ared b", row_text(&parser, 0, 10));
        let cell = |col: u16| parser.screen().cell(0, col).unwrap();
        assert_eq!(vt100::Color::Default, cell(0).fgcolor());
        assert_eq!(vt100::Color::Idx(1), cell(1).fgcolor());
        assert_eq!(vt100::Color::Idx(1), cell(2).fgcolor());
        assert_eq!(vt100::Color::Idx(1), cell(3).fgcolor());
        assert_eq!(vt100::Color::Default, cell(4).fgcolor());
        assert_eq!(vt100::Color::Default, cell(5).fgcolor());
    }

    #[test]
    fn test_draw_preserves_bold_and_color_across_wrap() {
        let parser = draw_screen(b"\x1b[1;32m0123456789ab", 10, 3, 0);
        assert_eq!("0123456789", row_text(&parser, 0, 10));
        assert_eq!("ab", row_text(&parser, 1, 10));
        let top = parser.screen().cell(0, 0).unwrap();
        assert_eq!(vt100::Color::Idx(2), top.fgcolor());
        assert_eq!(true, top.bold());
        let wrapped = parser.screen().cell(1, 0).unwrap();
        assert_eq!(vt100::Color::Idx(2), wrapped.fgcolor());
        assert_eq!(true, wrapped.bold());
    }

    #[test]
    fn test_draw_skips_when_visible_unchanged() {
        let mut renderer = Renderer::new(TerminalBg::Dark);
        renderer.set_lines(parse_command_output(b"a\nb\nc\n"), size(10, 2));
        let mut buf = Vec::new();
        renderer.draw(&mut buf, size(10, 2)).unwrap();
        assert_eq!(false, buf.is_empty());
        buf.clear();
        renderer.draw(&mut buf, size(10, 2)).unwrap();
        assert_eq!(true, buf.is_empty());
    }

    #[test]
    fn test_draw_skips_when_change_is_below_visible_area() {
        let mut renderer = Renderer::new(TerminalBg::Dark);
        renderer.set_lines(parse_command_output(b"a\nb\nc\n"), size(10, 3));
        let mut buf = Vec::new();
        renderer.draw(&mut buf, size(10, 3)).unwrap();
        buf.clear();
        renderer.set_lines(parse_command_output(b"a\nb\nX\n"), size(10, 3));
        renderer.draw(&mut buf, size(10, 3)).unwrap();
        assert_eq!(true, buf.is_empty());
    }

    #[test]
    fn test_draw_writes_when_visible_area_changes() {
        let mut renderer = Renderer::new(TerminalBg::Dark);
        renderer.set_lines(parse_command_output(b"a\nb\nc\n"), size(10, 3));
        let mut buf = Vec::new();
        renderer.draw(&mut buf, size(10, 3)).unwrap();
        buf.clear();
        renderer.set_lines(parse_command_output(b"a\nB\nc\n"), size(10, 3));
        renderer.draw(&mut buf, size(10, 3)).unwrap();
        assert_eq!(false, buf.is_empty());
    }

    #[test]
    fn test_redraw_writes_when_visible_unchanged() {
        let mut renderer = Renderer::new(TerminalBg::Dark);
        renderer.set_lines(parse_command_output(b"a\nb\n"), size(10, 2));
        let mut buf = Vec::new();
        renderer.draw(&mut buf, size(10, 2)).unwrap();
        buf.clear();
        renderer.redraw(&mut buf, size(10, 2)).unwrap();
        assert_eq!(false, buf.is_empty());
    }

    #[test]
    fn test_search_highlight_sets_bg() {
        let mut renderer = Renderer::new(TerminalBg::Dark);
        let size = size(20, 3);
        renderer.set_lines(
            parse_command_output(b"xxFOOyy\na\x1b[31mFOO\x1b[0mb\n"),
            size,
        );
        renderer.set_highlight(Some("FOO".to_owned()), Some((0, 2)), false);
        let mut buf = Vec::new();
        renderer.draw(&mut buf, size).unwrap();
        let mut parser = vt100::Parser::new(3, 20, 0);
        parser.process(&buf);
        assert_eq!("xxFOOyy", row_text(&parser, 0, 20));
        let plain = |col: u16| parser.screen().cell(0, col).unwrap();
        assert_eq!(vt100::Color::Default, plain(0).bgcolor());
        assert_eq!(vt100::Color::Default, plain(1).bgcolor());
        // Dark terminal: current hit is light yellow, other hits are dark yellow.
        assert_eq!(vt_color(NVIM_LIGHT_YELLOW), plain(2).bgcolor());
        assert_eq!(vt_color(NVIM_LIGHT_YELLOW), plain(3).bgcolor());
        assert_eq!(vt_color(NVIM_LIGHT_YELLOW), plain(4).bgcolor());
        assert_eq!(vt_color(NVIM_DARK_GREY1), plain(2).fgcolor());
        assert_eq!(vt100::Color::Default, plain(5).bgcolor());
        // Match that crosses a style boundary, on the next line: "a" + "FOO" + "b".
        let crossed = |col: u16| parser.screen().cell(1, col).unwrap();
        assert_eq!("aFOOb", row_text(&parser, 1, 20));
        assert_eq!(vt100::Color::Default, crossed(0).bgcolor());
        assert_eq!(vt_color(NVIM_DARK_YELLOW), crossed(1).bgcolor());
        assert_eq!(vt_color(NVIM_DARK_YELLOW), crossed(2).bgcolor());
        assert_eq!(vt_color(NVIM_DARK_YELLOW), crossed(3).bgcolor());
        assert_eq!(vt100::Color::Default, crossed(4).bgcolor());
    }

    #[test]
    fn test_draw_preserves_bg() {
        let parser = draw_screen(b"\x1b[44mxy", 10, 2, 0);
        assert_eq!("xy", row_text(&parser, 0, 10));
        assert_eq!(
            vt100::Color::Idx(4),
            parser.screen().cell(0, 0).unwrap().bgcolor()
        );
    }

    #[test]
    fn test_status_bar_end_on_dark_bg() {
        let parser = draw_screen(b"a\n", 10, 3, 0);
        assert_eq!("a", row_text(&parser, 0, 10));
        assert_eq!("", row_text(&parser, 1, 10));
        assert_eq!("(END)", row_text(&parser, 2, 10));
        let cell = parser.screen().cell(2, 0).unwrap();
        assert_eq!(vt100::Color::Idx(8), cell.bgcolor());
        let pad = parser.screen().cell(2, 9).unwrap();
        assert_eq!(vt100::Color::Idx(8), pad.bgcolor());
    }

    #[test]
    fn test_status_bar_hides_end_when_more_below() {
        let parser = draw_screen(b"a\nb\nc\n", 10, 3, 0);
        assert_eq!("a", row_text(&parser, 0, 10));
        assert_eq!("b", row_text(&parser, 1, 10));
        assert_eq!("", row_text(&parser, 2, 10));
        let cell = parser.screen().cell(2, 0).unwrap();
        assert_eq!(vt100::Color::Idx(8), cell.bgcolor());
    }

    #[test]
    fn test_status_bar_light_grey_on_light_bg() {
        let mut renderer = Renderer::new(TerminalBg::Light);
        renderer.set_lines(parse_command_output(b"a\n"), size(10, 2));
        let mut buf = Vec::new();
        renderer.draw(&mut buf, size(10, 2)).unwrap();
        let mut parser = vt100::Parser::new(2, 10, 0);
        parser.process(&buf);
        assert_eq!("(END)", row_text(&parser, 1, 10));
        let cell = parser.screen().cell(1, 0).unwrap();
        assert_eq!(vt100::Color::Idx(7), cell.bgcolor());
        let pad = parser.screen().cell(1, 9).unwrap();
        assert_eq!(vt100::Color::Idx(7), pad.bgcolor());
    }

    #[test]
    fn test_draw_writes_when_end_indicator_appears() {
        let mut renderer = Renderer::new(TerminalBg::Dark);
        renderer.set_lines(parse_command_output(b"a\nb\nc\n"), size(10, 3));
        let mut buf = Vec::new();
        renderer.draw(&mut buf, size(10, 3)).unwrap();
        buf.clear();
        renderer.set_lines(parse_command_output(b"a\nb\n"), size(10, 3));
        renderer.draw(&mut buf, size(10, 3)).unwrap();
        assert_eq!(false, buf.is_empty());
    }
}
