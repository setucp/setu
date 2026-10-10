//! Wall-clock seam for the runner and scheduler (tests pin it with
//! [`crate::testing::FixedClock`]).

use chrono::{DateTime, Utc};

/// The current time.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

/// The system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}
