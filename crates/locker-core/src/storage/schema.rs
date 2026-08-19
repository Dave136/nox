//! SQLite schema owned by `locker-core`.

/// Current forward-only schema version.
pub const SCHEMA_VERSION: u32 = 1;

/// Tables created by the initial migration.
pub const TABLE_NAMES: [&str; 9] = [
    "vault_meta",
    "local_device",
    "memberships",
    "blocked_devices",
    "changes",
    "items",
    "conflicts",
    "sync_cursors",
    "clock_state",
];

/// Initial schema. All later migrations must be appended in `migrations.rs`.
pub const CREATE_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS vault_meta (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    format_version INTEGER NOT NULL,
    vault_id BLOB NOT NULL UNIQUE,
    kdf_algorithm TEXT NOT NULL,
    kdf_salt BLOB NOT NULL,
    argon2_memory_kib INTEGER NOT NULL,
    argon2_iterations INTEGER NOT NULL,
    argon2_parallelism INTEGER NOT NULL,
    key_wrap_algorithm TEXT NOT NULL,
    wrapped_dek_nonce BLOB NOT NULL,
    wrapped_dek BLOB NOT NULL
);

CREATE TABLE IF NOT EXISTS local_device (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    device_id BLOB NOT NULL UNIQUE,
    ed25519_public_key BLOB NOT NULL,
    x25519_public_key BLOB NOT NULL,
    encrypted_private_key_material BLOB NOT NULL
);

CREATE TABLE IF NOT EXISTS memberships (
    record_hash BLOB PRIMARY KEY,
    vault_id BLOB NOT NULL,
    record_type TEXT NOT NULL,
    record BLOB NOT NULL
);

CREATE TABLE IF NOT EXISTS blocked_devices (
    device_id BLOB PRIMARY KEY,
    blocked_at_physical_ms INTEGER NOT NULL,
    blocked_at_logical INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS changes (
    change_id BLOB PRIMARY KEY,
    vault_id BLOB NOT NULL,
    item_id BLOB NOT NULL,
    parent_change_ids BLOB NOT NULL,
    origin_device_id BLOB NOT NULL,
    origin_seq INTEGER NOT NULL CHECK (origin_seq >= 0),
    hlc_physical_ms INTEGER NOT NULL CHECK (hlc_physical_ms >= 0),
    hlc_logical INTEGER NOT NULL CHECK (hlc_logical >= 0),
    operation INTEGER NOT NULL,
    payload_schema_version INTEGER NOT NULL,
    nonce BLOB NOT NULL,
    ciphertext BLOB NOT NULL,
    signature BLOB NOT NULL,
    quarantined INTEGER NOT NULL DEFAULT 0 CHECK (quarantined IN (0, 1)),
    UNIQUE (origin_device_id, origin_seq)
);

CREATE INDEX IF NOT EXISTS changes_item_id_idx ON changes (item_id);
CREATE INDEX IF NOT EXISTS changes_origin_seq_idx
    ON changes (origin_device_id, origin_seq);

CREATE TABLE IF NOT EXISTS items (
    item_id BLOB PRIMARY KEY,
    winning_change_id BLOB NOT NULL,
    deleted INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0, 1)),
    FOREIGN KEY (winning_change_id) REFERENCES changes (change_id)
);

CREATE TABLE IF NOT EXISTS conflicts (
    item_id BLOB NOT NULL,
    losing_change_id BLOB NOT NULL,
    winning_change_id BLOB NOT NULL,
    PRIMARY KEY (item_id, losing_change_id),
    FOREIGN KEY (losing_change_id) REFERENCES changes (change_id),
    FOREIGN KEY (winning_change_id) REFERENCES changes (change_id)
);

CREATE TABLE IF NOT EXISTS sync_cursors (
    origin_device_id BLOB PRIMARY KEY,
    highest_contiguous_origin_seq INTEGER NOT NULL
        CHECK (highest_contiguous_origin_seq >= 0)
);

CREATE TABLE IF NOT EXISTS clock_state (
    vault_id BLOB PRIMARY KEY,
    hlc_physical_ms INTEGER NOT NULL CHECK (hlc_physical_ms >= 0),
    hlc_logical INTEGER NOT NULL CHECK (hlc_logical >= 0),
    next_origin_seq INTEGER NOT NULL CHECK (next_origin_seq >= 0)
);
"#;
