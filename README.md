# Locker

Locker is a native desktop password manager for a **single-device** release.
It stores one local encrypted vault per installation. Synchronization is not
available yet.

## What works now

- One local encrypted vault per installation.
- Login and Secure Note items.
- Create, edit, delete, restore, search, and local password generation.
- Explicit locking and an inactivity timeout while the unlocked Locker window
  is inactive.
- Conditional clipboard clearing that leaves newer clipboard content alone.
- Encrypted backup export and restore.
- Local conflict selection when conflicting journal revisions are present.

## Platform support

Version 1 supports **macOS and Linux**. Windows is unsupported.

Locker currently has no dedicated operating-system suspend or session-lock
notification in its GUI stack. The macOS and Linux Wayland rows below therefore
record the accepted v1 limitation; Linux X11 was not exercised and remains a
separate unverified environment.

| Environment | Explicit Lock | Inactivity timeout | Suspend | OS session lock |
| --- | --- | --- | --- | --- |
| macOS | Not verified | Not verified | Not guaranteed—lock explicitly | Not guaranteed—lock explicitly |
| Linux Wayland / GNOME | Not verified | Not verified | Not guaranteed—lock explicitly | Not guaranteed—lock explicitly |
| Linux X11 | Not verified | Not verified | Not verified | Not verified |

The Suspend and OS session lock cells for macOS and Linux Wayland record the
accepted v1 limitation, not positive runtime claims. Explicitly lock Locker
before suspending the machine, locking the operating-system session, or leaving
it unattended. The real
production-duration and suspend/session runs must be completed on supported
hardware before the unverified explicit-lock and inactivity rows can support a
release claim. Locker does not currently promise immediate locking on a suspend
or operating-system session-lock event.

## Data and security model

While the vault is locked, item contents and vault secret keys are encrypted in
the local database. This is intended to protect item plaintext and keys if the
database is copied or device storage is stolen. The database can still reveal
limited metadata such as record counts, ciphertext sizes, timestamps, and
membership-related records.

While the vault is unlocked, decrypted data is available to the running Locker
process. Search decrypts data into memory and does not persist a plaintext
search index. Locking changes the application state and drops Locker-owned
unlocked state; it cannot erase copies already captured by the operating system,
another program, clipboard history, screenshots, or a user.

Encryption at rest is therefore a locked-state guarantee, not protection from a
compromised or already-unlocked computer.

## Passwords, backups, and recovery

### Master password

Locker cannot recover or reset a forgotten local master password. The creation
screen says:

> Locker cannot recover a forgotten master password. Store it somewhere safe.

After the vault is locked or Locker is restarted, losing the master password
means the local vault cannot be unlocked again. If Locker is still unlocked,
export a fresh encrypted backup before locking. A usable backup can still help
recover the data if its backup password is known: restore lets you choose a new
local master password; it does not recover or reset the old one.

### Backups

- Export is manual. Locker does not create, schedule, upload, replicate, or
  verify backups for you.
- Each backup is a point-in-time encrypted archive. Changes made after export
  are not in that archive.
- A backup needs both the archive and its backup password. Locker cannot recover
  or reset a forgotten backup password. The export screen says:

  > Locker cannot recover this backup password. Without it, the backup cannot be restored.

- The backup password and local master password have different roles. Restore
  accepts the backup password and asks for a new local master password.
- Restore validates the archive before changing local vault contents. With no
  local vault, a successful restore creates the destination; with an existing
  vault, it replaces that destination. The restored vault remains locked. The
  restore screen says:

  > The restored vault stays locked. The new master password cannot be recovered.

- A failed import leaves an existing vault unchanged. This does not remove the
  need for a separate backup before an operation that can replace a vault.
- Locker has no non-destructive backup-verification command. Keep multiple
  versioned backup files and their passwords in separately protected, durable
  locations. Do not use the only live vault as a restore-test fixture: a
  successful restore can replace newer local data.

Do not reuse the local master password as the backup password. Encryption does
not make one backup durable, and multiple copies do not help if their required
password is lost. Recovery is impossible when access to the local vault and
every usable encrypted backup are both lost.

## Security limitations

Locker does not protect against:

- a compromised operating system or an unlocked Locker process;
- keyloggers, screen capture, or a malicious clipboard manager;
- a malicious or compromised device that is already a trusted vault member;
- recovery of plaintext that a formerly trusted device copied before removal;
- denial of service by a LAN peer; or
- loss of both the master password and every usable encrypted backup.

Device membership and LAN synchronization are not part of this single-device
release. The trusted-member and LAN-peer limitations describe the trust boundary
that will apply when synchronization ships; they are not a claim that this GUI
currently exposes a network surface.

Locker conditionally clears the clipboard after its timeout only when the
clipboard still contains text Locker copied. It does not disable operating
system or third-party clipboard history and cannot erase copies already read
elsewhere.

## Current scope / not yet available

- No device pairing or LAN synchronization.
- No cloud account, hosted service, WAN relay, or telemetry.
- No browser extension, autofill, or mobile client.
- One vault per local installation.
- No multi-user sharing or roles.
- No Windows support.
- Only Login and Secure Note items; card, identity, and custom fields are
  deferred.

## Design details

See [`ARCHITECTURE.md`](ARCHITECTURE.md) for the design, security model,
verification requirements, and future protocol boundaries.
