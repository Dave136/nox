//! Linux suspend detection via systemd-logind's `PrepareForSleep` D-Bus
//! signal on the system bus, and best-effort GNOME/KDE screen-lock
//! detection via `org.gnome.ScreenSaver`/`org.freedesktop.ScreenSaver`'s
//! `ActiveChanged` signal on the session bus.
//!
//! Cannot be exercised by a unit test — there is no way to make systemd
//! actually suspend the machine (or fake a `PrepareForSleep` emission
//! end-to-end) from a test process; `busctl --system emit ...` does not
//! replicate logind's real suspend path. Likewise there is no way to fake a
//! real GNOME/KDE screen-lock event from a test process. Verify manually:
//! run `cargo run -p gui` on a real Linux machine or a VM with systemd and
//! `systemd-logind` running, then run `systemctl suspend` from a terminal
//! and confirm Nox is locked when the machine wakes back up; separately,
//! lock the screen through the desktop environment and confirm Nox is
//! locked when you return.
//!
//! Degrades to doing nothing, never panicking, when the relevant bus or
//! service isn't available (e.g. a container, an init system other than
//! systemd, or a desktop environment that owns neither the GNOME nor KDE
//! screensaver bus name) — this matches the best-effort platform
//! limitations already documented in `README.md`.

use super::{SuspendSignal, is_session_lock_edge, is_suspend_edge};
use futures::{StreamExt, channel::mpsc::UnboundedSender};
use zbus::proxy;

#[proxy(
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1",
    interface = "org.freedesktop.login1.Manager"
)]
trait Login1Manager {
    #[zbus(signal)]
    fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;
}

// Each screen-lock proxy trait is wrapped in its own private module: zbus's
// `#[proxy]` macro names the signal-support types it generates (e.g.
// `ActiveChanged`, `ActiveChangedArgs`) after the signal method name only,
// not the trait name, so two traits in the same module both declaring
// `active_changed` collide. Namespacing them like this changes nothing about
// the real D-Bus wire signal name ("ActiveChanged" on both interfaces) —
// only the generated Rust items' module paths.
mod gnome_proxy {
    #[zbus::proxy(
        default_service = "org.gnome.ScreenSaver",
        default_path = "/org/gnome/ScreenSaver",
        interface = "org.gnome.ScreenSaver"
    )]
    pub(super) trait GnomeScreenSaver {
        #[zbus(signal)]
        fn active_changed(&self, active: bool) -> zbus::Result<()>;
    }
}

mod kde_proxy {
    #[zbus::proxy(
        default_service = "org.freedesktop.ScreenSaver",
        default_path = "/org/freedesktop/ScreenSaver",
        interface = "org.freedesktop.ScreenSaver"
    )]
    pub(super) trait FreedesktopScreenSaver {
        #[zbus(signal)]
        fn active_changed(&self, active: bool) -> zbus::Result<()>;
    }
}

use gnome_proxy::GnomeScreenSaverProxy;
use kde_proxy::FreedesktopScreenSaverProxy;

/// Connects to systemd-logind on the system bus and sends
/// `SuspendSignal::Suspend` on `tx` once per suspend edge of
/// `PrepareForSleep`. Logs once and returns — never panics — if the system
/// bus or systemd-logind isn't available.
pub(crate) async fn watch_prepare_for_sleep(tx: UnboundedSender<SuspendSignal>) {
    let connection = match zbus::Connection::system().await {
        Ok(connection) => connection,
        Err(error) => {
            eprintln!("suspend listener: no system D-Bus connection: {error}");
            return;
        }
    };
    let proxy = match Login1ManagerProxy::new(&connection).await {
        Ok(proxy) => proxy,
        Err(error) => {
            eprintln!("suspend listener: systemd-logind unavailable: {error}");
            return;
        }
    };
    let mut signals = match proxy.receive_prepare_for_sleep().await {
        Ok(signals) => signals,
        Err(error) => {
            eprintln!("suspend listener: could not subscribe to PrepareForSleep: {error}");
            return;
        }
    };
    while let Some(signal) = signals.next().await {
        let Ok(args) = signal.args() else {
            continue;
        };
        if is_suspend_edge(args.start) && tx.unbounded_send(SuspendSignal::Suspend).is_err() {
            return;
        }
    }
}

