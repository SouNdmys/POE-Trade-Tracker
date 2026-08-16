//! Auto-watch loop primitives.
//!
//! The [`MonitorGate`] is a platform-free state machine deciding, per
//! captured fingerprint, whether to stay idle, wait for stability, or run
//! recognition — and, per recognized book, whether it is new content. The
//! Windows driver (`session` module) loops GDI capture through the gate and
//! the recognition route with a double-read confirmation: Windows OCR is
//! slightly jittery run-to-run, so a book is only accepted when two
//! back-to-back recognitions of the same stable frame agree on the content
//! signature. Skipping remains free; guessing remains banned.

use ptt_core::BookSignature;

#[cfg(windows)]
mod session;
#[cfg(windows)]
pub use session::{SessionConfig, SessionEvent, SessionStats, run_session, skip_key};

/// Decision for one captured tables-region fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateStep {
    /// Same content as the last handled frame — nothing to do.
    Idle,
    /// Content changed; capture again and require the same fingerprint
    /// before spending OCR on it (animation/scroll rejection).
    AwaitStability,
    /// Two consecutive captures agreed on a new fingerprint — recognize.
    Recognize,
}

/// Fingerprint/stability/dedupe state for one watch session.
#[derive(Debug, Default)]
pub struct MonitorGate {
    handled_fingerprint: Option<u64>,
    pending_fingerprint: Option<u64>,
    last_signature: Option<BookSignature>,
}

impl MonitorGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds the fingerprint of the freshly captured tables region.
    pub fn on_fingerprint(&mut self, fingerprint: u64) -> GateStep {
        if self.handled_fingerprint == Some(fingerprint) {
            self.pending_fingerprint = None;
            return GateStep::Idle;
        }
        match self.pending_fingerprint {
            Some(pending) if pending == fingerprint => GateStep::Recognize,
            _ => {
                self.pending_fingerprint = Some(fingerprint);
                GateStep::AwaitStability
            }
        }
    }

    /// Marks the pending fingerprint as handled (recognition ran, whatever
    /// the outcome) so the same frame is not re-OCR'd every tick.
    pub fn mark_handled(&mut self) {
        if let Some(pending) = self.pending_fingerprint.take() {
            self.handled_fingerprint = Some(pending);
        }
    }

    /// Content-level dedupe: true when this signature differs from the last
    /// emitted book (the same book re-rendered costs one hash compare).
    pub fn should_emit(&mut self, signature: BookSignature) -> bool {
        if self.last_signature == Some(signature) {
            return false;
        }
        self.last_signature = Some(signature);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_frames_stay_idle_after_first_handling() {
        let mut gate = MonitorGate::new();
        assert_eq!(gate.on_fingerprint(7), GateStep::AwaitStability);
        assert_eq!(gate.on_fingerprint(7), GateStep::Recognize);
        gate.mark_handled();
        assert_eq!(gate.on_fingerprint(7), GateStep::Idle);
        assert_eq!(gate.on_fingerprint(7), GateStep::Idle);
    }

    #[test]
    fn animation_frames_never_reach_recognition() {
        let mut gate = MonitorGate::new();
        assert_eq!(gate.on_fingerprint(1), GateStep::AwaitStability);
        assert_eq!(gate.on_fingerprint(2), GateStep::AwaitStability);
        assert_eq!(gate.on_fingerprint(3), GateStep::AwaitStability);
        assert_eq!(gate.on_fingerprint(3), GateStep::Recognize);
    }

    #[test]
    fn handled_frame_changing_back_requires_fresh_stability() {
        let mut gate = MonitorGate::new();
        gate.on_fingerprint(1);
        gate.on_fingerprint(1);
        gate.mark_handled();
        assert_eq!(gate.on_fingerprint(2), GateStep::AwaitStability);
        assert_eq!(gate.on_fingerprint(1), GateStep::Idle, "back to handled");
        assert_eq!(gate.on_fingerprint(2), GateStep::AwaitStability);
        assert_eq!(gate.on_fingerprint(2), GateStep::Recognize);
    }

    #[test]
    fn signature_dedupe_emits_only_new_content() {
        let mut gate = MonitorGate::new();
        assert!(gate.should_emit(BookSignature(10)));
        assert!(!gate.should_emit(BookSignature(10)));
        assert!(gate.should_emit(BookSignature(11)));
        assert!(gate.should_emit(BookSignature(10)), "A-B-A is new again");
    }
}
