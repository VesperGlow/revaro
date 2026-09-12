//! Login attempt admission control, ported from Go's `loginLimiter`.
//!
//! Three separate limits protect the login endpoint:
//!
//! * at most five **failures per client IP** in a fifteen-minute window,
//! * at most thirty failures **globally** in the same window,
//! * at most two **concurrent verifications**, because each one runs an Argon2id
//!   derivation over 64 MiB.
//!
//! The per-IP map is capped at 10 000 entries and expired entries are pruned
//! opportunistically, so a flood of forged client addresses cannot grow memory
//! without bound. The window clock is injectable so the tests can advance it
//! instead of sleeping for fifteen minutes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::service::Clock;
use revaro_core::Timestamp;

/// Failures allowed per client IP inside one window.
pub const MAX_ATTEMPTS_PER_IP: u32 = 5;
/// Failures allowed across all clients inside one window.
pub const MAX_GLOBAL_ATTEMPTS: u32 = 30;
/// Length of the sliding window, in milliseconds.
pub const WINDOW_MILLIS: i64 = 15 * 60 * 1000;
/// Hard cap on the number of tracked client addresses.
pub const MAX_ENTRIES: usize = 10_000;
/// `Retry-After` advertised when the caller is over a limit.
pub const RETRY_AFTER_BLOCKED: &str = "60";
/// `Retry-After` advertised when both verification slots are occupied.
pub const RETRY_AFTER_BUSY: &str = "2";

#[derive(Debug, Clone, Copy, Default)]
struct Attempt {
    count: u32,
    reset: Timestamp,
}

#[derive(Debug, Default)]
struct State {
    attempts: HashMap<String, Attempt>,
    global: Attempt,
}

/// The process-wide login limiter.
///
/// Cloning shares the counters: an [`super::AuthService`] clone must not get a
/// fresh budget, so the mutable state sits behind an `Arc`.
#[derive(Debug, Clone)]
pub struct LoginLimiter {
    clock: Clock,
    state: Arc<Mutex<State>>,
    slots: Arc<Semaphore>,
}

impl LoginLimiter {
    /// Build a limiter using the system clock.
    #[must_use]
    pub fn new() -> Self {
        Self::with_clock(Clock::system())
    }

    /// Build a limiter with an injected clock. Tests use this to advance time.
    #[must_use]
    pub fn with_clock(clock: Clock) -> Self {
        Self {
            clock,
            state: Arc::new(Mutex::new(State::default())),
            slots: Arc::new(Semaphore::new(super::service::MAX_LOGIN_CONCURRENCY)),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// True when the caller may attempt a login.
    ///
    /// A blocked caller gets `429` with [`RETRY_AFTER_BLOCKED`].
    pub fn allow(&self, ip: &str) -> bool {
        let now = self.clock.now();
        let mut state = self.lock();
        if state.attempts.len() >= MAX_ENTRIES {
            // `retain` keeps what the predicate accepts, so the predicate must
            // be the *negation* of "expired". Inverting this would throw away
            // live rate-limit state and keep stale entries, letting an attacker
            // clear their own budget by filling the map.
            state.attempts.retain(|_, attempt| now <= attempt.reset);
        }
        if now > state.global.reset {
            state.global = Attempt::default();
        }
        if state.global.count >= MAX_GLOBAL_ATTEMPTS {
            return false;
        }
        match state.attempts.get(ip).copied() {
            None => true,
            Some(attempt) if now > attempt.reset => {
                state.attempts.remove(ip);
                true
            }
            Some(attempt) => attempt.count < MAX_ATTEMPTS_PER_IP,
        }
    }

    /// Record a failed verification against both the IP and the global window.
    pub fn fail(&self, ip: &str) {
        let now = self.clock.now();
        let reset = Timestamp::from_unix_millis(now.unix_millis() + WINDOW_MILLIS);
        let mut state = self.lock();
        let mut attempt = state.attempts.get(ip).copied().unwrap_or_default();
        if now > attempt.reset {
            attempt = Attempt { count: 0, reset };
        }
        attempt.count += 1;
        if state.attempts.contains_key(ip) || state.attempts.len() < MAX_ENTRIES {
            state.attempts.insert(ip.to_owned(), attempt);
        }
        if now > state.global.reset {
            state.global = Attempt { count: 0, reset };
        }
        state.global.count += 1;
    }

    /// Clear the per-IP window after a successful login.
    pub fn success(&self, ip: &str) {
        self.lock().attempts.remove(ip);
    }

    /// Take one of the two verification slots, or `None` when both are busy.
    ///
    /// A busy caller gets `429` with [`RETRY_AFTER_BUSY`]; unlike [`allow`], this
    /// is a transient condition and is not counted as a failure.
    #[must_use]
    pub fn acquire(&self) -> Option<OwnedSemaphorePermit> {
        Arc::clone(&self.slots).try_acquire_owned().ok()
    }
}

impl Default for LoginLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};

