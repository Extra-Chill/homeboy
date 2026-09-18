//! Shared reaping contract for durable registries.
//!
//! Each registry answers one question per record: is the subject still real,
//! and does any live process claim it? A record that fails both is residue.
//! Residue is retired on a normal cadence and must not be counted as live.

/// Liveness evidence for one durable registry record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordLiveness {
    /// The subject this record describes still exists.
    pub subject_exists: bool,
    /// A live process currently claims this record.
    pub claimed: bool,
}

impl RecordLiveness {
    pub const fn new(subject_exists: bool, claimed: bool) -> Self {
        Self {
            subject_exists,
            claimed,
        }
    }

    /// The subject exists or a live process claims it.
    pub const fn live(self) -> bool {
        self.subject_exists || self.claimed
    }

    /// The subject is gone and no live process claims it.
    pub const fn residue(self) -> bool {
        !self.live()
    }
}

/// Keep records that still exist or are still claimed.
pub fn live_only<T>(
    records: impl IntoIterator<Item = T>,
    liveness: impl Fn(&T) -> RecordLiveness,
) -> Vec<T> {
    records
        .into_iter()
        .filter(|record| liveness(record).live())
        .collect()
}

/// Keep records that fail both liveness tests.
pub fn residue_only<T>(
    records: impl IntoIterator<Item = T>,
    liveness: impl Fn(&T) -> RecordLiveness,
) -> Vec<T> {
    records
        .into_iter()
        .filter(|record| liveness(record).residue())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_is_residue_only_when_gone_and_unclaimed() {
        assert!(RecordLiveness::new(true, true).live());
        assert!(RecordLiveness::new(true, false).live());
        assert!(RecordLiveness::new(false, true).live());
        assert!(RecordLiveness::new(false, false).residue());
        assert!(!RecordLiveness::new(false, false).live());
    }

    #[test]
    fn live_only_excludes_residue() {
        let records = [1, 2, 3];
        let kept = live_only(records, |record| RecordLiveness::new(*record != 2, false));
        assert_eq!(kept, [1, 3]);
    }
}
