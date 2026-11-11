//! Harness databases, copied before a run backs them up.
//!
//! A database a harness keeps writing is never read as a file: a copy taken mid-write may hold
//! half a transaction. Each is copied with SQLite's backup API, which gives every committed
//! transaction, those still in the write-ahead log included, and the copy is what the run backs
//! up. A copy sits under the staging folder at its database's own path.

use std::{
    fs::{self, DirBuilder},
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use rusqlite::{
    Connection, OpenFlags,
    backup::{Backup, StepResult},
};

use crate::file::{AtPath, pretty};

/// How long a copy waits for a database another program has locked.
const BUSY: Duration = Duration::from_secs(10);

/// Where the copy of the database at `db`, an absolute path, sits under `staging`.
pub fn staged(staging: &Path, db: &Path) -> PathBuf {
    staging.join(db.strip_prefix("/").unwrap_or(db))
}

/// Copies the database at `db`, where it really sits, to its place under `staging`. A copy the
/// same as the one there leaves that one untouched, so an unchanged database is no change to
/// back up.
pub fn stage(staging: &Path, db: &Path) -> Result<()> {
    let to = staged(staging, db);
    let dir = to.parent().unwrap_or(staging);
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .at(dir)?;
    let tmp = tempfile::NamedTempFile::new_in(dir).at(dir)?;
    {
        let src = Connection::open_with_flags(
            db,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("Cannot open the database {} to copy it.", pretty(db)))?;
        src.busy_timeout(BUSY)?;
        let mut dst = Connection::open(tmp.path())
            .with_context(|| format!("Cannot make a copy in {}.", pretty(dir)))?;
        let backup = Backup::new(&src, &mut dst)?;
        // The whole database in one step: a copy in several starts over at every write.
        let step = backup
            .step(-1)
            .with_context(|| format!("Cannot copy the database {}.", pretty(db)))?;
        if step != StepResult::Done {
            bail!(
                "Cannot copy the database {}: another program kept it locked for {}s.",
                pretty(db),
                BUSY.as_secs()
            );
        }
    }
    let same = match fs::read(&to) {
        Ok(held) => held == fs::read(tmp.path()).at(tmp.path())?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(e).at(&to),
    };
    if !same {
        tmp.persist(&to).map_err(|e| e.error).at(&to)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(db: &Path) -> i64 {
        Connection::open(db)
            .unwrap()
            .query_row("select count(*) from t", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn a_copy_holds_what_the_write_ahead_log_holds_and_stays_when_nothing_changed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let db = root.join("h/state.db");
        fs::create_dir_all(db.parent().unwrap()).unwrap();
        let live = Connection::open(&db).unwrap();
        live.pragma_update(None, "journal_mode", "wal").unwrap();
        live.execute_batch("create table t (x); insert into t values (1), (2);")
            .unwrap();
        let staging = root.join("staging");
        let to = staged(&staging, &db);
        assert_eq!(to, staging.join(db.strip_prefix("/").unwrap()));

        stage(&staging, &db).unwrap();
        assert_eq!(rows(&to), 2);
        let first = fs::metadata(&to).unwrap().modified().unwrap();

        stage(&staging, &db).unwrap();
        assert_eq!(fs::metadata(&to).unwrap().modified().unwrap(), first);

        live.execute("insert into t values (3)", []).unwrap();
        stage(&staging, &db).unwrap();
        assert_eq!(rows(&to), 3);
        assert_eq!(fs::read_dir(to.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn what_is_no_database_is_an_error_naming_it() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let db = root.join("state.db");
        fs::write(
            &db,
            "not a database, but long enough to have a header to misread",
        )
        .unwrap();
        let e = format!("{:#}", stage(&root.join("staging"), &db).unwrap_err());
        assert!(e.contains("state.db"), "{e}");
    }
}
