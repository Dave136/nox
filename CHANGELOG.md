# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## v0.1.1 - 2026-09-10

### Added

- Change vault password from Settings > Security, re-wrapping the vault's encryption key without touching its contents.

### Changed

- Settings dialog: removed the Privacy, Notifications, and Account sections, none of which had functional backing.
- Settings > Security: duration controls now render as visible selects instead of cycling on click, and row descriptions span full width with controls right-aligned.
- Settings dialog and surrounding app chrome now use shared theme tokens instead of hardcoded colors.
- Moved the vault lock button from the workspace header to the window title bar.
- Matched Recent items and Recently deleted row heights and hover color to the Quick actions styling.
- Removed macOS Intel builds from the release workflow.

### Fixed

- Secure Notes detail view and full-note copy now render and strip `:::copy` / `:::copy-locked` markdown fences correctly, matching the create/edit preview.

## v0.1.0 - 2026-09-09

### Added

- Initial Nox desktop app for macOS and Linux.
- Local encrypted vault creation, unlock, explicit lock, and multi-vault management.
- Login and Secure Note item support with create, edit, delete, restore, search, and favorites.
- Local password generator with strength feedback.
- Secure Note formatting support, including markdown preview, note colors, tags, and copy blocks.
- Custom item icons, preset icons, uploaded local icons, and login favicon fetching.
- Trash view with deleted-item previews and restore actions.
- Conditional clipboard clearing that only clears Nox-owned copied secrets when unchanged.
- Manual encrypted backup export and restore.
- Local journal-based revisions, deterministic merge behavior, tombstones, and conflict selection.
- Foundations for LAN sync, including pairing, Noise handshakes, transport, replication, and device blocking in the `sync` crate.
- GitHub Actions release workflow for tagged app releases with Linux and macOS binaries, compressed archives, checksums, and curated changelog notes.

### Security

- Encrypts vault contents at rest while locked using Argon2id-derived keys and authenticated item encryption.
- Keeps search local without persisting a plaintext search index.
- Uses restrictive file permissions for vaults, backups, WAL sidecars, and local icon/favicon caches on Unix platforms.
- Rejects unsafe backup and vault paths that would follow symlinks into unintended locations.
- Bounds and validates favicon/icon downloads before caching them locally.

### Known limitations

- Device pairing and LAN synchronization are not wired into the app yet; v0.1.0 is a single-device release.
- No browser extension, autofill, mobile client, WAN relay, sharing roles, or Windows support.
- No macOS signing or notarization yet.
- No automatic backup scheduling or non-destructive backup verification command.
- Lock on operating-system suspend/session-lock is not guaranteed; lock Nox explicitly before leaving a device unattended.
