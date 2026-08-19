//! Background actor runtime — P0 skeleton.
//!
//! The full runtime is ported in P2 from POE Alarm's `poe-alarm-runtime`
//! (`actor.rs`): a `std::sync::mpsc` actor with generations + leases so stale
//! recognition results can never write into the current book, latest-value
//! snapshot coalescing toward the UI, bounded native ownership, and a 1-second
//! shutdown contract. Only the generation primitive lives here for now so
//! earlier layers can already stamp work with the session that issued it.

pub mod analysis;
#[cfg(windows)]
pub mod live;
#[cfg(windows)]
pub mod pipeline;
pub mod report_text;
pub mod reports;

/// Monotonically increasing id for a runtime session. Work stamped with an old
/// generation is discarded on arrival; replacement invalidates the generation
/// *before* waiting on anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeGeneration(u64);

impl RuntimeGeneration {
    pub const FIRST: RuntimeGeneration = RuntimeGeneration(1);

    pub fn next(self) -> RuntimeGeneration {
        RuntimeGeneration(self.0.checked_add(1).expect("generation counter overflow"))
    }

    pub fn accepts(self, stamped: RuntimeGeneration) -> bool {
        self == stamped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_generations_are_rejected() {
        let current = RuntimeGeneration::FIRST;
        let replaced = current.next();
        assert!(current.accepts(current));
        assert!(!replaced.accepts(current));
        assert!(replaced > current);
    }
}
