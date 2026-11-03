//! Folders and files as entries: what they hold, a copy that keeps modes, times and links, and
//! their size.

use std::{
    collections::BTreeMap,
    ffi::CString,
    fs::{self, DirBuilder, File, Metadata},
    io::{self, Read},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, MetadataExt, symlink},
    },
    path::Path,
};

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::file::{AtPath, pretty};

/// One entry under a folder, or the folder or file itself.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Entry {
    Dir {
        mode: u32,
    },
    File {
        mode: u32,
        size: u64,
        /// Its SHA-1, collision-checked as git checks it.
        sha1: String,
    },
    Link {
        to: String,
    },
    /// A socket, pipe or device: listed, never copied.
    Other,
}

/// Every entry by its path under the root, the root itself as `.`: two folders hold the same
/// data exactly when their inventories are equal.
pub type Inventory = BTreeMap<String, Entry>;

/// What `root` holds, following `root` itself when it is a link and no link under it.
pub fn inventory(root: &Path) -> Result<Inventory> {
    let mut inv = Inventory::new();
    let meta = fs::metadata(root).at(root)?;
    walk(root, ".", &meta, &mut inv)?;
    Ok(inv)
}

fn walk(p: &Path, rel: &str, meta: &Metadata, inv: &mut Inventory) -> Result<()> {
    let ft = meta.file_type();
    let mode = meta.mode() & 0o7777;
    let entry = if ft.is_dir() {
        for e in fs::read_dir(p).at(p)? {
            let e = e.at(p)?;
            let name = e.file_name();
            let Some(name) = name.to_str() else {
                bail!(
                    "{}: a name that isn't UTF-8 can't be recorded.",
                    pretty(&e.path())
                );
            };
            let child = if rel == "." {
                name.to_owned()
            } else {
                format!("{rel}/{name}")
            };
            let m = e.metadata().at(&e.path())?;
            walk(&e.path(), &child, &m, inv)?;
        }
        Entry::Dir { mode }
    } else if ft.is_file() {
        Entry::File {
            mode,
            size: meta.len(),
            sha1: sha1(p)?,
        }
    } else if ft.is_symlink() {
        let to = fs::read_link(p).at(p)?;
        let Some(to) = to.to_str() else {
            bail!(
                "{}: a link to a path that isn't UTF-8 can't be recorded.",
                pretty(p)
            );
        };
        Entry::Link { to: to.to_owned() }
    } else {
        Entry::Other
    };
    inv.insert(rel.to_owned(), entry);
    Ok(())
}

fn sha1(p: &Path) -> Result<String> {
    let mut f = File::open(p).at(p)?;
    let mut h = gix::hash::hasher(gix::hash::Kind::Sha1);
    let mut buf = vec![0; 1 << 20];
    loop {
        let n = f.read(&mut buf).at(p)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    let id = h
        .try_finalize()
        .map_err(|e| anyhow!("{}: {e}.", pretty(p)))?;
    Ok(id.to_string())
}

/// Copies `from` to `to`, which must not exist: folders, files and links, with their modes and
/// access and modification times. Refuses sockets, pipes and devices.
pub fn copy(from: &Path, to: &Path) -> Result<()> {
    let meta = fs::metadata(from).at(from)?;
    copy_entry(from, to, &meta)
}

fn copy_entry(from: &Path, to: &Path, meta: &Metadata) -> Result<()> {
    let ft = meta.file_type();
    if ft.is_dir() {
        DirBuilder::new().mode(0o700).create(to).at(to)?;
        for e in fs::read_dir(from).at(from)? {
            let e = e.at(from)?;
            let m = e.metadata().at(&e.path())?;
            copy_entry(&e.path(), &to.join(e.file_name()), &m)?;
        }
        fs::set_permissions(to, meta.permissions()).at(to)?;
    } else if ft.is_file() {
        fs::copy(from, to).at(from)?;
    } else if ft.is_symlink() {
        symlink(fs::read_link(from).at(from)?, to).at(to)?;
    } else {
        bail!(
            "{}: neither a folder, a file nor a link, so it can't be copied.",
            pretty(from)
        );
    }
    set_times(to, meta).at(to)
}

/// Gives `p`, not what it links to, the access and modification times in `meta`.
fn set_times(p: &Path, meta: &Metadata) -> io::Result<()> {
    let c = CString::new(p.as_os_str().as_bytes())?;
    let times = [
        libc::timespec {
            tv_sec: meta.atime() as _,
            tv_nsec: meta.atime_nsec() as _,
        },
        libc::timespec {
            tv_sec: meta.mtime() as _,
            tv_nsec: meta.mtime_nsec() as _,
        },
    ];
    // SAFETY: `c` is a valid C string and `times` holds the two timespecs utimensat reads.
    let r = unsafe {
        libc::utimensat(
            libc::AT_FDCWD,
            c.as_ptr(),
            times.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if r == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// The bytes of every file at or under `root`, following `root` itself and no link under it.
pub fn size(root: &Path) -> Result<u64> {
    let meta = fs::metadata(root).at(root)?;
    if !meta.is_dir() {
        return Ok(meta.len());
    }
    let mut n = 0;
    for e in fs::read_dir(root).at(root)? {
        let e = e.at(root)?;
        let m = e.metadata().at(&e.path())?;
        n += if m.is_dir() {
            size(&e.path())?
        } else if m.is_file() {
            m.len()
        } else {
            0
        };
    }
    Ok(n)
}

/// `n` bytes as a person reads them: `551M`, `1.2G`.
pub fn human(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u > 0 && v < 10.0 {
        format!("{v:.1}{}", UNITS[u])
    } else {
        format!("{v:.0}{}", UNITS[u])
    }
}

/// Removes `p`: a folder with everything under it, else the file or link itself.
pub fn remove(p: &Path) -> Result<()> {
    let m = fs::symlink_metadata(p).at(p)?;
    if m.is_dir() {
        fs::remove_dir_all(p).at(p)
    } else {
        fs::remove_file(p).at(p)
    }
}
