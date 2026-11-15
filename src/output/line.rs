use crate::output::span::Span;
use crate::output::width::Width;

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
}