/// Best-effort GNOME screen-lock listener: connects to the session bus and
/// sends `SuspendSignal::SessionLock` on `tx` once per lock edge of
/// `org.gnome.ScreenSaver`'s `ActiveChanged`. Logs once and returns — never
/// panics — if there is no session bus or GNOME's screensaver service isn't
/// running (e.g. a different desktop environment); this is the documented
/// best-effort limitation, not a bug to fix by adding more listeners here.
pub(crate) async fn watch_gnome_screensaver(tx: UnboundedSender<SuspendSignal>) {
    let connection = match zbus::Connection::session().await {
        Ok(connection) => connection,
        Err(error) => {
            eprintln!("session-lock listener: no session D-Bus connection: {error}");
            return;
        }
    };
    let proxy = match GnomeScreenSaverProxy::new(&connection).await {
        Ok(proxy) => proxy,
        Err(error) => {
            eprintln!("session-lock listener: org.gnome.ScreenSaver proxy failed: {error}");
            return;
        }
    };
    let mut signals = match proxy.receive_active_changed().await {
        Ok(signals) => signals,
        Err(error) => {
            eprintln!("session-lock listener: could not subscribe to GNOME ActiveChanged: {error}");
            return;
        }
    };
    while let Some(signal) = signals.next().await {
        let Ok(args) = signal.args() else {
            continue;
        };
        if is_session_lock_edge(args.active)
            && tx.unbounded_send(SuspendSignal::SessionLock).is_err()
        {
            return;
        }
    }
}

/// Best-effort KDE (and other freedesktop-compliant desktop environments)
/// screen-lock listener: the same shape as `watch_gnome_screensaver` against
/// `org.freedesktop.ScreenSaver` instead. Kept as a separate, independent
/// function rather than a shared abstraction over both interfaces — the two
/// D-Bus interfaces are similar by convention, not by any shared contract,
/// and duplicating fifteen straightforward lines is clearer than a generic
/// wrapper built for exactly two callers.
///
/// Targets `org.freedesktop.ScreenSaver`, which KDE implements but which
/// other desktop environments — including GNOME, via `gsd-screensaver` —
/// may also implement alongside `org.gnome.ScreenSaver`. On such a system
/// both this listener and `watch_gnome_screensaver` fire together on the
/// same screen lock; that's expected and harmless, since `lock_vault` is
/// idempotent, not a bug to dedupe.
pub(crate) async fn watch_kde_screensaver(tx: UnboundedSender<SuspendSignal>) {
    let connection = match zbus::Connection::session().await {
        Ok(connection) => connection,
        Err(error) => {
            eprintln!("session-lock listener: no session D-Bus connection: {error}");
            return;
        }
    };
    let proxy = match FreedesktopScreenSaverProxy::new(&connection).await {
        Ok(proxy) => proxy,
        Err(error) => {
            eprintln!("session-lock listener: org.freedesktop.ScreenSaver proxy failed: {error}");
            return;
        }
    };
    let mut signals = match proxy.receive_active_changed().await {
        Ok(signals) => signals,
        Err(error) => {
            eprintln!(
                "session-lock listener: could not subscribe to freedesktop ActiveChanged: {error}"
            );
            return;
        }
    };
    while let Some(signal) = signals.next().await {
        let Ok(args) = signal.args() else {
            continue;
        };
        if is_session_lock_edge(args.active)
            && tx.unbounded_send(SuspendSignal::SessionLock).is_err()
        {
            return;
        }
    }
}
