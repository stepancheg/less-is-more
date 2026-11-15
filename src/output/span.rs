use crate::output::style::Style;
use crate::output::terminal_str::string::TerminalString;
use crate::output::width::Width;

/// One run of terminal chars that share a style.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Span {
    pub(crate) style: Style,
    pub(crate) content: TerminalString,
}

impl Span {
    pub(crate) fn width(&self) -> Width {
        self.content.width()
    }
}
