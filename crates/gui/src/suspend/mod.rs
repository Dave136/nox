//! OS suspend (Phase 1) and, later, OS session/screen-lock (Phase 2)
//! detection, one listener implementation per supported platform.
//!
//! Neither signal can be triggered from a unit test — see each platform
//! module's doc comment for the actual manual verification steps. What is
//! unit-tested is the message-parsing/decision logic each listener forwards
//! into (`crates/gui/src/app.rs`'s `handle_suspend_signal`), kept separate
//! from "receive the raw OS signal" for exactly that reason.

use futures::{StreamExt, channel::mpsc};
use gpui::{Context, Task, Window};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

/// True only for the suspend edge of systemd-logind's
/// `PrepareForSleep(start: bool)` signal; the matching `start == false`
/// fires again on resume and must not lock a second time.
pub(crate) fn is_suspend_edge(start: bool) -> bool {
    start
}

/// Starts the OS suspend listener for the current platform and returns the
/// `Task` driving it. The returned task never completes on its own — the
/// caller (`Nox::new`) must store it in a field that is never reassigned,
/// unlike `Nox::_inactivity_task`, because this must keep listening for the
/// app's entire lifetime, including while the vault is already locked
/// (harmless: `Nox::lock_vault` is a no-op then).
pub(crate) fn spawn_suspend_listener(
    window: &mut Window,
    cx: &mut Context<crate::app::Nox>,
) -> Task<()> {
    let (tx, mut rx) = mpsc::unbounded::<()>();

    #[cfg(target_os = "macos")]
    macos::watch_will_sleep(tx);
    #[cfg(target_os = "linux")]
    cx.background_executor()
        .spawn(linux::watch_prepare_for_sleep(tx))
        .detach();
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    drop(tx);

    cx.spawn_in(window, async move |this, cx| {
        while rx.next().await.is_some() {
            let updated = cx.update(|window, app| {
                this.update(app, |this, cx| this.handle_suspend_signal(window, cx))
            });
            if updated.is_err() {
                return;
            }
        }
    })
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
