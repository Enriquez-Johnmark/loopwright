//! The only place that reads the system clock. Everything else asks a
//! [`Clock`], so tests can control time.

use std::sync::{Mutex, PoisonError};

use time::{Duration, OffsetDateTime};

/// A source of the current time.
pub trait Clock: Send + Sync {
    /// The current time in UTC.
    fn now(&self) -> OffsetDateTime;
}

/// The real clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    #[expect(clippy::disallowed_methods, reason = "this is the one sanctioned system-clock read")]
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

/// A clock that stays where it is put; for tests and demos.
#[derive(Debug)]
pub struct FixedClock {
    now: Mutex<OffsetDateTime>,
}

impl FixedClock {
    /// A clock frozen at `now`.
    pub fn new(now: OffsetDateTime) -> Self {
        Self { now: Mutex::new(now) }
    }

    /// The time `by` from now; handy for scheduling in tests.
    pub fn now_plus(&self, by: Duration) -> OffsetDateTime {
        self.now() + by
    }

    /// Moves the clock forward by `by`.
    pub fn advance(&self, by: Duration) {
        let mut now = self.now.lock().unwrap_or_else(PoisonError::into_inner);
        *now += by;
    }
}

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
