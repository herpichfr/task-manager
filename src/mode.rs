//! Editing mode and the pending multi-key-sequence accumulator.

/// The editing mode the app is currently in. Buffers for `Command`/`Search`
/// text entry live on `App`, not inside these variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
    Command,
    Search,
}

/// Accumulates a vim-style count (digits `1`-`9` then any digits; a leading
/// `0` is reserved, not a count) and a one-character command prefix (`g`,
/// `d`, `Z`) while a multi-key sequence is in progress.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PendingSeq {
    pub count: Option<u32>,
    pub prefix: Option<char>,
}

impl PendingSeq {
    /// Pushes one decimal digit (0-9) into the accumulating count. A `0`
    /// is only accepted once a count has already started (e.g. `1` then
    /// `0` making `10`); a bare leading `0` is reserved and left untouched.
    pub fn push_digit(&mut self, d: u32) {
        if d == 0 && self.count.is_none() {
            return;
        }
        self.count = Some(self.count.unwrap_or(0).saturating_mul(10).saturating_add(d));
    }

    /// Sets the pending one-character command prefix.
    pub fn set_prefix(&mut self, c: char) {
        self.prefix = Some(c);
    }

    /// Takes the accumulated count, or `default` if none was accumulated,
    /// clearing the count (the prefix, if any, is left untouched).
    pub fn take_count(&mut self, default: u32) -> u32 {
        self.count.take().unwrap_or(default)
    }

    /// Clears both the accumulated count and the pending prefix.
    pub fn clear(&mut self) {
        self.count = None;
        self.prefix = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_digit_single() {
        let mut p = PendingSeq::default();
        p.push_digit(3);
        assert_eq!(p.count, Some(3));
    }

    #[test]
    fn push_digit_multi_accumulates() {
        let mut p = PendingSeq::default();
        p.push_digit(1);
        p.push_digit(0);
        p.push_digit(2);
        assert_eq!(p.count, Some(102));
    }

    #[test]
    fn leading_zero_is_not_a_count() {
        let mut p = PendingSeq::default();
        p.push_digit(0);
        assert_eq!(p.count, None);
    }

    #[test]
    fn zero_after_nonzero_digit_accumulates() {
        let mut p = PendingSeq::default();
        p.push_digit(1);
        p.push_digit(0);
        assert_eq!(p.count, Some(10));
    }

    #[test]
    fn take_count_returns_default_when_empty() {
        let mut p = PendingSeq::default();
        assert_eq!(p.take_count(1), 1);
    }

    #[test]
    fn take_count_clears_count_only() {
        let mut p = PendingSeq::default();
        p.push_digit(5);
        p.set_prefix('g');
        assert_eq!(p.take_count(1), 5);
        assert_eq!(p.count, None);
        assert_eq!(p.prefix, Some('g'));
    }

    #[test]
    fn set_prefix_and_clear() {
        let mut p = PendingSeq::default();
        p.set_prefix('d');
        p.push_digit(2);
        p.clear();
        assert_eq!(p.prefix, None);
        assert_eq!(p.count, None);
    }
}
