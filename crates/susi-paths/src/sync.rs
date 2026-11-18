//! Non-poisoning `RwLock` over `std::sync` (`PoisonError::into_inner`).
use std::sync::PoisonError;

pub use std::sync::{RwLockReadGuard, RwLockWriteGuard};

/// A reader-writer lock that ignores poisoning.
#[derive(Default)]
pub struct RwLock<T: ?Sized>(std::sync::RwLock<T>);

impl<T> RwLock<T> {
    /// Usable in `static` items.
    pub const fn new(value: T) -> Self {
        Self(std::sync::RwLock::new(value))
    }
}

impl<T: ?Sized> RwLock<T> {
    /// Shared access; a poisoned lock is recovered.
    pub fn read(&self) -> RwLockReadGuard<'_, T> {
        self.0.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// Exclusive access; a poisoned lock is recovered.
    pub fn write(&self) -> RwLockWriteGuard<'_, T> {
        self.0.write().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<T: ?Sized + std::fmt::Debug> std::fmt::Debug for RwLock<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::RwLock;
    use std::sync::Arc;

    #[test]
    fn a_panicked_writer_does_not_poison_the_rwlock() {
        let lock = Arc::new(RwLock::new(String::from("a")));
        let held = Arc::clone(&lock);
        let joined = std::thread::spawn(move || {
            held.write().push('b');
            let _guard = held.write();
            panic!("writer panics with the guard held");
        })
        .join();
        assert!(joined.is_err());
        assert_eq!(lock.read().as_str(), "ab");
        lock.write().push('c');
        assert_eq!(lock.read().as_str(), "abc");
    }
}
