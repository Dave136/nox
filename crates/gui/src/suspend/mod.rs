//! OS suspend (Phase 1) and, later, OS session/screen-lock (Phase 2)
//! detection, one listener implementation per supported platform.
//!
//! Neither signal can be triggered from a unit test — see each platform
//! module's doc comment for the actual manual verification steps. What is
//! unit-tested is the message-parsing/decision logic each listener forwards
//! into (`crates/gui/src/app.rs`'s `handle_suspend_signal`), kept separate
//! from "receive the raw OS signal" for exactly that reason.

#[cfg(target_os = "macos")]
mod macos;
