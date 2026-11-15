use std::cmp;

use crate::render::Renderer;
use crate::search::find::occurrences;
use crate::terminal::line::Line;
use crate::terminal::line::fold_case;

pub(crate) const PATTERN_NOT_FOUND: &str = "Pattern not found";
pub(crate) const NO_PREVIOUS_SEARCH: &str = "No previous search";

/// Status line to show after a command-output refresh.
/// A "Pattern not found" message stays up until the user issues a command.
pub(crate) fn status_after_output(
    current_status: Option<&str>,
    match_label: Option<String>,
) -> Option<String> {
    let not_found = current_status.is_some_and(|status| status.starts_with(PATTERN_NOT_FOUND));
    if !not_found {
        return match_label;
    }
    match match_label {
        Some(label) => {
            let mut msg = String::from(PATTERN_NOT_FOUND);
            msg.push(' ');
            msg.push_str(&label);
            Some(msg)
        }
        None => Some(PATTERN_NOT_FOUND.to_owned()),
    }
}

/// less `-i` / `-I` case handling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CaseMode {
    Sensitive,
    /// `-i`: ignore case when the pattern has no uppercase letters.
    IgnoreCase,
    /// `-I`: ignore case even when the pattern has uppercase letters.
    IgnoreCaseAlways,
}

impl CaseMode {
    /// Status text describing this `-i`/`-I` case mode.
    pub(crate) fn label(&self) -> &'static str {
        match *self {
            CaseMode::IgnoreCaseAlways => "Ignore case always",
            CaseMode::IgnoreCase => "Ignore case",
            CaseMode::Sensitive => "Case sensitive",
        }
    }
}

pub(crate) struct Search {
    pub(crate) pattern: String,
    pub(crate) forward: bool,
    case_mode: CaseMode,
    /// Selected hit: line index and byte offset in that line's plain text.
    /// `n`/`N` continue from the next occurrence.
    pub(crate) current: Option<(usize, usize)>,
}

/// Selected-hit position captured before a refresh replaces the lines.
#[derive(Clone, Copy)]
pub(crate) struct MatchAnchor {
    index: Option<usize>,
    line: usize,
    byte: usize,
}

impl Search {
    pub(crate) fn new() -> Search {
        Search {
            pattern: String::new(),
            forward: true,
            case_mode: CaseMode::Sensitive,
            current: None,
        }
    }

    pub(crate) fn case_mode(&self) -> CaseMode {
        self.case_mode
    }

    pub(crate) fn toggle_ignore_case(&mut self) {
        self.case_mode = match self.case_mode {
            CaseMode::IgnoreCase => CaseMode::Sensitive,
            CaseMode::Sensitive | CaseMode::IgnoreCaseAlways => CaseMode::IgnoreCase,
        };
    }

    pub(crate) fn toggle_ignore_case_always(&mut self) {
        self.case_mode = match self.case_mode {
            CaseMode::IgnoreCaseAlways => CaseMode::Sensitive,
            CaseMode::Sensitive | CaseMode::IgnoreCase => CaseMode::IgnoreCaseAlways,
        };
    }

    pub(crate) fn folds_pattern(&self) -> bool {
        let ignore_case = self.case_mode == CaseMode::IgnoreCase;
        let ignore_case_always = self.case_mode == CaseMode::IgnoreCaseAlways;
        fold_case(&self.pattern, ignore_case, ignore_case_always)
    }

    pub(crate) fn clear_match(&mut self) {
        self.current = None;
    }

    pub(crate) fn resume_from_history(&mut self, entries: &[String]) {
        if !self.pattern.is_empty() {
            return;
        }
        if let Some(last) = entries.last() {
            if !last.is_empty() {
                self.pattern = last.clone();
            }
        }
    }

    /// Remember which hit is selected, before the lines are replaced.
    pub(crate) fn anchor(&self, lines: &[Line]) -> Option<MatchAnchor> {
        let (line, byte) = self.current?;
        let index = occurrences(lines, &self.pattern, self.folds_pattern())
            .iter()
            .position(|occ| occ.line == line && occ.byte_start == byte);
        Some(MatchAnchor { index, line, byte })
    }

    /// Point `current` at a hit in the new lines. Prefer the same
    /// coordinates, then the same occurrence index.
    pub(crate) fn reattach(&mut self, lines: &[Line], anchor: Option<MatchAnchor>) {
        let Some(anchor) = anchor else {
            return;
        };
        let occs = occurrences(lines, &self.pattern, self.folds_pattern());
        if occs.is_empty() {
            self.current = None;
            return;
        }
        if let Some(occ) = occs
            .iter()
            .find(|occ| occ.line == anchor.line && occ.byte_start == anchor.byte)
        {
            self.current = Some((occ.line, occ.byte_start));
            return;
        }
        if let Some(index) = anchor.index {
            let occ = occs[cmp::min(index, occs.len() - 1)];
            self.current = Some((occ.line, occ.byte_start));
            return;
        }
        let occ = match occs.iter().find(|occ| occ.line >= anchor.line) {
            Some(occ) => *occ,
            None => occs[occs.len() - 1],
        };
        self.current = Some((occ.line, occ.byte_start));
    }

