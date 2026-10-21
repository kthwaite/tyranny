// Task 10 routes this module and removes the attribute.
#![allow(dead_code)]

mod http;
mod journal;
mod musicbrainz;
mod safe_write;
pub mod tags;
#[cfg(test)]
mod test_support;

use crate::cancel::{Cancellation, Stop};
use std::{fmt, time::Instant};

/// Why a provider lookup stopped. `Display` is the user-visible reason.
#[derive(Debug)]
pub(super) enum LookupError {
    Busy(&'static str),
    Cancelled,
    TimedOut,
    Failed(String),
}
impl fmt::Display for LookupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy(service) => write!(f, "{service} is busy, try again shortly"),
            Self::Cancelled => f.write_str("lookup cancelled"),
            Self::TimedOut => f.write_str("lookup took too long"),
            Self::Failed(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for LookupError {}

impl Cancellation {
    pub(super) fn check_lookup(&self, deadline: Instant) -> Result<(), LookupError> {
        match self.stopped(deadline) {
            Some(Stop::Cancelled) => Err(LookupError::Cancelled),
            Some(Stop::Expired) => Err(LookupError::TimedOut),
            None => Ok(()),
        }
    }
}
