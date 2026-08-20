# Locker — Local-Sync Password Manager Architecture

## Status

This document is the implementation specification for Locker v1. It preserves the
original product direction while making the storage, cryptographic, trust, and
replication rules explicit enough to implement and test.

Confirmed product decisions:

- **Client:** native desktop GUI built with GPUI, not Tauri or a web shell. The
  `gui` crate uses `gpui-component` for pre-built widgets and `gpui-rsx`
  ([Dave136/gpui-rsx](https://github.com/Dave136/gpui-rsx)) for JSX-like view
  markup instead of raw GPUI method chaining.
- **Platforms:** macOS and Linux for v1. Windows is out of scope until GPUI support
  is suitable for release.
- **Topology:** symmetric peer-to-peer sync on the same LAN. There is no central
  coordinator, cloud account, relay, or hosted service, although each running peer
  listens for direct connections.
- **Trust:** a one-time out-of-band pairing code admits a device to a vault.
- **Conflicts:** last-write-wins at the item level, with every concurrent losing
  revision retained and shown for manual resolution.
- **Availability:** v1 unlocks before pairing or syncing. Background sync while the
  vault is locked is out of scope.
- **Vault scope:** one vault per local install for v1. Multiple vaults on the same
  device is out of scope until a real need is shown.

## Security and Failure Model

Locker v1 protects against:

- theft or copying of the local vault database while it is locked;
- passive or active LAN attackers who do not know the pairing code and are not
  already vault members;
- accidental overwrite of concurrent edits; and
- interrupted local writes, duplicate messages, reordered messages, and peers that
  are temporarily offline.

Locker v1 does **not** protect against:

- a compromised operating system, unlocked process, keylogger, screen capture, or
  malicious clipboard manager;
- a malicious or compromised device that is already a vault member;
- recovery of plaintext that a formerly trusted device copied before removal;
- denial of service by a LAN peer; or
- loss of both the master password and every usable encrypted backup.

Every admitted device is a full-trust vault member: while unlocked, it can decrypt
and modify the complete vault and may admit another device. The SQLite file exposes
limited metadata such as record counts, ciphertext sizes, timestamps, and device
membership, but never item plaintext or secret keys.

## Workspace and Responsibility Boundaries

```text
locker/
  Cargo.toml                 # workspace; Cargo.lock is committed
  crates/
    locker-core/             # model, crypto, SQLite, revisions, merge rules
    sync/                    # discovery, pairing, transport, replication
    gui/                     # GPUI binary and presentation only
```

`locker-core` keeps its full name because a bare `core` crate would collide with
Rust's own `core` crate name; `sync` and `gui` are unambiguous on their own.

### `locker-core`

`locker-core` has no networking. It owns:

- vault and item types;
- key derivation, wrapping, encryption, and secret handling;
- the SQLite schema and migrations;
- immutable change creation and validation;
- deterministic merge, conflict, and tombstone rules;
- replication cursor persistence; and
- encrypted backup and restore.

A local edit or received change is applied through one `locker-core` transaction.
The GUI and sync crate must not write database tables directly.

### `sync`

`sync` depends on the public `locker-core` API. It owns:

- mDNS discovery;
- pairing and vault admission;
- Noise handshakes and framed TCP messages;
- change and cursor exchange;
- connection limits, timeouts, retries, and duplicate-session handling.

It does not choose merge winners or manipulate plaintext item fields. It runs its
own `tokio` runtime; it does not assume anything about the caller's executor.

### `gui`

`gui` owns GPUI views and user interaction. It calls the core and sync APIs but
contains no cryptography, SQL, merge logic, or network protocol logic.

GPUI has its own executor and is not tokio-aware. `sync` runs on a background
tokio runtime; `gui` never `.await`s a tokio future directly from a view. Sync
events and results cross into GPUI's foreground executor over a plain async
channel that a GPUI task drains and turns into view-model updates.

GPUI, `gpui-component`, and `gpui-rsx` are all pre-1.0, so all three must be pinned
to an exact compatible release or Git revision. Do not use `version = "*"` for any
of them. `gpui-rsx` is a small, independently maintained macro crate, not part of
Zed's GPUI itself — it can lag or break independently of a GPUI upgrade, so bumping
GPUI and bumping `gpui-rsx`/`gpui-component` are treated as separate, independently
tested changes. The project follows the latest stable Rust required by whichever
pinned dependency needs it most.

## Identifiers and Clocks

- `VaultId`, `DeviceId`, `ItemId`, and `ChangeId` are distinct Rust newtypes.
- `VaultId` is randomly generated when a vault is created.
- `DeviceId` is derived from the device's canonical Ed25519 public key.
- `ItemId` and `ChangeId` use ULIDs generated locally.
- Each device maintains a persistent, monotonically increasing `origin_seq` per
  vault. Increment and change insertion happen in the same SQLite transaction.
- Every change also carries a persistent Hybrid Logical Clock (HLC) timestamp.

HLC timestamps determine a stable winner; they are **not** replication cursors and
do not prove that two revisions are concurrent. The complete deterministic ordering
key is:

```text
(hlc.physical_ms, hlc.logical, origin_device_id, origin_seq)
```

The HLC state survives restarts. A configurable future-skew limit defaults to five
minutes. A validly signed change beyond that limit is stored but quarantined from
merge until the clock catches up or the user explicitly accepts it. Invalid or
unauthenticated input never advances the local HLC.

## Vault and Key Hierarchy

### Vault header

Each local vault stores a versioned header containing:

```text
format_version
vault_id
kdf_algorithm
kdf_salt
argon2_memory_kib
argon2_iterations
argon2_parallelism
key_wrap_algorithm
wrapped_dek_nonce
wrapped_dek
```

The salt and nonce are generated with the operating system CSPRNG. KDF parameters
are stored with the vault so they can be increased later. Argon2id parameters are
fixed, conservative constants for v1 — at least 64 MiB of memory and enough
iterations to take roughly 250 ms on typical hardware from the last few years,
chosen ahead of time and hardcoded. A runtime calibration routine that tunes
iterations to the actual device is deferred until real hardware reports show the
fixed constants are wrong for some target.

### Key encryption key and data encryption key

1. The local master password and stored Argon2id parameters derive a 32-byte **key
   encryption key (KEK)**.
2. The KEK decrypts a random 32-byte **data encryption key (DEK)** from the vault
   header.
3. The DEK encrypts item revisions and other vault secrets.
4. Changing the local master password derives a new KEK and re-wraps only the DEK.
5. Different devices may use different local master passwords and KDF parameters
   while sharing the same vault DEK.

The password, KEK, DEK, decrypted item payloads, pairing secret, and session keys
must use secret-owning types that zeroize on drop where practical. Secret values
must never be included in logs, panic messages, tracing fields, or telemetry.

### Item encryption

Each encryption operation generates a fresh random 24-byte nonce and uses
XChaCha20-Poly1305. A nonce is never reused with the same DEK, including when an
existing item is edited.

The authenticated associated data is a canonical, versioned encoding of:

```text
vault_id
item_id
change_id
parent_change_ids
origin_device_id
origin_seq
hlc
operation
payload_schema_version
```

Binding metadata as associated data prevents a valid ciphertext from being moved to
a different vault, item, revision, or operation. A tombstone encrypts an empty
payload so its metadata still receives an authentication tag.

### Device identity keys

Every device generates two separate keypairs:

- Ed25519 for signing identity, membership, and change records;
- X25519 as the static Noise transport key.

Do not convert one key type into the other. A canonical identity statement binds the
public keys and is signed by the Ed25519 key:

```text
DeviceIdentity {
  device_id,
  ed25519_public_key,
  x25519_public_key,
  display_name,
  protocol_version,
  self_signature
}
```

Private identity keys are stored in the OS credential store where a supported,
secure backend is available. Otherwise they are encrypted under the vault DEK and
are unavailable until unlock. There is no fallback that encrypts a key with a
"device-local secret" stored beside the ciphertext.

## Vault Membership

Membership is vault-wide, not merely a collection of unrelated pairwise links.
Every member learns the public identities of the other current members and can make
a mutually authenticated Noise connection to them when discovered.

The creator writes a self-signed genesis membership record. Pairing adds an
immutable admission record signed by the inviting member and an acceptance signed
by the admitted device:

```text
MembershipAdmission {
  vault_id,
  admitted_device_identity,
  invited_by_device_id,
  admitted_at_hlc,
  signature
}

MembershipAcceptance {
  vault_id,
  admission_hash,
  admitted_device_id,
  accepted_at_hlc,
  signature
}
```

A peer accepts an admission only when its signature chains to an already accepted
member of the same vault. The new member becomes active only when both matching
records exist. The two-record handshake makes an interrupted pairing retryable
without pretending that two separate SQLite databases can commit atomically. Any
current member may invite a new device because the v1 system has no owner or
administrator role.

V1 has no distributed cryptographic revocation. "Forget device" adds the selected
identity to a **local block list**, closes its sessions, and refuses future
connections on that installation. The UI must not call this global revocation.
Other devices retain their own trust decisions, and the forgotten device retains
any vault data it previously copied.

## Pairing Protocol

Pairing admits Device B to Device A's vault and transfers the shared DEK.
Device A must be unlocked for the entire operation.

1. A generates a single-use pairing secret with at least 40 bits of entropy,
   displays it, and advertises a temporary pairing instance over mDNS.
2. The pairing instance expires after five minutes, accepts one successful pairing,
   limits failed attempts, and is destroyed on cancellation or application exit.
3. B enters the secret, discovers A, and opens a bounded, timeout-controlled TCP
   connection.
4. A and B run asymmetric SPAKE2 roles. The SPAKE2 identity strings bind the Locker
   protocol version, pairing instance, and role names to the transcript.
5. The SPAKE2 result is expanded into separate confirmation and encryption keys.
   Both sides perform an explicit second-round key confirmation over the complete
   transcript before transferring secrets.
6. B sends its signed device identity. A validates it, creates and persists a signed
   pending membership admission, then sends it with the vault ID, DEK, membership
   chain, and A's identity.
7. B chooses or confirms its local master password, derives its local KEK, persists
   a locally wrapped DEK and the admission, then returns a signed membership
   acceptance.
8. A persists the acceptance and returns a final confirmation. Both devices now
   consider B active, erase temporary secrets, stop the pairing advertisement, and
   switch future sessions to Noise.

The DEK flows from the unlocked existing member to the joining device only. The
joining device never sends the DEK back. An interrupted run may leave an inactive
admission without its matching acceptance. Neither side treats that as membership;
a retry can retransmit the signed records or discard the inactive admission. A new
PAKE run always uses a fresh pairing instance and secret.

## Post-Pairing Transport

Known members use the concrete Noise suite:

```text
Noise_KK_25519_ChaChaPoly_BLAKE2s
```

Both sides know the other's static X25519 key from the membership chain. The Noise
prologue binds the Locker protocol version and vault ID. Plain TCP is sufficient;
QUIC is intentionally omitted because Noise already supplies mutual authentication
and encryption.

Transport messages use a versioned, fixed binary encoding and a 32-bit
length-prefixed frame. Implementations enforce a conservative maximum frame size,
bounded change batches, handshake/read/write timeouts, and connection limits before
allocating from untrusted lengths.

To avoid simultaneous duplicate sessions, the lexicographically smaller `DeviceId`
is the preferred initiator. If both connect anyway, both peers deterministically
keep the connection initiated by the preferred device and close the other.

## Immutable Change Journal

The journal is the replication source and audit history. A change contains the
complete encrypted resulting revision; it is not merely an operation pointing at a
mutable item row.

```text
Change {
  change_id,
  vault_id,
  item_id,
  parent_change_ids,
  origin_device_id,
  origin_seq,
  hlc,
  operation: upsert | tombstone,
  payload_schema_version,
  nonce,
  ciphertext,
  signature
}
```

`parent_change_ids` normally contains the current winning revision. It may contain
multiple revisions when the user resolves conflicts. The change signature covers
the canonical encoding of every preceding field and allows a relaying peer to
forward another device's change without being able to alter or impersonate it.

The SQLite `items` table is only a materialized projection of the journal. Creating
a local change performs all of the following in one transaction:

1. read and validate the current revision;
2. allocate the next persistent `origin_seq` and HLC;
3. encrypt and sign the immutable change;
4. insert it into the journal; and
5. update the item and conflict projections.

Received changes are authenticated, deduplicated, and inserted through the same core
apply path. Duplicate `change_id` or `(origin_device_id, origin_seq)` values are
idempotent only when their signed bytes match exactly; mismatches are rejected as
corruption or protocol abuse.

## Replication Protocol

Each vault stores a cursor map:

```text
origin_device_id -> highest_contiguous_origin_seq
```

Peers exchange cursor maps, then send journal changes missing from the other map.
A peer may receive sequence 12 before sequence 11; it stores 12 but keeps the
contiguous cursor at 10 and continues requesting the gap. HLC values are never used
to decide which journal entries a peer has seen.

Minimal session flow:

1. complete Noise KK and confirm the vault and protocol versions;
2. exchange validated membership information and local block status;
3. exchange cursor summaries;
4. exchange bounded batches of missing changes;
5. validate and transactionally apply each batch;
6. acknowledge the new highest contiguous sequence per origin; and
7. repeat until both cursor maps match or the session deadline is reached.

Changes may be relayed: if A authored a change that B sent to C, C verifies A's
signature using the vault membership chain. Retries, duplicate delivery, connection
loss, and changes received in any order must converge to the same journal and
projection.

Discovery uses mDNS/DNS-SD service `_locker._tcp`. Advertisements expose only the
protocol version, listening port, and the stable device identifier needed to match a
known member. This leaks the presence of a Locker peer and its stable pseudonymous
identifier to the LAN; it does not expose vault contents, names, or keys. No mDNS
response means no automatic sync attempt in v1.

## Merge and Conflict Rules

The journal forms a per-item revision DAG through `parent_change_ids`.

For every item:

- If the incoming revision descends from the current winner, it becomes the winner.
- If the current winner descends from the incoming revision, the incoming revision
  is retained but does not replace the projection.
- If neither revision descends from the other, they are concurrent.
- Concurrent branches choose the highest deterministic HLC ordering key as the
  displayed winner and retain every losing head as a conflict.
- A conflict is a projection referencing the original revisions, not a synthetic
  duplicate journal change.
- Manual resolution creates one new revision whose parents include all resolved
  branch heads. Its payload is exactly one of the existing conflicting revisions,
  picked by the user; v1 has no field-level merge UI.

The merge operates on the whole encrypted item payload. V1 does not maintain
per-field clocks or automatically combine fields.

Deletes are ordinary signed tombstone revisions and participate in the same
ancestry and conflict rules as edits. An edit concurrent with a delete is preserved
as a conflict. Restoring an item creates a normal upsert revision descending from
the tombstone and any resolved conflict heads.

Journal revisions and tombstones are retained indefinitely in v1. Garbage
collection, compaction, and distributed acknowledgement-based deletion are deferred
until measured database growth justifies their protocol complexity.

## Item Model

The encrypted payload is versioned JSON:

```text
ItemPayload {
  schema_version,
  item_type: login | secure_note,
  title,
  username,
  password,
  uris,
  notes,
  created_at,
  updated_at
}
```

`card`, `identity`, and free-form `custom_fields` are deferred past v1.
`schema_version` exists specifically so those can be added later without a storage
migration.

Item type, title, username, URI, and notes remain encrypted. Search and filtering
occur in memory after unlock; v1 does not persist a plaintext search index.

## SQLite Storage

A single local SQLite database contains at least:

- `vault_meta` — versioned vault header and migration state;
- `local_device` — local public identity and encrypted private-key material;
- `memberships` — genesis and admission records;
- `blocked_devices` — local-only connection blocks;
- `changes` — immutable signed encrypted revisions;
- `items` — materialized winning revision per item;
- `conflicts` — unresolved losing revision references;
- `sync_cursors` — highest contiguous sequence per origin and peer state;
- `clock_state` — persistent local HLC and next origin sequence.

Schema changes use explicit, transactional migrations keyed by SQLite
`user_version`. The database and its parent directory use owner-only permissions
where supported. WAL files and backups are treated as part of the sensitive vault
because they expose the same ciphertext metadata.

All item mutation, journal append, projection update, HLC update, and sequence
allocation operations are atomic SQLite transactions. The application must never
acknowledge a remote batch before its transaction commits.

## Backup and Recovery

Because there is no server, encrypted backup and restore are v1 requirements.
Export writes a portable, versioned encrypted archive containing the vault header,
membership chain, and immutable journal. It contains no device private keys or
plaintext. The export is written to a temporary file, flushed, and atomically
renamed when supported.

Restore validates the archive version, limits, signatures, membership chain, AEAD
tags, and journal invariants before replacing any local state. It creates a new
local device identity and wrapper. That identity must pair with a current member
before joining the restored vault's sync mesh. If no member survives, recovery
imports the latest item state into a new vault ID and membership genesis rather than
forging membership in the old vault. Import failure leaves the existing vault
unchanged.

Locker cannot recover a forgotten master password. The UI must state this clearly
when the user creates a vault and backup.

## GPUI Application

Minimal v1 screens and behaviors:

### Unlock and locking

- Derive the KEK and unwrap the DEK without revealing whether failure came from a
  wrong password or a corrupted wrapper.
- Lock on explicit command, configurable inactivity timeout, operating-system
  suspend, and session lock where the platform exposes the event.
- On lock, stop sync, drop plaintext search state, and zeroize owned secret buffers.

### Vault list and editor

- Search and filter decrypted items in memory.
- Create, edit, delete, restore, and resolve item conflicts.
- Generate passwords locally with the OS CSPRNG and user-selected length and
  character classes.
- Copy username or password to the clipboard with a configurable timeout. Clear it
  only if the clipboard still contains the value Locker wrote, so newer user data
  is not overwritten.

### Devices and sync

- Show vault members, local blocks, last successful sync, and current errors.
- Pair a new device by showing or entering the one-time code.
- Label v1 removal as **Forget/block on this device**, not revoke.
- Provide manual "sync now" while unlocked.

### Backup

- Export an encrypted backup.
- Restore only after validation and explicit confirmation that identifies the
  affected local vault.

Errors shown in the GUI must be actionable but must not include passwords, keys,
item plaintext, ciphertext dumps, pairing secrets, or full protocol transcripts.

## Dependencies

Use the smallest maintained set that directly implements the design:

- Crypto and secrets: `argon2`, `chacha20poly1305`, `ed25519-dalek`,
  `x25519-dalek`, `spake2`, `snow`, `hkdf`, `sha2`, `zeroize`, `getrandom`.
- Model and encoding: `serde`, `serde_json`, a pinned deterministic binary encoding,
  and `ulid`.
- Storage: `rusqlite` with application-layer payload encryption; do not add
  SQLCipher in v1.
- Networking: `tokio`, `mdns-sd`, and plain TCP; do not add QUIC in v1. The
  `gui`-to-`sync` bridge is a plain async channel, not a second async runtime
  inside `gui`.
- GUI: exactly pinned `gpui`, `gpui-component`, and `gpui-rsx` dependencies.

Pin exact versions in `Cargo.lock`. Protocol and persisted formats must not depend on
unstable Rust memory layouts or an encoder's undocumented defaults.

## Build Phases

1. **Workspace:** create the three compiling crates, shared formatting/lint settings,
   and one-command checks.
2. **Core:** implement identifiers, HLC, KDF and envelope encryption, immutable
   signed changes, SQLite transactions and migrations, deterministic merge,
   conflicts, and encrypted backup/restore.
3. **Single-device GUI:** implement vault creation, unlock/lock, item CRUD, password
   generation, clipboard handling, conflict display, and backup without networking.
4. **Release hardening (single-device v1):** run dependency and security review,
   test on supported platforms, verify file permissions and suspend/session
   locking, and document recovery limitations, scoped to the single-device
   surface built through Phase 3. Ship a complete, usable, secure v1 release
   here — sync is deliberately not a precondition for it.
5. **Headless sync:** implement membership, SPAKE2 pairing, Noise KK, framed TCP,
   cursor-map replication, and mDNS discovery.
6. **GUI sync integration:** add pairing, device/block management, sync status, and
   manual sync.
7. **Sync release hardening:** repeat dependency and security review scoped to the
   new attack surface Phases 5–6 introduce — pairing protocol review, Noise
   implementation audit, mDNS LAN exposure, and cross-device interoperability
   testing on supported platforms. Does not repeat Phase 4's single-device-only
   coverage; it is a scoped follow-up, not a second full pass.

Sequencing single-device release hardening (Phase 4) before sync (Phases 5–6) is
deliberate: it ships a complete, secure, usable product without waiting on the
highest-risk remaining work, and it forces the item model and backup format to
stabilize under real usage before the sync wire protocol locks in assumptions
about them. Phase 7 exists because pairing, transport, and discovery are new
attack surface Phase 4's review never covered — deferring sync must not mean
deferring its own security review along with it.

Do not scaffold future browser, mobile, cloud, relay, multi-user, or plugin systems
inside these phases.

## Verification

### `locker-core`

- Argon2id known vectors and stored-parameter compatibility.
- DEK wrap/unwrap, item encryption, wrong-password, tamper, nonce, and AAD swap
  failures.
- Secret buffers are owned by zeroizing wrappers.
- Atomic change creation and crash-safe journal/projection consistency.
- Duplicate, reordered, and gapped origin sequences.
- Persistent HLC behavior, deterministic ties, and future-skew quarantine.
- Revision ancestry, edit/edit conflicts, edit/delete conflicts, multi-parent manual
  resolution, and restore.
- Convergence after applying the same valid changes in multiple orders.
- Transactional migrations and backup round trips; malformed archives never replace
  an existing vault.

### `sync`

A loopback integration test runs at least three independent profiles:

- A pairs with B, then A pairs with C; B and C learn and validate vault membership.
- Correct codes succeed; wrong, expired, replayed, and over-attempted codes fail.
- Pairing performs explicit key confirmation and leaves no partial admission after
  interruption.
- Unknown and locally blocked identities fail before application messages are
  accepted.
- A, B, and C create divergent edits, relay each other's changes, receive them out
  of order with duplicates and gaps, and converge.
- Concurrent edit/edit and edit/delete cases retain identical conflicts everywhere.
- Oversized frames, invalid signatures, altered ciphertext metadata, timeouts, and
  simultaneous connections are rejected deterministically.
- The test bypasses mDNS for repeatability; mDNS discovery receives a separate local
  smoke test.

### `gui`

Automate core view-model behavior where practical and manually verify. The
first five bullets are verifiable after Phase 3 and gate Phase 4's
single-device release; the last two depend on Phases 5–6 and gate Phase 7
instead — this list is the complete v1 checklist, reached in two passes, not
one:

- create vault → create item → lock → unlock → item persists;
- clipboard timeout does not overwrite content copied afterward;
- password generation uses requested constraints;
- conflict resolution creates a multi-parent revision and converges;
- encrypted export restores into a fresh profile;
- inactivity and suspend/session events lock and stop sync; and
- pairing and sync work across two supported machines on the same LAN.

The repository-wide release gate is:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

## Explicit V1 Non-Goals

- WAN sync, relay servers, cloud accounts, or telemetry.
- Browser extensions, autofill, and mobile clients.
- Multi-user vault sharing or roles.
- Multiple vaults per local install.
- Sync while locked.
- Per-field merge clocks and field-level conflict merging in the resolution UI
  (pick-a-revision only).
- Card, identity, and custom-field item types (deferred, not designed away).
- Runtime Argon2id calibration (fixed constants for v1).
- Journal compaction or tombstone garbage collection.
- Distributed cryptographic device revocation or automatic DEK rotation.
- Windows support until the pinned GPUI stack is release-ready there.
- QUIC, custom transport negotiation, or protocol plugin systems.

These boundaries are deliberate. Add any deferred mechanism only when a measured
product or operational need exceeds the simpler v1 behavior.

## Performance and Security Practices

These are cross-cutting rules that apply to every task in every crate, not
new features. They make the guarantees already described elsewhere in this
document actually hold once real code is written.

### Security

- **Never branch on secret validity in a way that leaks timing.** Password/
  KEK checks, signature verification, and pairing-code comparison must use
  constant-time comparison (`subtle` crate or the constant-time compare each
  crypto crate already provides) — do not add `==` on secret bytes.
- **Fail closed and generic.** A wrong master password and a corrupted DEK
  wrapper must produce the same user-visible error (already required by
  ARCHITECTURE.md's unlock section) — apply the same rule to signature and
  AEAD failures: report "invalid" without saying which check failed.
- **Secrets never cross a log, panic, `Debug`, or trace boundary.** Enforce
  this by construction: secret-owning types (Task 2 `secret.rs`) must not
  derive `Debug`/`Display`, or must hand-implement them to print a fixed
  redaction string.
- **No panics on untrusted input.** Anything parsed from the network, a
  restored backup, or another device's signed record must return `Result`,
  never `.unwrap()`/`.expect()`/array-index panic. Reserve `unwrap`/`expect`
  for invariants the local process itself established (e.g. "we just
  inserted this row").
- **Validate lengths and bounds before allocating.** Frame sizes, batch
  counts, and archive sizes are checked against a fixed maximum *before*
  `Vec::with_capacity`/`read_to_end`, per the transport section — apply the
  same rule to backup import.
- **Zeroize on drop, not just on the happy path.** Use `Drop` impls / the
  `zeroize` crate so a secret is wiped even when a function returns early
  via `?`.
- **Run `cargo audit` (or `cargo deny check advisories`) before each release
  gate**, not just once at project start — pinned git deps for GPUI/
  gpui-component/gpui-rsx are exempt from crates.io advisories and must be
  tracked manually against their upstream repos instead.
- **Treat WAL files, temp files, and backup exports as sensitive** — same
  owner-only permissions as the main DB file, same "no plaintext" rule for
  temp files during export (write ciphertext directly, never a plaintext
  intermediate on disk).

### Performance

- **Argon2id cost is a UX budget, not a security dial to tune down.** If
  unlock feels slow on real hardware, that's a signal for the deferred
  runtime-calibration feature, not a reason to lower the fixed v1 constants.
- **Every mutation is one SQLite transaction** (already required by the
  journal design) — this is also the performance rule: don't wrap multiple
  transactions in a retry loop where one would do, and don't hold a
  transaction open across network or GUI I/O.
- **Index what you query.** `changes` needs indexes on `(item_id)` and
  `(origin_device_id, origin_seq)`; `sync_cursors` is keyed by
  `origin_device_id`. Add indexes with the migration that creates the table,
  not as an afterthought.
- **Batch, don't chat.** Replication sends bounded batches of changes, not
  one round-trip per change (already specified) — keep the same principle
  inside `locker-core`: bulk-insert a restore/import journal in one
  transaction, not one transaction per change.
- **Never block the GPUI foreground executor.** Any `locker-core` or `sync`
  call that touches disk or network from a GUI callback must go through the
  async channel bridge already specified — no synchronous SQLite call
  directly on a GPUI view callback for anything larger than a single indexed
  row lookup.
- **In-memory search stays in memory.** Don't add a persisted plaintext
  index "for speed" — v1 explicitly decrypts to memory and searches there;
  if that's ever too slow, that's a measured decision to revisit, not a
  default.
- **Measure before adding caching or indexes beyond the above.** No
  speculative LRU caches, no query result caching layer — SQLite with the
  indexes above is fast enough for the target scale (one local vault, one
  user's items) until a profile says otherwise.

## Primary References

- [RFC 9106 — Argon2 Memory-Hard Function](https://www.rfc-editor.org/rfc/rfc9106)
- [RustCrypto XChaCha20-Poly1305 documentation](https://docs.rs/chacha20poly1305/)
- [RustCrypto SPAKE2 documentation](https://docs.rs/spake2/)
- [Noise Protocol Framework](https://noiseprotocol.org/noise.html)
- [Hybrid Logical Clocks paper](https://cse.buffalo.edu/~demirbas/publications/hlc.pdf)
- [SQLite transactional guarantees](https://www.sqlite.org/transactional.html)
- [GPUI README](https://github.com/zed-industries/zed/tree/main/crates/gpui)
