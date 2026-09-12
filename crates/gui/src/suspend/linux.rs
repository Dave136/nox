//! Linux suspend detection via systemd-logind's `PrepareForSleep` D-Bus
//! signal on the system bus.
//!
//! Cannot be exercised by a unit test — there is no way to make systemd
//! actually suspend the machine (or fake a `PrepareForSleep` emission
//! end-to-end) from a test process; `busctl --system emit ...` does not
//! replicate logind's real suspend path. Verify manually: run
//! `cargo run -p gui` on a real Linux machine or a VM with systemd and
//! `systemd-logind` running, then run `systemctl suspend` from a terminal
//! and confirm Nox is locked when the machine wakes back up.
//!
//! Degrades to doing nothing, never panicking, when there is no system bus
//! or no systemd-logind (e.g. a container, or an init system other than
//! systemd) — this matches the best-effort platform limitations already
//! documented in `README.md`.

use super::is_suspend_edge;
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

/// Connects to systemd-logind on the system bus and sends `()` on `tx` once
/// per suspend edge of `PrepareForSleep`. Logs once and returns — never
/// panics — if the system bus or systemd-logind isn't available.
// wired in Task 5
#[allow(dead_code)]
pub(crate) async fn watch_prepare_for_sleep(tx: UnboundedSender<()>) {
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
        if is_suspend_edge(args.start) {
            let _ = tx.unbounded_send(());
        }
    }
}
