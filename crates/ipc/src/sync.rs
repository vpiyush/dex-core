
//! Atomic + cell, sourced from std normally and form loom under `--cfg loom`
//! so the model checker can instrument them
#[cfg(loom)]
pub(crate) use loom::cell::UnsafeCell;
#[cfg(loom)]
pub(crate) use loom::sync::atomic::{fence, AtomicU64, Ordering};


#[cfg(not(loom))]
pub(crate) use std::sync::atomic::{fence, AtomicU64, Ordering};

#[cfg(not(loom))]
#[derive(Debug)]
#[repr(transparent)]
pub(crate) struct UnsafeCell<T>(core::cell::UnsafeCell<T>);

#[cfg(not(loom))]
impl<T> UnsafeCell<T> {
    pub(crate) fn new(data: T) -> Self {
        Self(core::cell::UnsafeCell::new(data))
    }
    pub(crate) fn with<R>(&self, f: impl FnOnce(*const T) -> R) -> R {
        f(self.0.get())
    }

    pub(crate) fn with_mut<R>(&self, f: impl FnOnce(*mut T) -> R) -> R {
        f(self.0.get())
    }
}
