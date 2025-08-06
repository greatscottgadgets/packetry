#[derive(Copy, Clone, Debug)]
#[allow(clippy::enum_variant_names)]
pub enum Decision {
    RejectWithChildren,
    AcceptWithChildren,
    FilterChildren,
}

use Decision::*;

impl Decision {
    pub fn accepts_parent(&self) -> bool {
        matches!(self, AcceptWithChildren | FilterChildren)
    }
}
