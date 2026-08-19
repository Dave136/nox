//! Forward-only, transactional SQLite schema migrations.

use super::schema::{CREATE_SCHEMA, SCHEMA_VERSION};
use rusqlite::Connection;
use std::fmt;

/// Errors returned while reading or applying schema migrations.
#[derive(Debug)]
pub enum MigrationError {
    /// SQLite rejected a migration operation.
    Sql(rusqlite::Error),
    /// The database was created by a newer Locker release.
    NewerVersion(u32),
}

impl fmt::Display for MigrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sql(error) => write!(formatter, "SQLite migration failed: {error}"),
            Self::NewerVersion(version) => {
                write!(
                    formatter,
                    "database schema version {version} is newer than supported"
                )
            }
        }
    }
}

impl std::error::Error for MigrationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sql(error) => Some(error),
            Self::NewerVersion(_) => None,
        }
    }
}

impl From<rusqlite::Error> for MigrationError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sql(error)
    }
}

/// Apply all migrations required by the current library version.
pub fn migrate(connection: &mut Connection) -> Result<(), MigrationError> {
    let version = user_version(connection)?;
    if version > SCHEMA_VERSION {
        return Err(MigrationError::NewerVersion(version));
    }

    if version == 0 {
        let transaction = connection.transaction()?;
        transaction.execute_batch(CREATE_SCHEMA)?;
        transaction.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))?;
        transaction.commit()?;
    }

    Ok(())
}

/// Read SQLite's forward-only schema version.
pub fn user_version(connection: &Connection) -> Result<u32, MigrationError> {
    connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(MigrationError::from)
}
