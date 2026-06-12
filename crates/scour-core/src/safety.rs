//! The safety interlock.
//!
//! Scour is built so that *no code path can start a wipe unattended*:
//!
//! 1. There are no CLI flags that select disks or start jobs.
//! 2. [`crate::engine::spawn_wipe`] requires an [`ArmToken`] argument.
//! 3. The only constructor of `ArmToken` lives in this module, behind the
//!    [`SafetyGate`] state machine, which demands:
//!      * at least one explicitly selected, unlocked disk,
//!      * a dynamically generated confirmation phrase typed exactly
//!        (e.g. `ERASE 2 DISKS` — it changes with the selection, so it can
//!        never become muscle memory),
//!      * a five-second countdown during which any key aborts.
//!
//! Locked disks (mounted, swap, boot medium, held by RAID/dm) are rejected at
//! selection time *and* re-rejected by the engine.

use crate::CoreError;

pub const COUNTDOWN_MS: u64 = 5_000;

/// Proof that the operator completed the arming ceremony. Cannot be
/// constructed outside this module; required by the engine to start any job.
pub struct ArmToken {
    _private: (),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArmState {
    /// Nothing armed; the gate idles here.
    Disarmed,
    /// Operator is typing the confirmation phrase.
    Typing { typed: String },
    /// Phrase accepted; final abortable countdown is running.
    Countdown { remaining_ms: u64 },
    /// Token released. Terminal state — a gate can only fire once.
    Released,
}

pub struct SafetyGate {
    state: ArmState,
    phrase: String,
    disk_count: usize,
}

impl SafetyGate {
    /// Build a gate for a confirmed selection. Fails when nothing is selected.
    pub fn new(disk_count: usize) -> Result<Self, CoreError> {
        if disk_count == 0 {
            return Err(CoreError::NothingSelected);
        }
        let phrase = if disk_count == 1 {
            "ERASE 1 DISK".to_string()
        } else {
            format!("ERASE {disk_count} DISKS")
        };
        Ok(SafetyGate {
            state: ArmState::Typing {
                typed: String::new(),
            },
            phrase,
            disk_count,
        })
    }

    pub fn state(&self) -> &ArmState {
        &self.state
    }

    pub fn disk_count(&self) -> usize {
        self.disk_count
    }

    /// The exact phrase the operator must type.
    pub fn phrase(&self) -> &str {
        &self.phrase
    }

    pub fn typed(&self) -> &str {
        match &self.state {
            ArmState::Typing { typed } => typed,
            _ => "",
        }
    }

    /// Append a typed character. Only printable ASCII is accepted; anything
    /// else is ignored so stray escape sequences can't pollute the input.
    pub fn input_char(&mut self, c: char) {
        if let ArmState::Typing { typed } = &mut self.state {
            if c.is_ascii_graphic() || c == ' ' {
                // Uppercase as the user types: deliberate friction is in the
                // phrase content, not the shift key.
                typed.push(c.to_ascii_uppercase());
            }
        }
    }

    pub fn backspace(&mut self) {
        if let ArmState::Typing { typed } = &mut self.state {
            typed.pop();
        }
    }

    pub fn phrase_matches(&self) -> bool {
        matches!(&self.state, ArmState::Typing { typed } if typed == &self.phrase)
    }

    /// Move from typing to the countdown. Rejects a wrong phrase.
    pub fn confirm(&mut self) -> Result<(), CoreError> {
        match &self.state {
            ArmState::Typing { typed } if typed == &self.phrase => {
                self.state = ArmState::Countdown {
                    remaining_ms: COUNTDOWN_MS,
                };
                Ok(())
            }
            ArmState::Typing { .. } => Err(CoreError::PhraseMismatch),
            _ => Err(CoreError::InvalidState),
        }
    }

    /// Advance the countdown; returns the token exactly once when it expires.
    pub fn tick(&mut self, elapsed_ms: u64) -> Option<ArmToken> {
        if let ArmState::Countdown { remaining_ms } = &mut self.state {
            *remaining_ms = remaining_ms.saturating_sub(elapsed_ms);
            if *remaining_ms == 0 {
                self.state = ArmState::Released;
                return Some(ArmToken { _private: () });
            }
        }
        None
    }

    pub fn countdown_remaining_ms(&self) -> Option<u64> {
        match self.state {
            ArmState::Countdown { remaining_ms } => Some(remaining_ms),
            _ => None,
        }
    }

    /// Abort and disarm. Valid from any non-released state.
    pub fn abort(&mut self) {
        if self.state != ArmState::Released {
            self.state = ArmState::Disarmed;
        }
    }

    pub fn is_aborted(&self) -> bool {
        self.state == ArmState::Disarmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn type_str(gate: &mut SafetyGate, s: &str) {
        for c in s.chars() {
            gate.input_char(c);
        }
    }

    #[test]
    fn zero_disks_cannot_arm() {
        assert!(SafetyGate::new(0).is_err());
    }

    #[test]
    fn phrase_depends_on_selection() {
        assert_eq!(SafetyGate::new(1).unwrap().phrase(), "ERASE 1 DISK");
        assert_eq!(SafetyGate::new(3).unwrap().phrase(), "ERASE 3 DISKS");
    }

    #[test]
    fn wrong_phrase_is_rejected() {
        let mut g = SafetyGate::new(2).unwrap();
        type_str(&mut g, "ERASE 2 DISK"); // missing S
        assert!(!g.phrase_matches());
        assert!(g.confirm().is_err());
        // Still typing; the gate did not advance.
        assert!(matches!(g.state(), ArmState::Typing { .. }));
    }

    #[test]
    fn lowercase_input_is_normalized() {
        let mut g = SafetyGate::new(1).unwrap();
        type_str(&mut g, "erase 1 disk");
        assert!(g.phrase_matches());
    }

    #[test]
    fn control_chars_are_ignored() {
        let mut g = SafetyGate::new(1).unwrap();
        g.input_char('\x1b');
        g.input_char('\n');
        g.input_char('\t');
        assert_eq!(g.typed(), "");
    }

    #[test]
    fn backspace_edits() {
        let mut g = SafetyGate::new(1).unwrap();
        type_str(&mut g, "ERASE 1 DISKX");
        g.backspace();
        assert!(g.phrase_matches());
    }

    #[test]
    fn full_ceremony_releases_exactly_one_token() {
        let mut g = SafetyGate::new(1).unwrap();
        type_str(&mut g, "ERASE 1 DISK");
        g.confirm().unwrap();
        assert!(g.tick(2_000).is_none());
        assert_eq!(g.countdown_remaining_ms(), Some(3_000));
        let token = g.tick(3_000);
        assert!(token.is_some());
        // Terminal: no second token, no re-confirm.
        assert!(g.tick(10_000).is_none());
        assert!(g.confirm().is_err());
    }

    #[test]
    fn abort_during_countdown_disarms() {
        let mut g = SafetyGate::new(1).unwrap();
        type_str(&mut g, "ERASE 1 DISK");
        g.confirm().unwrap();
        g.tick(1_000);
        g.abort();
        assert!(g.is_aborted());
        assert!(g.tick(10_000).is_none(), "aborted gate must never release");
        assert!(g.confirm().is_err());
    }

    #[test]
    fn cannot_confirm_without_typing() {
        let mut g = SafetyGate::new(1).unwrap();
        assert!(g.confirm().is_err());
    }
}
