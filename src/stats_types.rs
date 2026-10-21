//! Wire types for listening statistics.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PeriodKind {
    Day,
    Week,
    Month,
    Year,
    All,
}

/// A period kind plus a local anchor date (`YYYY-MM-DD`).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct PeriodRequest {
    pub kind: PeriodKind,
    pub anchor: String,
}
