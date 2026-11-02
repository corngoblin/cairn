//! Progress reporting, shared by the CLI and the window.
//!
//! A plain callback rather than a channel: the extraction and the enumeration both
//! run on a worker thread, and the window already has to marshal to the main loop,
//! so an extra queue in the middle would add a place for a stale bar to hide.

/// Which stage of an operation a report describes. Only [`Phase::Writing`],
/// [`Phase::Removing`] and [`Phase::Verifying`] have a meaningful ratio; the rest
/// drive an indeterminate bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    /// Opening the storage and listing its files. Minutes on a Battle.net install.
    #[default]
    Enumerating,
    /// Diffing the archives against what is already on disk. Writes nothing.
    Planning,
    /// Writing files.
    Writing,
    /// Deleting files the archives no longer contain.
    Removing,
    /// Check-summing extracted files against their recorded keys.
    Verifying,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::Enumerating => "Reading the game archives",
            Phase::Planning => "Comparing with what is on disk",
            Phase::Writing => "Extracting",
            Phase::Removing => "Removing stale files",
            Phase::Verifying => "Verifying extracted files",
        }
    }

    /// Whether the phase's file/byte counts mean anything.
    pub fn has_ratio(self) -> bool {
        matches!(self, Phase::Writing | Phase::Removing | Phase::Verifying)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Progress {
    pub phase: Phase,
    pub files_done: u64,
    pub files_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// The file being worked on, or a status line during enumeration.
    pub current: String,
}

impl Progress {
    pub fn enumerating(message: impl Into<String>) -> Self {
        Self {
            phase: Phase::Enumerating,
            current: message.into(),
            ..Default::default()
        }
    }

    /// Fraction complete in `0.0..=1.0`, when the phase has one.
    pub fn fraction(&self) -> Option<f64> {
        if !self.phase.has_ratio() {
            return None;
        }
        if self.bytes_total > 0 {
            Some((self.bytes_done as f64 / self.bytes_total as f64).clamp(0.0, 1.0))
        } else if self.files_total > 0 {
            Some((self.files_done as f64 / self.files_total as f64).clamp(0.0, 1.0))
        } else {
            None
        }
    }
}

/// A borrowed progress callback. Borrowed rather than owned so a caller can pass
/// `&|p| { … }` and keep its own state, with no allocation and no `'static` bound.
#[derive(Clone, Copy)]
pub struct ProgressSink<'a> {
    f: &'a dyn Fn(&Progress),
}

fn ignore(_: &Progress) {}

impl<'a> ProgressSink<'a> {
    pub fn new(f: &'a dyn Fn(&Progress)) -> Self {
        Self { f }
    }

    /// A sink that discards everything, for callers that only want the result.
    pub fn silent() -> Self {
        Self { f: &ignore }
    }

    pub fn report(&self, progress: &Progress) {
        (self.f)(progress)
    }
}

impl Default for ProgressSink<'_> {
    fn default() -> Self {
        Self::silent()
    }
}

/// Throttles reports to at most one per configured interval, so 150k files do not
/// become 150k window updates. The final report of a run always gets through —
/// otherwise a bar stops at 99% forever.
pub struct Throttle {
    last: std::cell::Cell<std::time::Instant>,
    interval: std::time::Duration,
}

impl Throttle {
    pub fn new(millis: u64) -> Self {
        Self {
            last: std::cell::Cell::new(
                std::time::Instant::now() - std::time::Duration::from_secs(1),
            ),
            interval: std::time::Duration::from_millis(millis),
        }
    }

    pub fn ready(&self) -> bool {
        let now = std::time::Instant::now();
        if now.duration_since(self.last.get()) >= self.interval {
            self.last.set(now);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratios_only_exist_for_phases_that_have_them() {
        let mut p = Progress::enumerating("scanning");
        assert!(p.fraction().is_none());
        p.phase = Phase::Writing;
        p.bytes_done = 25;
        p.bytes_total = 100;
        assert_eq!(p.fraction(), Some(0.25));
    }

    #[test]
    fn a_silent_sink_swallows_reports() {
        let sink = ProgressSink::silent();
        sink.report(&Progress::enumerating("nothing to see"));
    }

    #[test]
    fn throttle_lets_the_first_and_then_periodic_reports_through() {
        let t = Throttle::new(50);
        assert!(t.ready(), "the first report must always get through");
        assert!(!t.ready(), "an immediate second one is suppressed");
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert!(t.ready());
    }
}
