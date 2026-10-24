use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    str::FromStr,
};

use anyhow::{Context, Result, anyhow, bail};
use mti::prelude::*;
use serde::Deserialize;
use tempfile::NamedTempFile;
use toml_edit::DocumentMut;

use crate::{
    config::{self, FILE, LOCAL},
    file::{AtPath, pretty},
};

/// The type of a project's id: `project_` and a UUIDv7, as a TypeID.
const PREFIX: &str = "project";

/// `[project]` of a project's `ai.toml`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Table {
    pub id: Option<String>,
    pub name: Option<String>,
}

/// A project: the directory of the nearest `ai.toml` at or above where `ai` runs, never the
/// user's, holding the id `ai init` minted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    /// The directory holding its `ai.toml`.
    pub dir: PathBuf,
    /// Where it keeps what is not checked in: see [`home`].
    pub home: PathBuf,
    pub id: String,
    /// Its slug: `[project] name`, else the directory's name.
    pub name: String,
}

impl Project {
    /// The project at or above `dir`; `None` without an `ai.toml` holding an id.
    pub fn find(dir: &Path) -> Result<Option<Project>> {
        match config::project_file(dir) {
            Some(p) => Project::read(&p),
            None => Ok(None),
        }
    }

    /// [`Project::find`], else an error saying how to make one.
    pub fn require(dir: &Path) -> Result<Project> {
        if let Some(p) = Project::find(dir)? {
            return Ok(p);
        }
        match config::project_file(dir) {
            Some(p) => bail!(
                "{} has no project id. Run `ai init` in {}.",
                pretty(&p),
                pretty(p.parent().unwrap_or(&p))
            ),
            None => bail!(
                "Not in a project: no {FILE} here or above. Run `ai init` in the project's top \
                 directory."
            ),
        }
    }

    /// The project of the `ai.toml` at `p`; `None` without the file or an id in it.
    fn read(p: &Path) -> Result<Option<Project>> {
        let t = config::read(p)?.and_then(|f| f.project).unwrap_or_default();
        let Some(id) = t.id else {
            return Ok(None);
        };
        check_id(&id).map_err(|e| anyhow!("{}: [project] id: {e}", pretty(p)))?;
        let dir = p.parent().unwrap_or(p).to_path_buf();
        let name = t.name.unwrap_or_else(|| {
            dir.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        Ok(Some(Project {
            home: home(&dir)?,
            dir,
            id,
            name,
        }))
    }

    /// The project at `dir`, minting an id into its `ai.toml` when it has none, the rest of the
    /// file kept as written, with its `ai.local.toml` in its `.gitignore`.
    pub fn init(dir: &Path) -> Result<Project> {
        let p = dir.join(FILE);
        if config::is_user(&p) {
            bail!(
                "{} holds your own settings; a project's {FILE} sits in its top directory.",
                pretty(&p)
            );
        }
        let project = match Project::read(&p)? {
            Some(project) => project,
            None => mint(dir, &p)?,
        };
        ignore_local(dir)?;
        Ok(project)
    }
}

/// Where the project at `dir` keeps what is not checked in: in a linked git worktree, the same
/// directory in the main checkout; else `dir`.
pub fn home(dir: &Path) -> Result<PathBuf> {
    let Ok(repo) = gix::discover(dir) else {
        return Ok(dir.to_path_buf());
    };
    if !matches!(repo.kind(), gix::repository::Kind::LinkedWorkTree) {
        return Ok(dir.to_path_buf());
    }
    let main = repo
        .main_repo()
        .with_context(|| format!("Cannot open the main checkout of {}.", pretty(dir)))?;
    let (Some(tree), Some(root)) = (repo.workdir(), main.workdir()) else {
        return Ok(dir.to_path_buf());
    };
    let tree = fs::canonicalize(tree).at(tree)?;
    let dir = fs::canonicalize(dir).at(dir)?;
    Ok(match dir.strip_prefix(&tree) {
        Ok(rel) => root.join(rel),
        Err(_) => dir,
    })
}

/// Mints an id into the `ai.toml` at `p`, in `dir`.
fn mint(dir: &Path, p: &Path) -> Result<Project> {
    let text = read_or_empty(p)?;
    let mut doc: DocumentMut = text.parse().map_err(|e| anyhow!("{}: {e}", pretty(p)))?;
    let table = doc.entry("project").or_insert(toml_edit::table());
    table["id"] = toml_edit::value(PREFIX.create_type_id::<V7>().to_string());
    write(dir, p, &doc.to_string())?;
    Project::read(p)?.context("ai init wrote no project id.")
}

/// Adds `/ai.local.toml` to the `.gitignore` in `dir`, unless a line there already ignores it.
fn ignore_local(dir: &Path) -> Result<()> {
    let p = dir.join(".gitignore");
    let mut text = read_or_empty(&p)?;
    let line = format!("/{LOCAL}");
    if text.lines().any(|l| l.trim() == line || l.trim() == LOCAL) {
        return Ok(());
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&line);
    text.push('\n');
    write(dir, &p, &text)
}

/// The file at `p`, or nothing when there is none.
fn read_or_empty(p: &Path) -> Result<String> {
    match fs::read_to_string(p) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e).at(p),
    }
}

/// Writes `text` to `p`, in `dir`, by rename.
fn write(dir: &Path, p: &Path, text: &str) -> Result<()> {
    let mut f = NamedTempFile::new_in(dir).at(dir)?;
    f.write_all(text.as_bytes()).at(f.path())?;
    f.persist(p).map_err(|e| e.error).at(p)?;
    Ok(())
}

/// `id` if it is a project's: a TypeID of type `project`.
fn check_id(id: &str) -> Result<()> {
    let t = MagicTypeId::from_str(id).map_err(|e| anyhow!("'{id}' is not a TypeID: {e}."))?;
    if t.prefix().as_str() != PREFIX {
        bail!("'{id}' is not a project's id, which starts with '{PREFIX}_'.");
    }
    Ok(())
}

/// `ai init`.
#[derive(clap::Args)]
pub struct Init {}

impl Init {
    /// Makes the current directory a project, printing its id.
    pub fn exec(self) -> Result<ExitCode> {
        let cwd = env::current_dir().context("Cannot read the current directory.")?;
        println!("{}", Project::init(&cwd)?.id);
        Ok(ExitCode::SUCCESS)
    }
}
