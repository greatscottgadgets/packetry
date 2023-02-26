use std::sync::Arc;
use arc_swap::ArcSwapAny;

/// Trait to implement the read-copy-update pattern for a single writer.
pub trait SingleWriterRcu<T: Clone> {
    /// Copy the value, apply a function to it, and replace the current value.
    fn update<F: FnOnce(&mut T)>(&self, f: F);

    /// Copy the value, apply a function to it, and replace the the current
    /// value if the function returns true.
    fn maybe_update<F: FnOnce(&mut T) -> bool>(&self, f: F);
}

impl<T> SingleWriterRcu<T> for ArcSwapAny<Arc<T>>
where T: Clone
{
    fn update<F: FnOnce(&mut T)>(&self, f: F) {
        let mut copy = self.load().as_ref().clone();
        f(&mut copy);
        self.swap(Arc::new(copy));
    }

    fn maybe_update<F: FnOnce(&mut T) -> bool>(&self, f: F) {
        let mut copy = self.load().as_ref().clone();
        if f(&mut copy) {
            self.swap(Arc::new(copy));
        }
    }
}