    /// Vim-style `[i/n]` for the selected hit, if there is one.
    pub(crate) fn match_label(&self, lines: &[Line]) -> Option<String> {
        let (line, byte) = self.current?;
        let occs = occurrences(lines, &self.pattern, self.folds_pattern());
        let index = occs
            .iter()
            .position(|occ| occ.line == line && occ.byte_start == byte)?;
        Some(format!("[{}/{}]", index + 1, occs.len()))
    }

    pub(crate) fn apply_highlight(&self, renderer: &mut Renderer) {
        if self.pattern.is_empty() {
            renderer.set_highlight(None, None, false);
        } else {
            renderer.set_highlight(
                Some(self.pattern.clone()),
                self.current,
                self.folds_pattern(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::height::Height;
    use crate::render::Renderer;
    use crate::search::find::SearchOutcome;
    use crate::search::find::search_next;
    use crate::search::find::submit_prompt;
    use crate::search::state::CaseMode;
    use crate::search::state::Search;
    use crate::search::state::status_after_output;
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

    #[test]
    fn test_ignore_case_matches_unless_pattern_has_uppercase() {
        let mut renderer = Renderer::new(TerminalBg::Dark);
        let size = size(10, 6);
        renderer.set_lines(parse_command_output(b"FOO\nfoo\nFoo\nbar\n"), size);
        let mut search = Search::new();
        search.toggle_ignore_case();
        assert_eq!(CaseMode::IgnoreCase, search.case_mode());
        assert_eq!(
            SearchOutcome::Found,
            submit_prompt(&mut renderer, &mut search, size, true, "foo".to_owned())
        );
        assert_eq!(
            Some("[1/3]".to_owned()),
            search.match_label(renderer.lines())
        );
        // less -i: a pattern with uppercase is matched case-sensitively.
        assert_eq!(
            SearchOutcome::Found,
            submit_prompt(&mut renderer, &mut search, size, true, "Foo".to_owned())
        );
        assert_eq!(
            Some("[1/1]".to_owned()),
            search.match_label(renderer.lines())
        );
        // less -I: ignore case even when the pattern has uppercase.
        search.toggle_ignore_case_always();
        assert_eq!(CaseMode::IgnoreCaseAlways, search.case_mode());
        assert_eq!(
            SearchOutcome::Found,
            submit_prompt(&mut renderer, &mut search, size, true, "Foo".to_owned())
        );
        assert_eq!(
            Some("[1/3]".to_owned()),
            search.match_label(renderer.lines())
        );
        assert_eq!("Ignore case always", search.case_mode().label());
    }

    #[test]
    fn test_reattach_keeps_current_across_refresh() {
        let mut renderer = Renderer::new(TerminalBg::Dark);
        let size = size(20, 8);
        renderer.set_lines(
            parse_command_output(b"foo foo\nbar\nfoo\nx\nx\nx\nx\nx\n"),
            size,
        );
        let mut search = Search::new();
        submit_prompt(&mut renderer, &mut search, size, true, "foo".to_owned());
        search_next(&mut renderer, &mut search, size, false);
        assert_eq!(
            Some("[2/3]".to_owned()),
            search.match_label(renderer.lines())
        );
        let anchor = search.anchor(renderer.lines());
        // Same text, as a refresh that does not move the hit.
        let again = parse_command_output(b"foo foo\nbar\nfoo\nx\nx\nx\nx\nx\n");
        search.reattach(&again, anchor);
        assert_eq!(Some("[2/3]".to_owned()), search.match_label(&again));
        // A line inserted above shifts coordinates; keep the same index.
        let shifted = parse_command_output(b"zz\nfoo foo\nbar\nfoo\nx\nx\nx\nx\nx\n");
        search.reattach(&shifted, anchor);
        assert_eq!(Some("[2/3]".to_owned()), search.match_label(&shifted));
    }

    #[test]
    fn test_pattern_not_found_survives_output_refresh() {
        assert_eq!(
            Some("Pattern not found".to_owned()),
            status_after_output(Some("Pattern not found"), None)
        );
        assert_eq!(
            Some("Pattern not found [2/3]".to_owned()),
            status_after_output(Some("Pattern not found [1/3]"), Some("[2/3]".to_owned()))
        );
        assert_eq!(
            Some("[2/3]".to_owned()),
            status_after_output(Some("[1/3]"), Some("[2/3]".to_owned()))
        );
        assert_eq!(None, status_after_output(None, None));
    }
}
