//! macOS suspend detection via `NSWorkspaceWillSleepNotification`, and
//! session/screen-lock detection via the undocumented
//! `"com.apple.screenIsLocked"` distributed notification.
//!
//! Cannot be exercised by a unit test — there is no way to make macOS post
//! a real sleep or screen-lock notification from a test process. Verify
//! manually: run `cargo run -p gui` on a real Mac (or a macOS VM with sleep
//! support), then run `pmset sleepnow` from a terminal and confirm Nox locks
//! before the display goes dark (add a temporary
//! `eprintln!("suspend: will sleep")` at the top of the closure below if the
//! lock isn't visually obvious with the screen already off).

use super::SuspendSignal;
use block2::RcBlock;
use futures::channel::mpsc::UnboundedSender;
use objc2_app_kit::{NSWorkspace, NSWorkspaceWillSleepNotification};
use objc2_foundation::{NSDistributedNotificationCenter, NSNotification, NSString};
use std::ptr::NonNull;

/// Registers an observer for `NSWorkspaceWillSleepNotification` that sends
/// `SuspendSignal::Suspend` on `tx` once per suspend event, for the
/// remaining process lifetime.
///
/// The observer and its block are intentionally never unregistered:
/// `NSNotificationCenter` keeps its own strong reference to the block for as
/// long as the process runs, which is exactly the lifetime this listener
/// needs — there is no point in the app's life where it should stop
/// listening for suspend while still running.
pub(crate) fn watch_will_sleep(tx: UnboundedSender<SuspendSignal>) {
    let workspace = NSWorkspace::sharedWorkspace();
    let center = workspace.notificationCenter();
    let block = RcBlock::new(move |_note: NonNull<NSNotification>| {
        // The receiver may already be gone if the app is shutting down;
        // dropping the event is correct in that case, not a bug.
        let _ = tx.unbounded_send(SuspendSignal::Suspend);
    });
    // Safety: `addObserverForName:object:queue:usingBlock:` requires the
    // block's signature to match `NSNotificationCenter`'s documented
    // `(NSNotification *) -> Void`, which is exactly `block`'s type here.
    // `queue: None` delivers on the thread that posts the notification
    // (AppKit's main thread) — this function is only ever called from
    // `Nox::new`, which itself runs on the main thread inside
    // `cx.open_window`'s callback.
    let _observer = unsafe {
        center.addObserverForName_object_queue_usingBlock(
            Some(NSWorkspaceWillSleepNotification),
            None,
            None,
            &block,
        )
    };
}

/// Registers an observer for the undocumented but long-standing
/// `"com.apple.screenIsLocked"` distributed notification (there is no
/// public `NSNotificationName` constant for it, so it's spelled out as a
/// literal string) that sends `SuspendSignal::SessionLock` on `tx` once per
/// screen-lock event, for the remaining process lifetime.
///
/// Cannot be exercised by a unit test — verify manually: run
/// `cargo run -p gui` on a real Mac, lock the screen (Control+Command+Q, or
/// let the display sleep with a screen lock configured), and confirm Nox
/// is locked when you return.
pub(crate) fn watch_screen_locked(tx: UnboundedSender<SuspendSignal>) {
    let center = NSDistributedNotificationCenter::defaultCenter();
    let name = NSString::from_str("com.apple.screenIsLocked");
    let block = RcBlock::new(move |_note: NonNull<NSNotification>| {
        let _ = tx.unbounded_send(SuspendSignal::SessionLock);
    });
    let _observer = unsafe {
        center.addObserverForName_object_queue_usingBlock(Some(&name), None, None, &block)
    };
}
