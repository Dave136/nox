//! Thin SQLite connection boundary for all locker-core mutations.

use super::migrations::{self, MigrationError};
use rusqlite::{Connection, Transaction};
use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
};

/// Errors returned while opening or using a vault database.
#[derive(Debug)]
pub enum DbError {
    /// SQLite rejected an operation.
    Sql(rusqlite::Error),
    /// The schema could not be migrated.
    Migration(MigrationError),
    /// The database path or permissions could not be prepared.
    Io(io::Error),
}

impl fmt::Display for DbError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sql(error) => write!(formatter, "SQLite error: {error}"),
            Self::Migration(error) => error.fmt(formatter),
            Self::Io(error) => write!(formatter, "database path error: {error}"),
        }
    }
}

impl std::error::Error for DbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sql(error) => Some(error),
            Self::Migration(error) => Some(error),
            Self::Io(error) => Some(error),
        }
    }
}

impl From<rusqlite::Error> for DbError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sql(error)
    }
}

impl From<MigrationError> for DbError {
    fn from(error: MigrationError) -> Self {
        Self::Migration(error)
    }
}

impl From<io::Error> for DbError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// An opened, migrated vault database.
pub struct Db {
    connection: Connection,
    path: Option<PathBuf>,
}

impl Db {
    /// Open or create a file-backed vault database and apply migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DbError> {
        let path = path.as_ref();
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        set_owner_only_directory(parent)?;

        let connection = Connection::open(path)?;
        set_owner_only_file(path)?;
        let mut db = Self {
            connection,
            path: Some(path.to_path_buf()),
        };
        db.configure()?;
        db.migrate()?;
        Ok(db)
    }

    /// Open an in-memory database for tests or ephemeral operations.
    pub fn open_in_memory() -> Result<Self, DbError> {
        let connection = Connection::open_in_memory()?;
        let mut db = Self {
            connection,
            path: None,
        };
        db.configure()?;
        db.migrate()?;
        Ok(db)
    }

    /// Borrow the underlying connection for read-only queries.
    #[must_use]
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Return the file path, or `None` for an in-memory database.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Return the current SQLite schema version.
    pub fn user_version(&self) -> Result<u32, DbError> {
        Ok(migrations::user_version(&self.connection)?)
    }

    /// Re-run forward-only migrations; already-current databases are a no-op.
    pub fn migrate(&mut self) -> Result<(), DbError> {
        Ok(migrations::migrate(&mut self.connection)?)
    }

    /// Execute one mutation transaction and roll it back on callback failure.
    pub fn transaction<F, T, E>(&mut self, callback: F) -> Result<T, E>
    where
        F: FnOnce(&Transaction<'_>) -> Result<T, E>,
        E: From<rusqlite::Error>,
    {
        let transaction = self.connection.transaction().map_err(E::from)?;
        let value = callback(&transaction)?;
        transaction.commit().map_err(E::from)?;
        Ok(value)
    }

    fn configure(&self) -> Result<(), DbError> {
        self.connection.pragma_update(None, "foreign_keys", true)?;
        self.connection
            .busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(())
    }
}

#[cfg(unix)]
fn set_owner_only_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_owner_only_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_owner_only_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_owner_only_file(_path: &Path) -> io::Result<()> {
    Ok(())
}
