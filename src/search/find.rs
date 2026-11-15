use crate::render::Renderer;
use crate::search::state::CaseMode;
use crate::search::state::Search;
use crate::size::Size;
use crate::terminal::line::Line;
use crate::terminal::line::compile_search;
use crate::terminal::line::fold_case;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SearchOutcome {
    Found,
    NotFound,
    NoPattern,
    InvalidPattern,
}

#[derive(Clone, Copy)]
pub(crate) struct Occurrence {
    pub(crate) line: usize,
    pub(crate) byte_start: usize,
}

/// Fresh search from the screen edge, or a repeat from the last match.
/// `repeat` continues from the last hit. `forward` is the direction of this
/// jump; the stored direction is updated only by the caller.
pub(crate) fn run_search(
    renderer: &mut Renderer,
    search: &mut Search,
    size: Size,
    forward: bool,
    repeat: bool,
) -> SearchOutcome {
    if search.pattern.is_empty() {
        renderer.set_highlight(None, None, false);
        return SearchOutcome::NoPattern;
    }
    let ignore_case = search.folds_pattern();
    let re = match compile_search(&search.pattern, ignore_case) {
        Ok(re) => re,
        Err(_) => {
            renderer.set_highlight(None, None, false);
            return SearchOutcome::InvalidPattern;
        }
    };
    let occs = occurrences_with(renderer.lines(), &re);
    let picked = pick_occurrence(renderer, search, size, &occs, forward, repeat);
    match picked {
        Some(idx) => {
            let occ = occs[idx];
            search.current = Some((occ.line, occ.byte_start));
            search.apply_highlight(renderer);
            renderer.scroll_to_match(occ.line, occ.byte_start, size);
            SearchOutcome::Found
        }
        None => {
            if !repeat {
                search.current = None;
            }
            search.apply_highlight(renderer);
            SearchOutcome::NotFound
        }
    }
}

/// Accept a submitted prompt query. An empty query repeats the previous pattern.
pub(crate) fn submit_prompt(
    renderer: &mut Renderer,
    search: &mut Search,
    size: Size,
    forward: bool,
    query: String,
) -> SearchOutcome {
    if query.is_empty() {
        if search.pattern.is_empty() {
            renderer.set_highlight(None, None, false);
            return SearchOutcome::NoPattern;
        }
    } else {
        let mode = search.case_mode();
        let ignore_case = fold_case(
            &query,
            mode == CaseMode::IgnoreCase,
            mode == CaseMode::IgnoreCaseAlways,
        );
        if compile_search(&query, ignore_case).is_err() {
            return SearchOutcome::InvalidPattern;
        }
        search.pattern = query;
    }
    search.forward = forward;
    run_search(renderer, search, size, forward, false)
}

pub(crate) fn search_next(
    renderer: &mut Renderer,
    search: &mut Search,
    size: Size,
    reverse: bool,
) -> SearchOutcome {
    let forward = if reverse {
        !search.forward
    } else {
        search.forward
    };
    run_search(renderer, search, size, forward, true)
}

pub(crate) fn occurrences(lines: &[Line], pattern: &str, ignore_case: bool) -> Vec<Occurrence> {
    if pattern.is_empty() {
        return Vec::new();
    }
    let Ok(re) = compile_search(pattern, ignore_case) else {
        return Vec::new();
    };
    occurrences_with(lines, &re)
}

fn occurrences_with(lines: &[Line], re: &regex::Regex) -> Vec<Occurrence> {
    let mut occs = Vec::new();
    for (line, text) in lines.iter().enumerate() {
        for (byte_start, _) in text.match_ranges(re) {
            occs.push(Occurrence { line, byte_start });
        }
    }
    occs
}

fn pick_occurrence(
    renderer: &Renderer,
    search: &Search,
    size: Size,
    occs: &[Occurrence],
    forward: bool,
    repeat: bool,
) -> Option<usize> {
    if occs.is_empty() {
        return None;
    }
    if repeat {
        if let Some((line, byte)) = search.current {
            return if forward {
                occs.iter()
                    .position(|occ| pos_after(occ.line, occ.byte_start, line, byte))
            } else {
                occs.iter()
                    .rposition(|occ| pos_before(occ.line, occ.byte_start, line, byte))
            };
        }
    }
    if forward {
        let top = renderer.top_line_index(size);
        occs.iter().position(|occ| occ.line >= top)
    } else {
        let bottom = renderer.bottom_line_index(size);
        occs.iter().rposition(|occ| occ.line <= bottom)
    }
}

