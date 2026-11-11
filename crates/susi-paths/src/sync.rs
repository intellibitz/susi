//! Non-poisoning locks over `std::sync`, for the crates that used the
//! `parking_lot` facade (T-SMALLFACADES-1).
//!
//! `parking_lot` locks never poison: a panic while a guard is held leaves
//! the data reachable for every later caller. `std::sync` locks poison
//! instead, so a plain `lock().unwrap()` would turn one panicked holder
//! into a panic for every later caller. These wrappers keep the
//! `parking_lot` contract on the standard library: `lock`, `read` and
//! `write` return the guard directly and recover a poisoned lock
//! (`PoisonError::into_inner`), so call sites read exactly as before.
use std::sync::{PoisonError, TryLockError};

pub use std::sync::{MutexGuard, RwLockReadGuard, RwLockWriteGuard};

/// A mutual-exclusion lock that ignores poisoning.
#[derive(Default)]
pub struct Mutex<T: ?Sized>(std::sync::Mutex<T>);

impl<T> Mutex<T> {
    /// Usable in `static` items, like `parking_lot::Mutex::new`.
    pub const fn new(value: T) -> Self {
        Self(std::sync::Mutex::new(value))
    }

    pub fn into_inner(self) -> T {
        self.0.into_inner().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<T: ?Sized> Mutex<T> {
    /// Blocks until the lock is held; a poisoned lock is recovered.
    pub fn lock(&self) -> MutexGuard<'_, T> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `None` only while another holder has the lock.
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        match self.0.try_lock() {
            Ok(guard) => Some(guard),
            Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        }
    }

    pub fn get_mut(&mut self) -> &mut T {
        self.0.get_mut().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<T: ?Sized + std::fmt::Debug> std::fmt::Debug for Mutex<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// A reader-writer lock that ignores poisoning.
#[derive(Default)]
pub struct RwLock<T: ?Sized>(std::sync::RwLock<T>);

impl<T> RwLock<T> {
    /// Usable in `static` items, like `parking_lot::RwLock::new`.
    pub const fn new(value: T) -> Self {
        Self(std::sync::RwLock::new(value))
    }

    pub fn into_inner(self) -> T {
        self.0.into_inner().unwrap_or_else(PoisonError::into_inner)
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

    /// `None` only while a writer holds the lock.
    pub fn try_read(&self) -> Option<RwLockReadGuard<'_, T>> {
        match self.0.try_read() {
            Ok(guard) => Some(guard),
            Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        }
    }

    /// `None` only while another holder has the lock.
    pub fn try_write(&self) -> Option<RwLockWriteGuard<'_, T>> {
        match self.0.try_write() {
            Ok(guard) => Some(guard),
            Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        }
    }

    pub fn get_mut(&mut self) -> &mut T {
        self.0.get_mut().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<T: ?Sized + std::fmt::Debug> std::fmt::Debug for RwLock<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::{Mutex, RwLock};
    use std::sync::Arc;

    static COUNTER: Mutex<u32> = Mutex::new(0);

    #[test]
    fn a_static_mutex_counts() {
        *COUNTER.lock() += 1;
        assert!(*COUNTER.lock() >= 1);
    }

    #[test]
    fn a_panicked_holder_does_not_poison_the_mutex() {
        let lock = Arc::new(Mutex::new(vec![1]));
        let held = Arc::clone(&lock);
        let joined = std::thread::spawn(move || {
            let mut guard = held.lock();
            guard.push(2);
            panic!("holder panics with the guard held");
        })
        .join();
        assert!(joined.is_err());
        assert_eq!(*lock.lock(), vec![1, 2]);
        assert!(lock.try_lock().is_some());
        let lock = Arc::try_unwrap(lock).expect("sole owner");
        assert_eq!(lock.into_inner(), vec![1, 2]);
    }

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
        assert_eq!(lock.try_read().as_deref().map(String::as_str), Some("abc"));
        assert!(lock.try_write().is_some());
    }

    #[test]
    fn try_variants_report_a_held_lock() {
        let mutex = Mutex::new(());
        let guard = mutex.lock();
        assert!(mutex.try_lock().is_none());
        drop(guard);
        let rw = RwLock::new(());
        let reader = rw.read();
        assert!(rw.try_read().is_some());
        assert!(rw.try_write().is_none());
        drop(reader);
        let writer = rw.write();
        assert!(rw.try_read().is_none());
        drop(writer);
    }
}
