//! Deterministic revision-DAG projection and conflict tracking.

use crate::{
    HlcOrderKey,
    crypto::{Ed25519Keypair, SecretKey, cipher},
    ids::{ChangeId, ItemId, VaultId},
    journal::{
        Change, JournalError, create_local_change_with_parents, load_item_changes, row_to_change,
    },
    storage::Db,
};
use rusqlite::{OptionalExtension, Transaction, params};
use std::collections::{HashMap, HashSet};

/// Merge operations currently reuse the journal's storage and validation errors.
pub type MergeError = JournalError;

/// Recompute one item's materialized winner and unresolved conflict heads.
pub fn apply_merge_projection(db: &mut Db, item_id: ItemId) -> Result<(), MergeError> {
    db.transaction(|tx| rebuild_item_projection(tx, item_id))
}

/// Return the unresolved revision heads for one item in deterministic order.
pub fn unresolved_heads(db: &Db, item_id: ItemId) -> Result<Vec<Change>, MergeError> {
    let changes = load_item_changes(db.connection(), item_id)?;
    let by_id = changes
        .iter()
        .map(|change| (change.change_id, change))
        .collect::<HashMap<_, _>>();
    validate_acyclic(&by_id)?;
    Ok(find_heads(&changes, &by_id).into_iter().cloned().collect())
}

/// Resolve all current heads by carrying one existing revision's payload
/// forward into a new multi-parent revision. No field-level merge is attempted.
#[allow(clippy::too_many_arguments)]
pub fn resolve_conflicts(
    db: &mut Db,
    vault_id: VaultId,
    item_id: ItemId,
    selected_change_id: ChangeId,
    signing_key: &Ed25519Keypair,
    encryption_key: &SecretKey,
    wall_clock_ms: u64,
) -> Result<Change, MergeError> {
    let heads = unresolved_heads(db, item_id)?;
    if heads.len() < 2 {
        return Err(JournalError::InvalidChange(
            "item has no unresolved conflicts",
        ));
    }
    let selected = heads
        .iter()
        .find(|change| change.change_id == selected_change_id)
        .ok_or(JournalError::InvalidChange(
            "selected revision is not an unresolved head",
        ))?;
    if selected.vault_id != vault_id {
        return Err(JournalError::InvalidChange(
            "selected revision is in another vault",
        ));
    }
    let decrypted = cipher::decrypt(
        encryption_key,
        &selected.aad_context(),
        &selected.encrypted_payload(),
    )?;
    let parent_change_ids = heads.iter().map(|change| change.change_id).collect();
    create_local_change_with_parents(
        db,
        vault_id,
        item_id,
        parent_change_ids,
        signing_key,
        encryption_key,
        selected.operation,
        decrypted.as_bytes(),
        selected.payload_schema_version,
        wall_clock_ms,
    )
}

/// Rebuild an item projection inside an already-open mutation transaction.
pub(crate) fn rebuild_item_projection(
    tx: &Transaction<'_>,
    item_id: ItemId,
) -> Result<(), JournalError> {
    let mut statement = tx.prepare(
        "SELECT change_id, vault_id, item_id, parent_change_ids, origin_device_id,
                origin_seq, hlc_physical_ms, hlc_logical, operation,
                payload_schema_version, nonce, ciphertext, signature
         FROM changes WHERE item_id = ?1",
    )?;
    let changes = statement
        .query_map([item_id.as_ref()], row_to_change)?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);

    if changes.is_empty() {
        return Err(JournalError::InvalidChange(
            "cannot merge an item without changes",
        ));
    }
    let by_id = changes
        .iter()
        .map(|change| (change.change_id, change))
        .collect::<HashMap<_, _>>();
    validate_parent_items(tx, item_id, &changes)?;
    validate_acyclic(&by_id)?;

    let heads = find_heads(&changes, &by_id);
    let winner = heads
        .iter()
        .copied()
        .max_by_key(|change| {
            HlcOrderKey::new(change.hlc, change.origin_device_id, change.origin_seq)
        })
        .ok_or(JournalError::InvalidChange("revision DAG has no head"))?;

    tx.execute(
        "INSERT INTO items (item_id, winning_change_id, deleted) VALUES (?1, ?2, ?3)
         ON CONFLICT(item_id) DO UPDATE SET
            winning_change_id = excluded.winning_change_id,
            deleted = excluded.deleted",
        params![
            item_id.as_ref(),
            winner.change_id.as_ref(),
            i64::from(winner.is_tombstone()),
        ],
    )?;
    tx.execute(
        "DELETE FROM conflicts WHERE item_id = ?1",
        [item_id.as_ref()],
    )?;
    for loser in heads {
        if loser.change_id != winner.change_id {
            tx.execute(
                "INSERT INTO conflicts (item_id, losing_change_id, winning_change_id)
                 VALUES (?1, ?2, ?3)",
                params![
                    item_id.as_ref(),
                    loser.change_id.as_ref(),
                    winner.change_id.as_ref(),
                ],
            )?;
        }
    }
    Ok(())
}

fn validate_parent_items(
    tx: &Transaction<'_>,
    item_id: ItemId,
    changes: &[Change],
) -> Result<(), JournalError> {
    for change in changes {
        for parent_id in &change.parent_change_ids {
            let parent = tx
                .query_row(
                    "SELECT item_id FROM changes WHERE change_id = ?1",
                    [parent_id.as_ref()],
                    |row| row.get::<_, Vec<u8>>(0),
                )
                .optional()?;
            if let Some(parent_item_id) = parent
                && parent_item_id.as_slice() != item_id.as_ref()
            {
                return Err(JournalError::InvalidChange(
                    "revision parent belongs to another item",
                ));
            }
        }
    }
    Ok(())
}

fn validate_acyclic(changes: &HashMap<ChangeId, &Change>) -> Result<(), JournalError> {
    for change_id in changes.keys().copied() {
        visit(change_id, changes, &mut HashSet::new())?;
    }
    Ok(())
}

fn visit(
    change_id: ChangeId,
    changes: &HashMap<ChangeId, &Change>,
    visiting: &mut HashSet<ChangeId>,
) -> Result<(), JournalError> {
    if !visiting.insert(change_id) {
        return Err(JournalError::InvalidChange("revision DAG contains a cycle"));
    }
    if let Some(change) = changes.get(&change_id) {
        for parent_id in &change.parent_change_ids {
            if changes.contains_key(parent_id) {
                visit(*parent_id, changes, visiting)?;
            }
        }
    }
    visiting.remove(&change_id);
    Ok(())
}

fn is_ancestor(
    ancestor_id: ChangeId,
    descendant_id: ChangeId,
    changes: &HashMap<ChangeId, &Change>,
) -> bool {
    let mut stack = changes
        .get(&descendant_id)
        .map(|change| change.parent_change_ids.clone())
        .unwrap_or_default();
    while let Some(parent_id) = stack.pop() {
        if parent_id == ancestor_id {
            return true;
        }
        if let Some(parent) = changes.get(&parent_id) {
            stack.extend(parent.parent_change_ids.iter().copied());
        }
    }
    false
}

fn find_heads<'a>(changes: &'a [Change], by_id: &HashMap<ChangeId, &'a Change>) -> Vec<&'a Change> {
    let mut heads = changes
        .iter()
        .filter(|candidate| {
            !changes.iter().any(|other| {
                other.change_id != candidate.change_id
                    && is_ancestor(candidate.change_id, other.change_id, by_id)
            })
        })
        .collect::<Vec<_>>();
    heads.sort_by_key(|change| change.change_id);
    heads
}
