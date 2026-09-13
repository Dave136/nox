//! Linux suspend detection via systemd-logind's `PrepareForSleep` D-Bus
//! signal on the system bus, and session-lock detection via two independent
//! sources: logind's own `Session.LockedHint` property (primary — see
//! `watch_locked_hint`'s doc comment for why) and best-effort GNOME/KDE
//! `ActiveChanged` signals on the session bus (secondary, for environments
//! where those fire correctly).
//!
//! Cannot be exercised by a unit test — there is no way to make systemd
//! actually suspend the machine (or fake a `PrepareForSleep` emission
//! end-to-end) from a test process; `busctl --system emit ...` does not
//! replicate logind's real suspend path. Likewise there is no way to fake a
//! real session-lock event from a test process. Verify manually: run
//! `cargo run -p gui` on a real Linux machine or a VM with systemd and
//! `systemd-logind` running, then run `systemctl suspend` from a terminal
//! and confirm Nox is locked when the machine wakes back up; separately,
//! lock the screen (a desktop environment's lock shortcut, or
//! `loginctl lock-session`) and confirm Nox is locked when you return.
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

    fn get_session_by_pid(&self, pid: u32) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
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

/// Session-lock listener via systemd-logind's `Session.LockedHint` property
/// (watched through the standard `org.freedesktop.DBus.Properties.
/// PropertiesChanged` signal on the session's own object, on the system
/// bus) rather than the desktop-environment screensaver interfaces below.
///
/// This is the primary session-lock signal, not a fallback: on a real GNOME
/// session (GNOME Shell, this repo's development reference desktop),
/// `org.gnome.ScreenSaver`'s `ActiveChanged` was verified NOT to fire for
/// either the Super+L keybinding or `loginctl lock-session` — GNOME Shell's
/// `org.gnome.ScreenSaver` compatibility script only received the `Lock()`
/// method call, never emitted `ActiveChanged` back out. `LockedHint`, by
/// contrast, flips reliably for both triggers (confirmed with
/// `loginctl show-session -p LockedHint` before/after). It's also the
/// desktop-environment-agnostic mechanism systemd itself documents for this
/// purpose, unlike the screensaver interfaces which are a GNOME/KDE-specific
/// legacy compatibility layer with no shared implementation. The GNOME/KDE
/// listeners below stay as a second, independent source for environments
/// where they do emit correctly — `lock_vault` is idempotent, so overlap is
/// harmless.
pub(crate) async fn watch_locked_hint(tx: UnboundedSender<SuspendSignal>) {
    let connection = match zbus::Connection::system().await {
        Ok(connection) => connection,
        Err(error) => {
            eprintln!("session-lock listener: no system D-Bus connection for LockedHint: {error}");
            return;
        }
    };
    let manager = match Login1ManagerProxy::new(&connection).await {
        Ok(proxy) => proxy,
        Err(error) => {
            eprintln!("session-lock listener: systemd-logind unavailable for LockedHint: {error}");
            return;
        }
    };
    let session_path = match manager.get_session_by_pid(std::process::id()).await {
        Ok(path) => path,
        Err(error) => {
            eprintln!("session-lock listener: could not resolve our logind session: {error}");
            return;
        }
    };
    let properties = match zbus::fdo::PropertiesProxy::builder(&connection)
        .destination("org.freedesktop.login1")
        .and_then(|builder| builder.path(session_path))
    {
        Ok(builder) => match builder.build().await {
            Ok(proxy) => proxy,
            Err(error) => {
                eprintln!(
                    "session-lock listener: could not build logind Properties proxy: {error}"
                );
                return;
            }
        },
        Err(error) => {
            eprintln!(
                "session-lock listener: could not configure logind Properties proxy: {error}"
            );
            return;
        }
    };
    let mut changes = match properties.receive_properties_changed().await {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!(
                "session-lock listener: could not subscribe to logind PropertiesChanged: {error}"
            );
            return;
        }
    };
    while let Some(change) = changes.next().await {
        let Ok(args) = change.args() else {
            continue;
        };
        if args.interface_name() != "org.freedesktop.login1.Session" {
            continue;
        }
        let Some(locked) = args
            .changed_properties()
            .get("LockedHint")
            .and_then(|value| bool::try_from(value.clone()).ok())
        else {
            continue;
        };
        if is_session_lock_edge(locked) && tx.unbounded_send(SuspendSignal::SessionLock).is_err() {
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
