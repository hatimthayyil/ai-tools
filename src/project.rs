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
    config::{self, FILE},
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
        Ok(Some(Project { dir, id, name }))
    }

    /// The project at `dir`, minting an id into its `ai.toml` when it has none, the rest of the
    /// file kept as written.
    pub fn init(dir: &Path) -> Result<Project> {
        let p = dir.join(FILE);
        if config::is_user(&p) {
            bail!(
                "{} holds your own settings; a project's {FILE} sits in its top directory.",
                pretty(&p)
            );
        }
        if let Some(project) = Project::read(&p)? {
            return Ok(project);
        }
        let text = match fs::read_to_string(&p) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e).at(&p),
        };
        let mut doc: DocumentMut = text.parse().map_err(|e| anyhow!("{}: {e}", pretty(&p)))?;
        let table = doc.entry("project").or_insert(toml_edit::table());
        table["id"] = toml_edit::value(PREFIX.create_type_id::<V7>().to_string());
        let mut f = NamedTempFile::new_in(dir).at(dir)?;
        f.write_all(doc.to_string().as_bytes()).at(f.path())?;
        f.persist(&p).map_err(|e| e.error).at(&p)?;
        Project::read(&p)?.context("ai init wrote no project id.")
    }
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
