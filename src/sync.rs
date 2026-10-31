//! Locks on the application's in-memory state, which keep their value when an
//! earlier holder panicked.
//!
//! Every value behind these locks is state that later calls overwrite or
//! rebuild: caches, per-conversation maps, the current agent, task handles,
//! the update check. A panic while one is held is a bug of its own; turning
//! it into a second panic in every later caller would close the session the
//! user is working in for a value the next update replaces. Durable state is
//! never behind them: it goes through revision-checked files and SQLite.

use std::sync::{Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::panic, reason = "The test poisons a lock on purpose")]
    use super::*;

    #[test]
    fn a_lock_keeps_its_value_after_a_holder_panicked() {
        let shared = std::sync::Arc::new(Mutex::new(1));
        let holder = shared.clone();
        let panicked = std::thread::spawn(move || {
            let _guard = lock(&holder);
            std::panic::panic_any("holder failed");
        })
        .join();
        assert!(panicked.is_err());
        assert!(shared.is_poisoned());
        *lock(&shared) += 1;
        assert_eq!(*lock(&shared), 2);
    }
}
