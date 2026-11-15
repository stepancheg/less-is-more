use crate::terminal::span::Span;
use crate::terminal::width::Width;

/// One row of styled spans. Not yet wrapped to the terminal width.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Line {
    pub(crate) spans: Vec<Span>,
}

impl Line {
    pub(crate) fn new() -> Line {
        Line { spans: Vec::new() }
    }

    pub(crate) fn width(&self) -> Width {
        self.spans.iter().map(|span| span.width()).sum()
    }

    pub(crate) fn plain_text(&self) -> String {
        let mut text = String::new();
        for span in &self.spans {
            text.push_str(span.content.as_ref().as_str());
        }
        text
    }

    /// Non-overlapping byte ranges of `pattern` in this line's plain text.
    /// When `fold` is true, comparison ignores case.
    pub(crate) fn match_ranges(&self, pattern: &str, fold: bool) -> Vec<(usize, usize)> {
        literal_match_ranges(&self.plain_text(), pattern, fold)
    }
}

/// less case rules: `-I` always folds; `-i` folds only when `pattern` has no uppercase.
pub(crate) fn fold_case(pattern: &str, ignore_case: bool, ignore_case_always: bool) -> bool {
    ignore_case_always || (ignore_case && !pattern.chars().any(char::is_uppercase))
}

pub(crate) fn literal_match_ranges(text: &str, pattern: &str, fold: bool) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    if pattern.is_empty() {
        return ranges;
    }
    if !fold {
        let mut start = 0;
        while start < text.len() {
            let Some(rel) = text[start..].find(pattern) else {
                break;
            };
            let abs = start + rel;
            let end = abs + pattern.len();
            ranges.push((abs, end));
            start = end;
        }
        return ranges;
    }
    let pat: Vec<char> = pattern.chars().collect();
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut i = 0;
    while i + pat.len() <= chars.len() {
        let mut ok = true;
        for (j, pc) in pat.iter().enumerate() {
            if !chars_eq_ignore_case(chars[i + j].1, *pc) {
                ok = false;
                break;
            }
        }
        if ok {
            let start = chars[i].0;
            let end_char = i + pat.len();
            let end = if end_char < chars.len() {
                chars[end_char].0
            } else {
                text.len()
            };
            ranges.push((start, end));
            i = end_char;
        } else {
            i += 1;
        }
    }
    ranges
}

fn chars_eq_ignore_case(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase())
}
