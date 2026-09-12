//! OS suspend (Phase 1) and, later, OS session/screen-lock (Phase 2)
//! detection, one listener implementation per supported platform.
//!
//! Neither signal can be triggered from a unit test — see each platform
//! module's doc comment for the actual manual verification steps. What is
//! unit-tested is the message-parsing/decision logic each listener forwards
//! into (`crates/gui/src/app.rs`'s `handle_suspend_signal`), kept separate
//! from "receive the raw OS signal" for exactly that reason.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

/// True only for the suspend edge of systemd-logind's
/// `PrepareForSleep(start: bool)` signal; the matching `start == false`
/// fires again on resume and must not lock a second time.
// wired in Task 5
#[allow(dead_code)]
pub(crate) fn is_suspend_edge(start: bool) -> bool {
    start
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_suspend_edge_is_true_only_when_going_to_sleep() {
        assert!(is_suspend_edge(true));
        assert!(!is_suspend_edge(false));
    }
}
