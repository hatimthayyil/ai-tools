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
    repo,
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
    /// Where it keeps its private store: see [`home`].
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
    /// file kept as written; with its private settings in its store, linked from its
    /// `ai.local.toml`, and that link and the store in its `.gitignore`.
    pub fn init(dir: &Path) -> Result<Project> {
        let p = dir.join(FILE);
        if config::is_user(&p) {
            bail!(
                "{} holds your own settings; a project's {FILE} sits in its top directory.",
                pretty(&p)
            );
        }
        config::check_link(&home(dir)?)?;
        let project = match Project::read(&p)? {
            Some(project) => project,
            None => mint(dir, &p)?,
        };
        ignore_untracked(dir)?;
        link_private(&project.home)?;
        Ok(project)
    }

    /// Its private store: a git repository of its own in its [home](Project::home).
    pub fn store(&self) -> PathBuf {
        self.home.join(STORE)
    }

    /// Commits its private store as one commit `msg`, making the store's repository first;
    /// nothing before the store exists or when nothing changed.
    pub fn commit(&self, msg: &str) -> Result<()> {
        let dir = self.store();
        if !dir.is_dir() {
            return Ok(());
        }
        self.repo()?;
        repo::commit(&dir, "ai", msg).map_err(|e| anyhow!("Cannot commit {}: {e:#}", pretty(&dir)))
    }

    /// Its `.ai` store, made a repository with its `.gitignore` unless it is one.
    pub fn repo(&self) -> Result<PathBuf> {
        let dir = self.home.join(STORE);
        if !repo::exists(&dir) {
            fs::create_dir_all(&dir).at(&dir)?;
            let p = dir.join(".gitignore");
            match fs::OpenOptions::new().write(true).create_new(true).open(&p) {
                Ok(mut f) => f.write_all(STORE_IGNORES.as_bytes()).at(&p)?,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e).at(&p),
            }
            repo::create(&dir)?;
        }
        Ok(dir)
    }
}

/// Where a project keeps its private data, below its [home](Project::home): a git repository of
/// its own, which the project's git ignores. The rest of `.ai` is the project's.
pub const STORE: &str = ".ai/local";
/// What the store's repository leaves out: locks and logs.
const STORE_IGNORES: &str = "*.lock\n*.log\n";

/// Runs `op`, then commits the store of the project at or above `cwd` as `msg`, whatever `op`
/// returned, so what it wrote before failing is kept. `op`'s error comes first.
pub fn committing<T>(cwd: &Path, msg: &str, op: impl FnOnce() -> Result<T>) -> Result<T> {
    let r = op();
    let c = Project::find(cwd).and_then(|p| p.map_or(Ok(()), |p| p.commit(msg)));
    match (r, c) {
        (Ok(v), Ok(())) => Ok(v),
        (Ok(_), Err(e)) => Err(e),
        (Err(e), Ok(())) => Err(e),
        (Err(e), Err(c)) => {
            eprintln!("{c}");
            Err(e)
        }
    }
}

/// Where the project at `dir` keeps its private store: in a linked git worktree, the same
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

/// Makes the private settings of the project at `home` if missing, and its [`LOCAL`] link to
/// them.
fn link_private(home: &Path) -> Result<()> {
    let file = config::private_file(home);
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).at(dir)?;
    }
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&file)
    {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e).at(&file),
    }
    let link = home.join(LOCAL);
    if fs::symlink_metadata(&link).is_ok() {
        return Ok(());
    }
    std::os::unix::fs::symlink(Path::new(STORE).join(FILE), &link).at(&link)
}

/// What a project keeps out of git: the link to its private settings, and its private store.
const UNTRACKED: [&str; 2] = [LOCAL, ".ai/local/"];

/// Adds each of [`UNTRACKED`] to the `.gitignore` in `dir` as `/<name>`, unless a line there
/// already names it, with or without its slashes.
fn ignore_untracked(dir: &Path) -> Result<()> {
    let p = dir.join(".gitignore");
    let mut text = read_or_empty(&p)?;
    let bare = |l: &str| l.trim().trim_matches('/').to_string();
    let have: Vec<String> = text.lines().map(bare).collect();
    let missing: Vec<&str> = UNTRACKED
        .into_iter()
        .filter(|n| !have.contains(&bare(n)))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    for n in missing {
        text.push('/');
        text.push_str(n);
        text.push('\n');
    }
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
