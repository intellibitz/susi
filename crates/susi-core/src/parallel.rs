//! Std-only data parallelism for the swarm: scoped worker threads over a
//! shared queue, results returned in input order.
//!
//! Replaces the `rayon` edges the GAWD crates carried until 2026-11-07
//! (lean-core program, `docs/lean-core.md`): a `par_iter().map().collect()`
//! over a few hundred agents, goals, peers or files needs no work-stealing
//! pool, only as many threads as the host has cores and a queue. Panics in
//! `f` propagate to the caller when the scope joins, as they did on rayon's
//! pool.
use std::collections::VecDeque;
use std::sync::Mutex;

/// Worker threads a parallel map uses at most: the host's logical cores.
pub fn threads() -> usize {
    std::thread::available_parallelism().map_or(1, usize::from)
}

/// Apply `f` to every item on up to [`threads`] worker threads and return
/// the results in the items' order. One item or one core runs inline.
pub fn map<T, R, F>(items: Vec<T>, f: F) -> Vec<R>
where
    T: Send,
    R: Send,
    F: Fn(T) -> R + Sync,
{
    let count = items.len();
    let workers = threads().min(count);
    if workers <= 1 {
        return items.into_iter().map(f).collect();
    }
    let queue: Mutex<VecDeque<(usize, T)>> = Mutex::new(items.into_iter().enumerate().collect());
    let results: Mutex<Vec<(usize, R)>> = Mutex::new(Vec::with_capacity(count));
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let next = queue.lock().unwrap_or_else(|e| e.into_inner()).pop_front();
                let Some((index, item)) = next else {
                    break;
                };
                let result = f(item);
                results
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push((index, result));
            });
        }
    });
    let mut results = results.into_inner().unwrap_or_else(|e| e.into_inner());
    results.sort_by_key(|(index, _)| *index);
    results.into_iter().map(|(_, result)| result).collect()
}

/// [`map`] over borrowed items.
pub fn map_ref<'a, T, R, F>(items: &'a [T], f: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&'a T) -> R + Sync,
{
    map(items.iter().collect(), f)
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn threads_is_at_least_one() {
        assert!(threads() >= 1);
    }

    #[test]
    fn map_keeps_input_order_and_visits_every_item_once() {
        let visits = AtomicUsize::new(0);
        let out = map((0..1000).collect(), |i: usize| {
            visits.fetch_add(1, Ordering::SeqCst);
            i * 2
        });
        assert_eq!(out, (0..1000).map(|i| i * 2).collect::<Vec<_>>());
        assert_eq!(visits.load(Ordering::SeqCst), 1000);
    }

    #[test]
    fn map_handles_empty_and_single_inputs_inline() {
        let none: Vec<usize> = map(Vec::new(), |i: usize| i);
        assert!(none.is_empty());
        assert_eq!(map(vec![7], |i: usize| i + 1), vec![8]);
    }

    #[test]
    fn map_ref_borrows_without_cloning() {
        let names = vec!["a".to_string(), "bb".to_string(), "ccc".to_string()];
        assert_eq!(map_ref(&names, |n| n.len()), vec![1, 2, 3]);
    }

    #[test]
    fn map_runs_items_concurrently_when_cores_allow() {
        if threads() < 2 {
            return;
        }
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let _ = map((0..2).collect(), |_: usize| {
            let now = active.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while peak.load(Ordering::SeqCst) < 2 && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            active.fetch_sub(1, Ordering::SeqCst);
        });
        assert_eq!(peak.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_panicking_item_propagates_to_the_caller() {
        let result = std::panic::catch_unwind(|| {
            map((0..8).collect(), |i: usize| {
                assert_ne!(i, 5, "item 5 fails");
                i
            })
        });
        assert!(result.is_err());
    }
}
