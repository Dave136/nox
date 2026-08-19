//! Hybrid Logical Clock state and deterministic journal ordering.

use crate::ids::DeviceId;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Five minutes, the default future-skew quarantine window for remote changes.
pub const DEFAULT_MAX_FUTURE_SKEW_MS: u64 = 5 * 60 * 1_000;

/// Alias used by callers that refer to the limit without its direction.
pub const DEFAULT_MAX_SKEW_MS: u64 = DEFAULT_MAX_FUTURE_SKEW_MS;

/// A Hybrid Logical Clock timestamp.
#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
pub struct Hlc {
    /// Wall-clock milliseconds since the Unix epoch.
    pub physical_ms: u64,
    /// Logical counter for events sharing a physical timestamp.
    pub logical: u32,
}

impl Hlc {
    /// Construct an HLC timestamp from its persisted components.
    #[must_use]
    pub const fn new(physical_ms: u64, logical: u32) -> Self {
        Self {
            physical_ms,
            logical,
        }
    }
}

/// Backwards-compatible descriptive name for an HLC timestamp.
pub type HlcTimestamp = Hlc;

/// The complete deterministic key used to choose a displayed revision.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct HlcOrderKey {
    /// The revision's HLC timestamp.
    pub hlc: Hlc,
    /// The author device, used to break HLC ties.
    pub origin_device_id: DeviceId,
    /// The author's per-vault sequence, used as the final tie-breaker.
    pub origin_seq: u64,
}

impl HlcOrderKey {
    /// Construct the ordering key `(physical_ms, logical, device, origin_seq)`.
    #[must_use]
    pub const fn new(hlc: Hlc, origin_device_id: DeviceId, origin_seq: u64) -> Self {
        Self {
            hlc,
            origin_device_id,
            origin_seq,
        }
    }
}

/// Alias emphasizing that the key orders changes, rather than clocks.
pub type ChangeOrderKey = HlcOrderKey;

/// Short alias for the deterministic revision ordering key.
pub type OrderingKey = HlcOrderKey;

/// Alias for code that spells out the HLC portion of the ordering key.
pub type HlcOrderingKey = HlcOrderKey;

/// A local event's timestamp and allocated origin sequence.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, PartialOrd, Ord, Serialize)]
pub struct ClockStamp {
    /// HLC assigned to the event.
    pub hlc: Hlc,
    /// Persistent per-device, per-vault sequence assigned to the event.
    pub origin_seq: u64,
}

/// The result of checking a remote timestamp against the local clock.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SkewClassification {
    /// The remote timestamp is within the configured future-skew window.
    Accept,
    /// The remote timestamp is too far in the future and must be quarantined.
    Quarantine,
}

/// Alias for callers that treat the result as a decision.
pub type SkewDecision = SkewClassification;

pub use SkewClassification::{Accept, Quarantine};

/// Check future skew without mutating clock state or doing any I/O.
#[must_use]
pub fn classify(remote_hlc: Hlc, local_hlc: Hlc, max_skew_ms: u64) -> SkewClassification {
    if remote_hlc.physical_ms.saturating_sub(local_hlc.physical_ms) <= max_skew_ms {
        SkewClassification::Accept
    } else {
        SkewClassification::Quarantine
    }
}

/// Errors returned while advancing or observing an HLC.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HlcError {
    /// A remote timestamp is beyond the configured future-skew window.
    FutureSkew {
        /// Remote physical time.
        remote_ms: u64,
        /// Local wall-clock time used for classification.
        local_ms: u64,
        /// Configured maximum tolerated skew.
        limit_ms: u64,
    },
    /// The logical component cannot be incremented without wrapping.
    LogicalOverflow,
    /// The persistent per-vault sequence cannot be incremented without wrapping.
    OriginSequenceOverflow,
}

impl fmt::Display for HlcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FutureSkew {
                remote_ms,
                local_ms,
                limit_ms,
            } => write!(
                f,
                "remote HLC physical time {remote_ms} exceeds local time {local_ms} by more than {limit_ms} ms"
            ),
            Self::LogicalOverflow => f.write_str("HLC logical counter overflow"),
            Self::OriginSequenceOverflow => f.write_str("origin sequence counter overflow"),
        }
    }
}

impl std::error::Error for HlcError {}