fn pos_after(line: usize, byte: usize, cur_line: usize, cur_byte: usize) -> bool {
    line > cur_line || (line == cur_line && byte > cur_byte)
}

fn pos_before(line: usize, byte: usize, cur_line: usize, cur_byte: usize) -> bool {
    line < cur_line || (line == cur_line && byte < cur_byte)
}

#[cfg_attr(not(test), expect(dead_code))]
pub(crate) fn find_forward(lines: &[Line], pattern: &str, start: usize) -> Option<usize> {
    if pattern.is_empty() {
        return None;
    }
    for (i, line) in lines.iter().enumerate().skip(start) {
        if line_contains(line, pattern) {
            return Some(i);
        }
    }
    None
}

#[cfg_attr(not(test), expect(dead_code))]
pub(crate) fn find_backward(lines: &[Line], pattern: &str, start: usize) -> Option<usize> {
    if pattern.is_empty() || lines.is_empty() {
        return None;
    }
    let start = if start >= lines.len() {
        lines.len() - 1
    } else {
        start
    };
    let mut i = start;
    loop {
        if line_contains(&lines[i], pattern) {
            return Some(i);
        }
        if i == 0 {
            return None;
        }
        i -= 1;
    }
}

#[cfg_attr(not(test), expect(dead_code))]
fn line_contains(line: &Line, pattern: &str) -> bool {
    match compile_search(pattern, false) {
        Ok(re) => !line.match_ranges(&re).is_empty(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use crate::height::Height;
    use crate::render::Renderer;
    use crate::search::find::SearchOutcome;
    use crate::search::find::find_backward;
    use crate::search::find::find_forward;
    use crate::search::find::search_next;
    use crate::search::find::submit_prompt;
    use crate::search::state::Search;
    use crate::size::Size;
    use crate::terminal::color::TerminalColor;
    use crate::terminal::parse::parse_command_output;
    use crate::terminal::width::Width;
    use crate::terminal_bg::TerminalBg;
    use crate::theme::NVIM_LIGHT_YELLOW;
    use crate::wrapped_row::WrappedRow;

    fn vt_parser(rows: u16, cols: u16) -> vt100::Parser {
        vt100::Parser::new(
            std::num::NonZeroU16::new(rows).unwrap(),
            std::num::NonZeroU16::new(cols).unwrap(),
            0,
        )
    }

    fn size(width: u32, height: u32) -> Size {
        Size {
            width: Width(width),
            height: Height(height),
        }
    }

    fn assert_line_visible(renderer: &Renderer, size: Size, line: usize) {
        let top = renderer.top_line_index(size);
        let bottom = renderer.bottom_line_index(size);
        assert_eq!(true, line >= top && line <= bottom);
    }

    #[test]
    fn test_find_forward_and_backward() {
        let lines = parse_command_output(b"aaa\nfoo\nbbb\nfoo\nccc\n");
        assert_eq!(Some(1), find_forward(&lines, "foo", 0));
        assert_eq!(Some(3), find_forward(&lines, "foo", 2));
        assert_eq!(None, find_forward(&lines, "foo", 4));
        assert_eq!(Some(3), find_backward(&lines, "foo", 4));
        assert_eq!(Some(1), find_backward(&lines, "foo", 2));
        assert_eq!(None, find_backward(&lines, "foo", 0));
        assert_eq!(None, find_forward(&lines, "", 0));
    }

    #[test]
    fn test_find_spans_style_boundary() {
        let lines = parse_command_output(b"a\x1b[31mb\x1b[0mc\n");
        assert_eq!(Some(0), find_forward(&lines, "abc", 0));
    }

    #[test]
    fn test_find_regex_dot() {
        let lines = parse_command_output(b"foo\nfao\nf\no\n");
        assert_eq!(Some(0), find_forward(&lines, "f.o", 0));
        assert_eq!(Some(1), find_forward(&lines, "f.o", 1));
        assert_eq!(None, find_forward(&lines, "f.o", 2));
    }

    #[test]
    fn test_invalid_pattern() {
        let mut renderer = Renderer::new(TerminalBg::Dark);
        let size = size(10, 4);
        renderer.set_lines(parse_command_output(b"foo\n"), size);
        let mut search = Search::new();
        assert_eq!(
            SearchOutcome::Found,
            submit_prompt(&mut renderer, &mut search, size, true, "foo".to_owned())
        );
        assert_eq!(
            SearchOutcome::InvalidPattern,
            submit_prompt(&mut renderer, &mut search, size, true, "[".to_owned())
        );
        // Previous pattern is kept.
        assert_eq!("foo", search.pattern);
        assert_eq!(
            Some("[1/1]".to_owned()),
            search.match_label(renderer.lines())
        );
    }

    #[test]
    fn test_find_is_case_sensitive() {
        let lines = parse_command_output(b"Foo\n");
        assert_eq!(None, find_forward(&lines, "foo", 0));
        assert_eq!(Some(0), find_forward(&lines, "Foo", 0));
    }

    #[test]
    fn test_submit_scrolls_to_match() {
        let mut renderer = Renderer::new(TerminalBg::Dark);
        let size = size(10, 4);
        renderer.set_lines(
            parse_command_output(b"a\nb\nc\nd\nTARGET\nf\ng\nh\ni\n"),
            size,
        );
        let mut search = Search::new();
        let outcome = submit_prompt(&mut renderer, &mut search, size, true, "TARGET".to_owned());
        assert_eq!(SearchOutcome::Found, outcome);
        assert_line_visible(&renderer, size, 4);
    }

    #[test]
    fn test_submit_backward_scrolls_to_match() {
        let mut renderer = Renderer::new(TerminalBg::Dark);
        let size = size(10, 4);
        renderer.set_lines(parse_command_output(b"TARGET\nb\nc\nd\ne\nf\n"), size);
        renderer.scroll_bottom(size);
        let mut search = Search::new();
        let outcome = submit_prompt(&mut renderer, &mut search, size, false, "TARGET".to_owned());
        assert_eq!(SearchOutcome::Found, outcome);
        assert_eq!(0, renderer.top_line_index(size));
    }

    #[test]
    fn test_search_next_moves_to_following_match() {
        let mut renderer = Renderer::new(TerminalBg::Dark);
        let size = size(10, 3);
        renderer.set_lines(
            parse_command_output(b"hit\na\nhit\nb\nhit\nx\nx\nx\n"),
            size,
        );
        let mut search = Search::new();
        assert_eq!(
            SearchOutcome::Found,
            submit_prompt(&mut renderer, &mut search, size, true, "hit".to_owned())
        );
        assert_eq!(0, renderer.top_line_index(size));
        assert_eq!(
            SearchOutcome::Found,
            search_next(&mut renderer, &mut search, size, false)
        );
        assert_line_visible(&renderer, size, 2);
        assert_eq!(
            SearchOutcome::Found,
            search_next(&mut renderer, &mut search, size, false)
        );
        assert_line_visible(&renderer, size, 4);
        let top_at_third = renderer.top_line_index(size);
        assert_eq!(
            SearchOutcome::NotFound,
            search_next(&mut renderer, &mut search, size, false)
        );
        assert_eq!(top_at_third, renderer.top_line_index(size));
        assert_eq!(
            SearchOutcome::NotFound,
            search_next(&mut renderer, &mut search, size, false)
        );
        assert_eq!(top_at_third, renderer.top_line_index(size));
    }

    #[test]
    fn test_search_prev_reverses_direction() {
        let mut renderer = Renderer::new(TerminalBg::Dark);
        let size = size(10, 3);
        renderer.set_lines(
            parse_command_output(b"hit\na\nhit\nb\nhit\nx\nx\nx\n"),
            size,
        );
        let mut search = Search::new();
        submit_prompt(&mut renderer, &mut search, size, true, "hit".to_owned());
        search_next(&mut renderer, &mut search, size, false);
        assert_line_visible(&renderer, size, 2);
        assert_eq!(
            SearchOutcome::Found,
            search_next(&mut renderer, &mut search, size, true)
        );
        assert_line_visible(&renderer, size, 0);
        assert_eq!(0, renderer.top_line_index(size));
    }

    #[test]
    fn test_search_next_centers_off_screen_match() {
        let mut renderer = Renderer::new(TerminalBg::Dark);
        // Content height is 5. A hit on line 10 is off the first page.
        let size = size(10, 6);
        let mut text = String::new();
        for i in 0..20 {
            if i == 10 {
                text.push_str("TARGET\n");
            } else {
                text.push_str("x\n");
            }
        }
        renderer.set_lines(parse_command_output(text.as_bytes()), size);
        let mut search = Search::new();
        assert_eq!(
            SearchOutcome::Found,
            submit_prompt(&mut renderer, &mut search, size, true, "TARGET".to_owned())
        );
        // body/2 == 2, so the hit sits on the middle row: top is line 8.
        assert_eq!(WrappedRow(8), renderer.top_row(size));
        assert_eq!(8, renderer.top_line_index(size));
        assert_line_visible(&renderer, size, 10);
    }

    #[test]
    fn test_search_next_does_not_scroll_when_match_is_on_screen() {
        let mut renderer = Renderer::new(TerminalBg::Dark);
        // Content height is 5, so lines 0..4 are on the first page.
        let size = size(20, 6);
        renderer.set_lines(
            parse_command_output(b"one\ntwo\nthree\nfour\nfive\nsix\n"),
            size,
        );
        let mut search = Search::new();
        assert_eq!(
            SearchOutcome::Found,
            submit_prompt(&mut renderer, &mut search, size, true, "o".to_owned())
        );
        // "one" is already at the top; page must not move.
        assert_eq!(WrappedRow(0), renderer.top_row(size));
        let top_before = renderer.top_row(size);
        // Next hit is "two", still on this page.
        assert_eq!(
            SearchOutcome::Found,
            search_next(&mut renderer, &mut search, size, false)
        );
        assert_eq!(
            Some("[2/3]".to_owned()),
            search.match_label(renderer.lines())
        );
        assert_eq!(top_before, renderer.top_row(size));
        // "four" is also on this page.
        assert_eq!(
            SearchOutcome::Found,
            search_next(&mut renderer, &mut search, size, false)
        );
        assert_eq!(
            Some("[3/3]".to_owned()),
            search.match_label(renderer.lines())
        );
        assert_eq!(top_before, renderer.top_row(size));
    }

    #[test]
    fn test_occurrence_index_on_same_line() {
        let mut renderer = Renderer::new(TerminalBg::Dark);
        let size = size(20, 6);
        renderer.set_lines(
            parse_command_output(b"foo foo\nbar\nfoo\nx\nx\nx\nx\nx\n"),
            size,
        );
        let mut search = Search::new();
        assert_eq!(
            SearchOutcome::Found,
            submit_prompt(&mut renderer, &mut search, size, true, "foo".to_owned())
        );
        assert_eq!(
            Some("[1/3]".to_owned()),
            search.match_label(renderer.lines())
        );
        assert_eq!(
            SearchOutcome::Found,
            search_next(&mut renderer, &mut search, size, false)
        );
        assert_eq!(
            Some("[2/3]".to_owned()),
            search.match_label(renderer.lines())
        );
        assert_eq!(0, renderer.top_line_index(size));
        assert_eq!(
            SearchOutcome::Found,
            search_next(&mut renderer, &mut search, size, false)
        );
        assert_eq!(
            Some("[3/3]".to_owned()),
            search.match_label(renderer.lines())
        );
        // The third hit is already on the first page, so the viewport stays put.
        assert_eq!(0, renderer.top_line_index(size));

        let mut buf = Vec::new();
        renderer.draw(&mut buf, size).unwrap();
        let mut parser = vt_parser(6, 20);
        parser.process(&buf);
        // Current match is the "foo" on logical line 2, still on screen.
        let cell = parser.screen().cell(2, 0).unwrap();
        let TerminalColor::Rgb(r, g, b) = NVIM_LIGHT_YELLOW else {
            panic!("NVIM_LIGHT_YELLOW is RGB");
        };
        assert_eq!(vt100::Color::Rgb(r, g, b), cell.bgcolor());
    }

    #[test]
    fn test_empty_submit_repeats_pattern() {
        let mut renderer = Renderer::new(TerminalBg::Dark);
        let size = size(10, 3);
        renderer.set_lines(parse_command_output(b"hit\na\nhit\nx\nx\nx\n"), size);
        let mut search = Search::new();
        submit_prompt(&mut renderer, &mut search, size, true, "hit".to_owned());
        assert_eq!(0, renderer.top_line_index(size));
        // Move off the first hit, then an empty forward submit starts again
        // from the current screen and can land on the second hit.
        renderer.scroll_to_line(1, size);
        search.clear_match();
        assert_eq!(
            SearchOutcome::Found,
            submit_prompt(&mut renderer, &mut search, size, true, String::new())
        );
        // Second hit is on the page that starts at line 1, so do not jump.
        assert_eq!(1, renderer.top_line_index(size));
        assert_line_visible(&renderer, size, 2);
    }

    #[test]
    fn test_empty_submit_without_pattern() {
        let mut renderer = Renderer::new(TerminalBg::Dark);
        let size = size(10, 3);
        renderer.set_lines(parse_command_output(b"a\n"), size);
        let mut search = Search::new();
        assert_eq!(
            SearchOutcome::NoPattern,
            submit_prompt(&mut renderer, &mut search, size, true, String::new())
        );
    }

    #[test]
    fn test_search_next_after_move_restarts_from_screen_edge() {
        let mut renderer = Renderer::new(TerminalBg::Dark);
        // Content height is 5; three pages of hits.
        let size = size(20, 6);
        renderer.set_lines(
            parse_command_output(b"hit0\nx\nx\nx\nx\nhit1\nx\nx\nx\nx\nhit2\n"),
            size,
        );
        let mut search = Search::new();
        assert_eq!(
            SearchOutcome::Found,
            submit_prompt(&mut renderer, &mut search, size, true, "hit".to_owned())
        );
        assert_eq!(
            Some("[1/3]".to_owned()),
            search.match_label(renderer.lines())
        );
        // Move to the last page, dropping the selected hit as a scroll does.
        renderer.scroll_bottom(size);
        search.clear_match();
        assert_eq!(
            SearchOutcome::Found,
            search_next(&mut renderer, &mut search, size, false)
        );
        // Forward restart begins at the top of the screen, not after hit0.
        assert_eq!(
            Some("[3/3]".to_owned()),
            search.match_label(renderer.lines())
        );

        // Backward restart begins at the bottom of the screen.
        renderer.scroll_top();
        search.clear_match();
        search.forward = false;
        assert_eq!(
            SearchOutcome::Found,
            search_next(&mut renderer, &mut search, size, false)
        );
        assert_eq!(
            Some("[1/3]".to_owned()),
            search.match_label(renderer.lines())
        );
    }

    #[test]
    fn test_search_next_resumes_last_history_pattern() {
        let mut renderer = Renderer::new(TerminalBg::Dark);
        let size = size(10, 6);
        renderer.set_lines(parse_command_output(b"a\nfoo\nb\n"), size);
        let mut search = Search::new();
        assert_eq!(
            SearchOutcome::NoPattern,
            search_next(&mut renderer, &mut search, size, false)
        );
        let mut entries = Vec::new();
        entries.push("old".to_owned());
        entries.push("foo".to_owned());
        search.resume_from_history(&entries);
        assert_eq!(
            SearchOutcome::Found,
            search_next(&mut renderer, &mut search, size, false)
        );
        assert_eq!(
            Some("[1/1]".to_owned()),
            search.match_label(renderer.lines())
        );
        // An in-session pattern is left alone.
        let mut other = Vec::new();
        other.push("bar".to_owned());
        search.resume_from_history(&other);
        assert_eq!("foo", search.pattern);
    }

    #[test]
    fn test_not_found_does_not_move() {
        let mut renderer = Renderer::new(TerminalBg::Dark);
        let size = size(10, 3);
        renderer.set_lines(parse_command_output(b"a\nb\nc\nd\n"), size);
        renderer.scroll_down(size);
        let before = renderer.top_line_index(size);
        let mut search = Search::new();
        assert_eq!(
            SearchOutcome::NotFound,
            submit_prompt(&mut renderer, &mut search, size, true, "zzz".to_owned())
        );
        assert_eq!(before, renderer.top_line_index(size));
    }
}
