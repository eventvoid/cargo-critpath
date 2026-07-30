//! Whole-millisecond durations.

use core::fmt;
use core::num::NonZeroU64;

/// A duration in whole milliseconds.
///
/// Weights are integers rather than floating-point seconds so that ordering is total, sums are
/// exact, and identical input produces identical output on every platform. Cargo reports two
/// decimal places of seconds, so milliseconds lose nothing that was ever measured.
///
/// Arithmetic saturates rather than overflowing, so a graph with absurd weights produces an
/// absurd answer instead of a panic or a wrapped one.
///
/// ```
/// use critpath_core::Millis;
///
/// let a = Millis::new(1_500);
/// let b = Millis::new(250);
/// assert_eq!(a.saturating_add(b), Millis::new(1_750));
/// assert_eq!(b.saturating_sub(a), Millis::ZERO);
/// assert_eq!(a.to_string(), "1500ms");
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Millis(u64);

impl Millis {
    /// No time at all.
    pub const ZERO: Self = Self(0);

    /// A duration of `ms` milliseconds.
    #[must_use]
    pub const fn new(ms: u64) -> Self {
        Self(ms)
    }

    /// The duration as a count of milliseconds.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// The sum, clamped to the maximum representable duration.
    #[must_use]
    pub const fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }

    /// The difference, clamped at zero. There are no negative durations.
    #[must_use]
    pub const fn saturating_sub(self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }

    /// The larger of two durations.
    #[must_use]
    pub const fn max(self, other: Self) -> Self {
        if self.0 >= other.0 {
            self
        } else {
            other
        }
    }

    /// This duration divided by `divisor`, rounded up.
    ///
    /// Rounding up matters because this computes the area bound `total_work / J`, which is a lower
    /// bound on the makespan. Rounding down would report a bound that no schedule can achieve.
    #[must_use]
    pub fn div_ceil_by(self, divisor: NonZeroU64) -> Self {
        Self(self.0.div_ceil(divisor.get()))
    }

    /// This duration scaled by the fraction `numerator / denominator`, rounded to nearest.
    ///
    /// Used to divide a unit's weight between the halves of a hypothetical split. Widening to 128
    /// bits keeps the product exact for any duration a real build could produce.
    #[must_use]
    pub fn scaled(self, numerator: u64, denominator: NonZeroU64) -> Self {
        let d = denominator.get() as u128;
        let product = self.0 as u128 * numerator as u128;
        // Deliberate integer division; rounding to nearest is done by adding half the divisor.
        #[allow(clippy::integer_division)]
        let q = (product + d / 2) / d;
        // Saturate rather than truncate if a caller passes a numerator larger than the
        // denominator and the result leaves the range.
        if q > u64::MAX as u128 {
            Self(u64::MAX)
        } else {
            Self(q as u64)
        }
    }
}

impl fmt::Display for Millis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}ms", self.0)
    }
}

impl core::iter::Sum for Millis {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::ZERO, Self::saturating_add)
    }
}
