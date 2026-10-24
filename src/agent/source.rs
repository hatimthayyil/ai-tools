use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

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