    /// A monotonic fake clock shared with the limiter.
    fn test_clock() -> (Arc<AtomicI64>, Clock) {
        let millis = Arc::new(AtomicI64::new(1_714_982_889_000));
        let handle = Arc::clone(&millis);
        let clock =
            Clock::from_fn(move || Timestamp::from_unix_millis(handle.load(Ordering::SeqCst)));
        (millis, clock)
    }

    fn advance(millis: &AtomicI64, delta: i64) {
        millis.fetch_add(delta, Ordering::SeqCst);
    }

    #[test]
    fn five_failures_block_only_that_address() {
        let (_millis, clock) = test_clock();
        let limiter = LoginLimiter::with_clock(clock);
        for attempt in 0..MAX_ATTEMPTS_PER_IP {
            assert!(
                limiter.allow("10.0.0.1"),
                "attempt {attempt} must be allowed"
            );
            limiter.fail("10.0.0.1");
        }
        assert!(
            !limiter.allow("10.0.0.1"),
            "the sixth attempt must be blocked"
        );
        // Another address is unaffected while the global budget lasts.
        assert!(limiter.allow("10.0.0.2"));
    }

    #[test]
    fn the_window_expires() {
        let (millis, clock) = test_clock();
        let limiter = LoginLimiter::with_clock(clock);
        for _ in 0..MAX_ATTEMPTS_PER_IP {
            limiter.fail("10.0.0.1");
        }
        assert!(!limiter.allow("10.0.0.1"));
        advance(&millis, WINDOW_MILLIS + 1);
        assert!(limiter.allow("10.0.0.1"), "the window must roll over");
    }

    #[test]
    fn a_successful_login_clears_the_address() {
        let (_millis, clock) = test_clock();
        let limiter = LoginLimiter::with_clock(clock);
        for _ in 0..MAX_ATTEMPTS_PER_IP - 1 {
            limiter.fail("10.0.0.1");
        }
        limiter.success("10.0.0.1");
        for _ in 0..MAX_ATTEMPTS_PER_IP {
            assert!(limiter.allow("10.0.0.1"));
            limiter.fail("10.0.0.1");
        }
        assert!(!limiter.allow("10.0.0.1"));
    }

    #[test]
    fn the_global_window_blocks_every_address() {
        let (millis, clock) = test_clock();
        let limiter = LoginLimiter::with_clock(clock);
        for index in 0..MAX_GLOBAL_ATTEMPTS {
            limiter.fail(&format!("10.0.{}.{}", index / 256, index % 256));
        }
        assert!(
            !limiter.allow("192.0.2.1"),
            "the global budget must block a fresh address"
        );
        advance(&millis, WINDOW_MILLIS + 1);
        assert!(limiter.allow("192.0.2.1"));
    }

    #[test]
    fn the_address_map_is_capped_and_pruned() {
        let (millis, clock) = test_clock();
        let limiter = LoginLimiter::with_clock(clock);
        for index in 0..MAX_ENTRIES {
            limiter.fail(&format!("ip-{index}"));
        }
        // A new address is rejected outright once the map is full, and does not
        // grow it further.
        limiter.fail("overflow");
        assert_eq!(limiter.lock().attempts.len(), MAX_ENTRIES);
        // Once entries expire they are pruned opportunistically.
        advance(&millis, WINDOW_MILLIS + 1);
        assert!(limiter.allow("overflow"));
        assert!(limiter.lock().attempts.len() < MAX_ENTRIES);
    }

    #[test]
    fn only_two_verifications_run_at_once() {
        let limiter = LoginLimiter::new();
        let first = limiter.acquire().expect("first slot");
        let second = limiter.acquire().expect("second slot");
        assert!(limiter.acquire().is_none(), "a third must be refused");
        drop(first);
        assert!(limiter.acquire().is_some(), "a freed slot is reusable");
        drop(second);
    }
}
