use crate::height::Height;
use crate::output::width::Width;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Size {
    pub(crate) width: Width,
    pub(crate) height: Height,
}
