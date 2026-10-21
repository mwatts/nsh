//! Characters typed while focus is moving. A handled key or a press arms
//! a three-second wait. Plain characters in that wait go to the next
//! editable field. Escape, a press that focuses nothing, the window going
//! inactive, or the three seconds elapsing drops them.

use std::time::{Duration, Instant};

/// How long typed characters wait for a field.
pub const HOLD: Duration = Duration::from_secs(3);

/// The queue one surface keeps.
#[derive(Debug, Default)]
pub struct TextHold {
    pending: String,
    until: Option<Instant>,
}

impl TextHold {
    /// A handled key or press just ran. Characters typed until [`HOLD`]
    /// elapses wait for a field.
    pub fn arm(&mut self, now: Instant) {
        self.until = Some(now + HOLD);
    }

    /// Drops text that was waiting.
    pub fn disarm(&mut self) {
        self.pending.clear();
        self.until = None;
    }

    /// Whether characters typed at `now` wait. The wait ending drops them.
    pub fn armed(&mut self, now: Instant) -> bool {
        match self.until {
            Some(until) if now < until => true,
            Some(_) => {
                self.disarm();
                false
            }
            None => false,
        }
    }

    /// Queues the plain characters in `text` while the wait is armed and
    /// no editable field has the keyboard, or while focus is still moving.
    /// A field that already has the keyboard takes the character itself,
    /// and the wait ends. True when the characters were queued.
    pub fn offer(
        &mut self,
        now: Instant,
        text: &str,
        field_has_keyboard: bool,
        focus_moving: bool,
    ) -> bool {
        let plain: String = text.chars().filter(|c| !c.is_control()).collect();
        if plain.is_empty() {
            return false;
        }
        let waiting = self.armed(now);
        if field_has_keyboard && !waiting {
            return false;
        }
        if field_has_keyboard && !focus_moving {
            self.disarm();
            return false;
        }
        if !waiting {
            return false;
        }
        self.pending.push_str(&plain);
        true
    }

    /// The characters waiting at `now`, if an editable field is taking
    /// them. Empty once the wait has ended, and the wait ends.
    pub fn take(&mut self, now: Instant) -> String {
        if !self.armed(now) {
            return String::new();
        }
        self.until = None;
        std::mem::take(&mut self.pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn characters_wait_three_seconds_then_drop() {
        let start = Instant::now();
        let mut hold = TextHold::default();
        assert!(!hold.offer(start, "a", false, false));
        hold.arm(start);
        assert!(hold.offer(start, "ab", false, false));
        assert_eq!(hold.take(start + Duration::from_secs(2)), "ab");
        hold.arm(start);
        assert!(hold.offer(start, "c", false, false));
        assert_eq!(hold.take(start + HOLD), "");
        assert!(!hold.armed(start + HOLD));
    }

    #[test]
    fn a_field_that_has_the_keyboard_ends_the_wait() {
        let start = Instant::now();
        let mut hold = TextHold::default();
        hold.arm(start);
        assert!(hold.offer(start, "q", false, false));
        hold.disarm();
        assert_eq!(hold.take(start), "");
        hold.arm(start);
        assert!(hold.offer(start, "w", true, true));
        assert!(!hold.offer(start, "e", true, false));
        assert_eq!(hold.take(start), "");
    }
}
