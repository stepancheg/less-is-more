use crate::output::width::Width;

#[derive(Copy, Clone, Debug, derive_more::Add, derive_more::Sub)]
pub(crate) struct WidthLen {
    pub(crate) width: Width,
    pub(crate) len: usize,
}
