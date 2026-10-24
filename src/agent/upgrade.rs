use std::{
    fs::{self, FileType},
    path::{Path, PathBuf},
};

use anyhow::{Result, anyhow, bail};

use crate::{
    agent::store::{AGENTS, DIR, FORMAT, ITEMS, LINES, SESSIONS, THREADS, check_name},
    file::{self, AtPath, pretty},
    project::Project,
    repo, segments,
};

/// The format a store converts from: format 4's segmented files kept whole.
const FROM: u32 = 3;

/// What a store awaiting conversion answers every command that is not run on a terminal.
pub const CONVERT: &str = "This project's agents are stored in format 3. Run an ai agent command on a terminal to convert them to format 4.";

/// Where a conversion builds the new agent store: in the git directory of the private `store`,
/// which no commit holds.
fn building(store: &Path) -> PathBuf {
    repo::git_dir(store).join("ai-agents-v4")
}

/// Where a conversion keeps the old agent store while it swaps the two.
fn replaced(store: &Path) -> PathBuf {
    repo::git_dir(store).join("ai-agents-v3")
}

/// Whether the agent store in the private `store` awaits conversion: it is format 3, or a
/// conversion of it was cut short.
pub fn pending(store: &Path) -> Result<bool> {
    Ok(building(store).exists()
        || replaced(store).exists()
        || file::format(&store.join(DIR), "agent")? == Some(FROM))
}

/// Converts the agent store of the project at or above `cwd` to format 4 when it awaits that,
/// once `ask` agrees, and commits it; an error if `ask` declines.
pub fn offer(cwd: &Path, ask: impl FnOnce(&str) -> Result<bool>) -> Result<()> {
    let Some(project) = Project::find(cwd)? else {
        return Ok(());
    };
    let store = project.store();
    if !pending(&store)? {
        return Ok(());
    }
    let question = format!(
        "Convert this project's agents ({}) to format {FORMAT}, splitting their growing files into 1 MiB segments?",
        pretty(&store.join(DIR))
    );
    if !ask(&question)? {
        bail!("Conversion cancelled; this project's agents are unchanged.");
    }
    convert(&project)?;
    project.commit(&format!("ai agent: convert to format {FORMAT}"))?;
    eprintln!("Converted this project's agents to format {FORMAT}.");
    Ok(())
}

/// Builds the format-4 store beside the old one, checks every byte, then swaps them; no commit
/// runs meanwhile. A conversion cut short is finished: before the swap it starts again, after
/// it the old store is removed.
fn convert(project: &Project) -> Result<()> {
    let _commits = repo::lock(&project.repo()?)?;
    let store = project.store();
    let (root, new, old) = (store.join(DIR), building(&store), replaced(&store));
    if old.exists() {
        if !root.exists() {
            fs::rename(&new, &root).at(&new)?;
        } else if new.exists() {
            bail!(
                "Cannot convert {}: both {} and {} exist. Keep the one to use as {0}.",
                pretty(&root),
                pretty(&new),
                pretty(&old)
            );
        }
        return fs::remove_dir_all(&old).at(&old);
    }
    if new.exists() {
        fs::remove_dir_all(&new).at(&new)?;
    }
    if file::format(&root, "agent")? != Some(FROM) {
        bail!(
            "Cannot convert {}: it is not a format-{FROM} store.",
            pretty(&root)
        );
    }
    copy_store(&root, &new)?;
    fs::rename(&root, &old).at(&root)?;
    fs::rename(&new, &root).at(&new)?;
    fs::remove_dir_all(&old).at(&old)
}

/// The entries of `dir`: name, path, type.
fn entries(dir: &Path) -> Result<Vec<(String, PathBuf, FileType)>> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir).at(dir)? {
        let e = e.at(dir)?;
        let name = e
            .file_name()
            .into_string()
            .map_err(|n| anyhow!("{}: a name is not UTF-8: {n:?}", pretty(dir)))?;
        out.push((name, e.path(), e.file_type().at(dir)?));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn unexpected(p: &Path) -> anyhow::Error {
    anyhow!(
        "Cannot convert the agent store: {} is not part of a format-3 store.",
        pretty(p)
    )
}

/// The format-3 store at `src` as format 4 at `dst`: logs kept, locks left behind.
fn copy_store(src: &Path, dst: &Path) -> Result<()> {
    file::ensure_format(dst, FORMAT)?;
    for (name, from, kind) in entries(src)? {
        let to = dst.join(&name);
        match name.as_str() {
            "format" => {}
            SESSIONS if kind.is_file() => segment(&from, &to)?,
            "sessions" if kind.is_dir() => {}
            THREADS if kind.is_dir() => {
                for (name, from, kind) in entries(&from)? {
                    if !kind.is_dir() || check_name(&name).is_err() {
                        return Err(unexpected(&from));
                    }
                    copy_thread(&from, &to.join(name))?;
                }
            }
            n if kind.is_file() && n.ends_with(".lock") => {}
            n if kind.is_file() && n.ends_with(".log") => {
                fs::copy(&from, &to).at(&from)?;
            }
            _ => return Err(unexpected(&from)),
        }
    }
    Ok(())
}

fn copy_thread(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).at(dst)?;
    for (name, from, kind) in entries(src)? {
        let to = dst.join(&name);
        match name.as_str() {
            ITEMS | LINES | SESSIONS | AGENTS if kind.is_file() => segment(&from, &to)?,
            "agents" | "sources" if kind.is_dir() => segment_tree(&from, &to)?,
            "calls" if kind.is_dir() => copy_tree(&from, &to)?,
            _ => return Err(unexpected(&from)),
        }
    }
    Ok(())
}

/// The whole file `src` as the segmented file `dst`, checked byte for byte.
fn segment(src: &Path, dst: &Path) -> Result<()> {
    let bytes = fs::read(src).at(src)?;
    segments::write(dst, &bytes)?;
    if segments::read_all(dst)? != bytes {
        bail!("Converting {} changed its bytes.", pretty(src));
    }
    Ok(())
}

/// Every file under `src` segmented, at the same path under `dst`.
fn segment_tree(src: &Path, dst: &Path) -> Result<()> {
    for (name, from, kind) in entries(src)? {
        let to = dst.join(name);
        match kind {
            k if k.is_dir() => segment_tree(&from, &to)?,
            k if k.is_file() => segment(&from, &to)?,
            _ => return Err(unexpected(&from)),
        }
    }
    Ok(())
}

/// Every file under `src`, copied to the same path under `dst`.
fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).at(dst)?;
    for (name, from, kind) in entries(src)? {
        let to = dst.join(name);
        match kind {
            k if k.is_dir() => copy_tree(&from, &to)?,
            k if k.is_file() => {
                fs::copy(&from, &to).at(&from)?;
            }
            _ => return Err(unexpected(&from)),
        }
    }
    Ok(())
}
