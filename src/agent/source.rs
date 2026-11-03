use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};

use crate::agent::claude;

/// Where `harness` keeps its files, from the environment `var`: Claude Code's configuration
/// directory, pi's agent directory or DSH's home; `None` for a harness whose home ai does not
/// know, or without a home directory.
pub fn home(harness: &str, var: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let var = |k: &str| var(k).filter(|v| !v.is_empty());
    let (key, default) = match harness {
        claude::HARNESS => return claude::config_dir(var),
        "pi" => ("PI_CODING_AGENT_DIR", ".pi/agent"),
        "dsh" => ("DSH_HOME", ".dsh"),
        _ => return None,
    };
    var(key)
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(var("HOME")?).join(default)))
}

/// [`home`] from this process's environment.
pub fn home_here(harness: &str) -> Option<PathBuf> {
    home(harness, |k| std::env::var(k).ok())
}

/// How a thread records the harness file at `abs`: relative to the harness's `home` when it lies
/// there, so the record holds wherever the home's files move; else `abs` itself.
pub fn point(home: Option<&Path>, abs: &Path) -> PathBuf {
    let Some(home) = home else {
        return abs.to_path_buf();
    };
    let under = |h: &Path, p: &Path| {
        p.strip_prefix(h)
            .ok()
            .filter(|r| !r.as_os_str().is_empty())
            .map(Path::to_path_buf)
    };
    under(home, abs)
        .or_else(|| {
            let parent = fs::canonicalize(abs.parent()?).ok()?;
            under(
                &fs::canonicalize(home).ok()?,
                &parent.join(abs.file_name()?),
            )
        })
        .unwrap_or_else(|| abs.to_path_buf())
}

/// The harness file a thread's record `p` names: under `harness`'s `home` when relative.
pub fn locate(harness: &str, home: Option<&Path>, p: &Path) -> Result<PathBuf> {
    if p.is_absolute() {
        return Ok(p.to_path_buf());
    }
    match home {
        Some(h) => Ok(h.join(p)),
        None => bail!(
            "Cannot find where {harness} keeps its files, to read {}: set HOME.",
            p.display()
        ),
    }
}

/// The git blob id of `bytes`, in hex.
pub fn hash(bytes: &[u8]) -> Result<String> {
    gix::objs::compute_hash(gix::hash::Kind::Sha1, gix::objs::Kind::Blob, bytes)
        .map(|id| id.to_string())
        .map_err(|e| anyhow!("Cannot hash a file: {e}"))
}

pub(super) fn capture(path: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    let parent = path
        .parent()
        .context("A transcript needs a parent directory.")?;
    let mut files = BTreeMap::new();
    capture_at(path, parent, &mut files, false)?;
    let companion = path.with_extension("");
    if companion != path {
        capture_at(&companion, parent, &mut files, true)?;
    }
    Ok(files)
}

fn capture_at(
    path: &Path,
    parent: &Path,
    files: &mut BTreeMap<PathBuf, Vec<u8>>,
    allow_directory: bool,
) -> Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("Cannot inspect {}.", path.display()));
        }
    };
    if meta.is_symlink() {
        bail!("Cannot archive symbolic link {}.", path.display());
    }
    if meta.is_dir() {
        if !allow_directory {
            bail!("A transcript must be a regular file: {}.", path.display());
        }
        for entry in
            fs::read_dir(path).with_context(|| format!("Cannot read {}.", path.display()))?
        {
            capture_at(&entry?.path(), parent, files, true)?;
        }
    } else if meta.is_file() {
        let relative = path.strip_prefix(parent)?.to_path_buf();
        relative
            .to_str()
            .context("A source file name is not UTF-8.")?;
        let bytes = fs::read(path).with_context(|| format!("Cannot read {}.", path.display()))?;
        files.insert(relative, bytes);
    } else {
        bail!("Cannot archive non-regular file {}.", path.display());
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn source_capture_refuses_links_into_external_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let transcript = dir.path().join("s-1.jsonl");
        fs::write(&transcript, "").unwrap();
        let companion = transcript.with_extension("");
        fs::create_dir(&companion).unwrap();
        let outside = dir.path().join("external");
        fs::write(&outside, "outside session").unwrap();
        std::os::unix::fs::symlink(&outside, companion.join("linked-file")).unwrap();

        let error = capture(&transcript).unwrap_err();

        assert!(error.to_string().contains("Cannot archive symbolic link"));
    }

    #[test]
    fn a_directory_cannot_be_used_as_the_main_transcript() {
        let dir = tempfile::TempDir::new().unwrap();
        fs::write(dir.path().join("unrelated.jsonl"), "outside session").unwrap();

        let error = capture(dir.path()).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("A transcript must be a regular file")
        );
    }
}
