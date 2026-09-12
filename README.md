<p align="center" style="background-color: #15181e; padding: 16px; width: 42px; border-radius: 12px; border: 1px solid #36393f; padding-bottom: 10px; margin-left: auto; margin-right: auto">
  <img src="crates/gui/src/assets/icons/nox-logo.svg" alt="Nox logo" width="96" style="filter: invert(1);" />
</p>


<h1 align="center">Nox</h1>

<p align="center">
  <img alt="Status" src="https://img.shields.io/badge/status-work--in--progress-orange">
  <img alt="Platforms" src="https://img.shields.io/badge/platforms-macOS%20%7C%20Linux-blue">
</p>

<p align="center">
  <a href="#what-works-now">What works</a> ·
  <a href="#platform-support">Platforms</a> ·
  <a href="#data-and-security-model">Security model</a> ·
  <a href="#passwords-backups-and-recovery">Backups</a> ·
  <a href="#security-limitations">Limitations</a> ·
  <a href="#not-yet-available">Roadmap</a> ·
  <a href="ARCHITECTURE.md">Architecture</a>
</p>

---

**🚧 Work in progress.** Nox is a local-first, multi-vault credential manager
for macOS and Linux. It stores Login and Secure Note items in encrypted local
vaults with journal-based revisions and local conflict resolution already
built in. **Version 1 targets a single device only** — a peer-to-peer sync
layer (Noise-protocol pairing and replication, see `crates/sync`) exists in
the codebase but is not yet wired into the app.

## What works now

- Multiple local encrypted vaults per installation.
- Login and Secure Note items.
- Create, edit, delete, restore, search, and local password generation.
- Explicit locking and an inactivity timeout while the unlocked Nox window
  is inactive.
- Automatic locking when the operating system suspends (macOS and Linux).
- Conditional clipboard clearing that leaves newer clipboard content alone.
- Encrypted backup export and restore.
- Local conflict selection when conflicting journal revisions are present.

## Platform support

Version 1 supports **macOS and Linux**. Windows is unsupported.

Nox listens for a suspend signal on both macOS (`NSWorkspaceWillSleepNotification`)
and Linux (systemd-logind's `PrepareForSleep`) and locks the vault immediately.
Nox has no dedicated operating-system session/screen-lock notification yet —
that row remains the accepted v1 limitation described below.

| Environment | Explicit Lock | Inactivity timeout | Suspend | OS session lock |
| --- | --- | --- | --- | --- |
| macOS | Not verified | Not verified | Locks on suspend (manually verified; no automated coverage) | Not guaranteed—lock explicitly |
| Linux Wayland / GNOME | Not verified | Not verified | Locks on suspend (manually verified; no automated coverage) | Not guaranteed—lock explicitly |
| Linux X11 | Not verified | Not verified | Locks on suspend (manually verified; no automated coverage) | Not guaranteed—lock explicitly |

The OS session lock column records the accepted v1 limitation, not a positive
runtime claim: explicitly lock Nox before locking the operating-system session
or leaving it unattended. Suspend detection relies on a real suspend/resume
cycle to verify (see the implementation plan under
`docs/superpowers/plans/` for the exact manual steps); it has no automated
test coverage because no unit test can make the OS actually suspend. The real
production-duration and explicit-lock/inactivity runs must still be completed
on supported hardware before those rows can support a release claim.

## Data and security model

While the vault is locked, item contents and vault secret keys are encrypted in
the local database. This is intended to protect item plaintext and keys if the
database is copied or device storage is stolen. The database can still reveal
limited metadata such as record counts, ciphertext sizes, timestamps, and
membership-related records.

While the vault is unlocked, decrypted data is available to the running Nox
process. Search decrypts data into memory and does not persist a plaintext
search index. Locking changes the application state and drops Nox-owned
unlocked state; it cannot erase copies already captured by the operating system,
another program, clipboard history, screenshots, or a user.

Encryption at rest is therefore a locked-state guarantee, not protection from a
compromised or already-unlocked computer.

## Passwords, backups, and recovery

### Master password

Nox cannot recover or reset a forgotten local master password. The creation
screen says:

> Nox cannot recover a forgotten master password. Store it somewhere safe.

After the vault is locked or Nox is restarted, losing the master password
means the local vault cannot be unlocked again. If Nox is still unlocked,
export a fresh encrypted backup before locking. A usable backup can still help
recover the data if its backup password is known: restore lets you choose a new
local master password; it does not recover or reset the old one.

### Backups

- Export is manual. Nox does not create, schedule, upload, replicate, or
  verify backups for you.
- Each backup is a point-in-time encrypted archive. Changes made after export
  are not in that archive.
- A backup needs both the archive and its backup password. Nox cannot recover
  or reset a forgotten backup password. The export screen says:

  > Nox cannot recover this backup password. Without it, the backup cannot be restored.

- The backup password and local master password have different roles. Restore
  accepts the backup password and asks for a new local master password.
- Restore validates the archive before changing local vault contents. With no
  local vault, a successful restore creates the destination; with an existing
  vault, it replaces that destination. The restored vault remains locked. The
  restore screen says:

  > The restored vault stays locked. The new master password cannot be recovered.

- A failed import leaves an existing vault unchanged. This does not remove the
  need for a separate backup before an operation that can replace a vault.
- Nox has no non-destructive backup-verification command. Keep multiple
  versioned backup files and their passwords in separately protected, durable
  locations. Do not use the only live vault as a restore-test fixture: a
  successful restore can replace newer local data.

Do not reuse the local master password as the backup password. Encryption does
not make one backup durable, and multiple copies do not help if their required
password is lost. Recovery is impossible when access to the local vault and
every usable encrypted backup are both lost.

## Security limitations

Nox does not protect against:

- a compromised operating system or an unlocked Nox process;
- keyloggers, screen capture, or a malicious clipboard manager;
- a malicious or compromised device that is already a trusted vault member;
- recovery of plaintext that a formerly trusted device copied before removal;
- denial of service by a LAN peer; or
- loss of both the master password and every usable encrypted backup.

Device membership and LAN synchronization are not part of this single-device
release. The trusted-member and LAN-peer limitations describe the trust boundary
that will apply when synchronization ships; they are not a claim that this GUI
currently exposes a network surface.

Nox conditionally clears the clipboard after its timeout only when the
clipboard still contains text Nox copied. It does not disable operating
system or third-party clipboard history and cannot erase copies already read
elsewhere.

## Not yet available

- Device pairing and LAN synchronization.
- WAN relay.
- Browser extension and autofill.
- Mobile client.
- Multi-user sharing or roles.
- Windows support.
- Card, identity, and custom item fields beyond Login and Secure Note.

## Architecture

See [`ARCHITECTURE.md`](ARCHITECTURE.md) for the design, security model,
verification requirements, and future protocol boundaries.