/// Persistent local HLC and origin-sequence state.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct HlcClock {
    current: Hlc,
    origin_seq: u64,
}

impl HlcClock {
    /// Start a clock at zero with no allocated origin sequence.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            current: Hlc::new(0, 0),
            origin_seq: 0,
        }
    }

    /// Restore a clock from persisted HLC and sequence state.
    #[must_use]
    pub const fn from_state(current: Hlc, origin_seq: u64) -> Self {
        Self {
            current,
            origin_seq,
        }
    }

    /// Restore a clock with a zero origin sequence.
    #[must_use]
    pub const fn from_timestamp(current: Hlc) -> Self {
        Self::from_state(current, 0)
    }

    /// Return the last timestamp emitted or observed by this clock.
    #[must_use]
    pub const fn current(&self) -> Hlc {
        self.current
    }

    /// Return the last allocated per-vault origin sequence.
    #[must_use]
    pub const fn origin_seq(&self) -> u64 {
        self.origin_seq
    }

    /// Return both persisted counters.
    #[must_use]
    pub const fn state(&self) -> (Hlc, u64) {
        (self.current, self.origin_seq)
    }

    /// Allocate the next local sequence and advance the HLC.
    pub fn next(&mut self, wall_clock_ms: u64) -> Result<ClockStamp, HlcError> {
        let next_origin_seq = self
            .origin_seq
            .checked_add(1)
            .ok_or(HlcError::OriginSequenceOverflow)?;
        let next_hlc = self.next_local_hlc(wall_clock_ms)?;
        self.origin_seq = next_origin_seq;
        self.current = next_hlc;
        Ok(ClockStamp {
            hlc: next_hlc,
            origin_seq: next_origin_seq,
        })
    }

    /// Advance the HLC for a local event and return its timestamp.
    pub fn tick(&mut self, wall_clock_ms: u64) -> Result<Hlc, HlcError> {
        Ok(self.next(wall_clock_ms)?.hlc)
    }

    /// Alias for [`Self::tick`] that makes local-event allocation explicit.
    pub fn advance(&mut self, wall_clock_ms: u64) -> Result<Hlc, HlcError> {
        self.tick(wall_clock_ms)
    }

    /// Merge an accepted remote HLC after checking its future skew.
    pub fn observe(
        &mut self,
        remote_hlc: Hlc,
        wall_clock_ms: u64,
        max_skew_ms: u64,
    ) -> Result<Hlc, HlcError> {
        if classify(remote_hlc, Hlc::new(wall_clock_ms, 0), max_skew_ms)
            == SkewClassification::Quarantine
        {
            return Err(HlcError::FutureSkew {
                remote_ms: remote_hlc.physical_ms,
                local_ms: wall_clock_ms,
                limit_ms: max_skew_ms,
            });
        }

        let physical_ms = self
            .current
            .physical_ms
            .max(remote_hlc.physical_ms)
            .max(wall_clock_ms);
        let logical = match (
            physical_ms == self.current.physical_ms,
            physical_ms == remote_hlc.physical_ms,
        ) {
            (true, true) => self.current.logical.max(remote_hlc.logical).checked_add(1),
            (true, false) => self.current.logical.checked_add(1),
            (false, true) => remote_hlc.logical.checked_add(1),
            (false, false) => Some(0),
        }
        .ok_or(HlcError::LogicalOverflow)?;

        self.current = Hlc::new(physical_ms, logical);
        Ok(self.current)
    }

    /// Merge a remote HLC using the five-minute default future-skew window.
    pub fn observe_default(
        &mut self,
        remote_hlc: Hlc,
        wall_clock_ms: u64,
    ) -> Result<Hlc, HlcError> {
        self.observe(remote_hlc, wall_clock_ms, DEFAULT_MAX_FUTURE_SKEW_MS)
    }

    fn next_local_hlc(&self, wall_clock_ms: u64) -> Result<Hlc, HlcError> {
        let physical_ms = self.current.physical_ms.max(wall_clock_ms);
        let logical = if physical_ms == self.current.physical_ms {
            self.current
                .logical
                .checked_add(1)
                .ok_or(HlcError::LogicalOverflow)?
        } else {
            0
        };
        Ok(Hlc::new(physical_ms, logical))
    }
}
