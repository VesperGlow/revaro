//! Decompression and render budgets.
//!
//! Both budgets exist for the same reason: an EPUB is attacker-controlled, and
//! "read the entry and see how big it is" is already too late if the entry is a
//! zip bomb. The byte counters are therefore checked while reading, and the Go
//! error semantics are preserved exactly — a failed [`Budget::take`] leaves the
//! counter untouched, and a take of exactly the remaining budget succeeds.

use crate::{MAX_DECOMPRESSED_TOTAL, ReaderError};

/// Cumulative decompressed-byte counter for one archive.
///
/// The same budget is shared by the container, OPF, TOC, cover and every spine
/// entry, so the cap is on the whole archive rather than per entry.
#[derive(Debug, Default)]
pub(crate) struct Budget {
    used: i64,
}

impl Budget {
    /// Reserve `amount` more decompressed bytes.
    ///
    /// Returns [`ReaderError::DecompressedTooLarge`] and leaves `used`
    /// unchanged when the total would exceed [`MAX_DECOMPRESSED_TOTAL`].
    pub(crate) fn take(&mut self, amount: i64) -> Result<(), ReaderError> {
        if amount > MAX_DECOMPRESSED_TOTAL - self.used {
            return Err(ReaderError::DecompressedTooLarge(
                MAX_DECOMPRESSED_TOTAL >> 20,
            ));
        }
        self.used += amount;
        Ok(())
    }
}

/// Whole-book budget for the cleaned chapter HTML.
///
/// `used` deliberately survives a chapter reset, matching Go: the cap applies
/// to the concatenation of all chapters, not to each one.
#[derive(Debug)]
pub(crate) struct RenderBudget {
    pub(crate) used: usize,
    pub(crate) max: usize,
    pub(crate) overflow: bool,
}

impl RenderBudget {
    /// A budget that may hold `max` rendered bytes.
    pub(crate) const fn new(max: usize) -> Self {
        Self {
            used: 0,
            max,
            overflow: false,
        }
    }

    /// Append `value` to `out` if it fits.
    ///
    /// Once the budget overflows every later write is a no-op and the overflow
    /// flag stays set, so the caller checks the flag once per chapter instead
    /// of threading an error through the recursive walk.
    pub(crate) fn write(&mut self, out: &mut String, value: &str) {
        if self.overflow || value.len() > self.max - self.used {
            self.overflow = true;
            return;
        }
        out.push_str(value);
        self.used += value.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_enforces_the_total_limit() {
        let mut budget = Budget::default();
        budget.take(MAX_DECOMPRESSED_TOTAL).unwrap();
        assert!(budget.take(1).is_err());
        // A failed take must not advance the counter, so a zero take still fits.
        budget.take(0).unwrap();
    }

    #[test]
    fn render_budget_stops_writing_at_the_cap() {
        let mut budget = RenderBudget::new(5);
        let mut out = String::new();
        budget.write(&mut out, "abc");
        budget.write(&mut out, "def");
        assert!(budget.overflow);
        // Nothing after the overflow is appended.
        budget.write(&mut out, "x");
        assert_eq!(out, "abc");
    }

    #[test]
    fn render_budget_usage_survives_a_reset() {
        let mut budget = RenderBudget::new(4);
        let mut out = String::new();
        budget.write(&mut out, "abcd");
        out.clear();
        budget.write(&mut out, "e");
        assert!(budget.overflow);
        assert!(out.is_empty());
    }
}
