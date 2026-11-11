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
    agent,
    config::{self, FILE, Homes, LOCAL},
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
/// user's, holding the id `ai init` minted. Its agents and private settings live in its
/// [folder](Project::folder) of the user's repository, `~/ai`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    /// The directory holding its `ai.toml`.
    pub dir: PathBuf,
    /// Where its checkouts meet: see [`home`].
    pub home: PathBuf,
    pub id: String,
    /// Its slug: `[project] name`, else the directory's name.
    pub name: String,
}

/// Where `~/ai` keeps every project's folder.
pub const PROJECTS: &str = "projects";

/// The file in a project's folder naming the project that holds the folder.
const ID: &str = "id";

impl Project {
    /// The project at or above `dir`; `None` without an `ai.toml` holding an id.
    pub fn find(dir: &Path) -> Result<Option<Project>> {
        match config::project_file(dir) {
            Some(p) => Project::at(&p),
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
    pub(crate) fn at(p: &Path) -> Result<Option<Project>> {
        let t = config::project_table(p)?.unwrap_or_default();
        let Some(id) = t.id else {
            return Ok(None);
        };
        check_id(&id).map_err(|e| anyhow!("{}: [project] id: {e}", pretty(p)))?;
        let dir = p.parent().unwrap_or(p).to_path_buf();
        let name = name_of(&dir, t.name);
        Ok(Some(Project {
            home: home(&dir)?,
            dir,
            id,
            name,
        }))
    }

    /// [`Project::init_in`] with the user's `~/ai`.
    pub fn init(dir: &Path) -> Result<Project> {
        Project::init_in(dir, &Homes::user()?)
    }

    /// The project at `dir`, minting an id into its `ai.toml` when it has none, the rest of the
    /// file kept as written; its [folder](Project::claim) in `homes.repo` claimed, its private
    /// settings made there and linked from its `ai.local.toml`, that link in its `.gitignore`.
    pub fn init_in(dir: &Path, homes: &Homes) -> Result<Project> {
        let p = dir.join(FILE);
        if config::is_user(&p) {
            bail!(
                "{} holds your own settings; a project's {FILE} sits in its top directory.",
                pretty(&p)
            );
        }
        let found = Project::at(&p)?;
        let at = home(dir)?;
        let old = at.join(OLD_STORE);
        if old.exists() {
            bail!(
                "{} holds this project's agents as an older ai kept them. Run `ai agent list` on \
                 a terminal to move them to {}.",
                pretty(&old),
                pretty(&homes.repo)
            );
        }
        let folder = match &found {
            Some(project) => project.folder(homes)?,
            None => free(
                homes,
                &name_of(dir, config::project_table(&p)?.and_then(|t| t.name)),
                &format!("the project at {}", pretty(dir)),
                &p,
            )?,
        };
        config::check_link(&at, &folder.join(FILE))?;
        let project = match found {
            Some(project) => project,
            None => mint(dir, &p)?,
        };
        ignore_untracked(dir)?;
        project.claim(homes)?;
        Ok(project)
    }

    /// The folder of `projects/` in `homes.repo` that its id holds, whatever its name; `None`
    /// until [`Project::claim`] makes one. Every checkout and clone of the project finds the
    /// same folder.
    pub fn held(&self, homes: &Homes) -> Result<Option<PathBuf>> {
        let projects = homes.repo.join(PROJECTS);
        match fs::read_dir(&projects) {
            Ok(entries) => {
                for e in entries {
                    let p = e.at(&projects)?.path();
                    if read_id(&p)?.as_deref() == Some(self.id.as_str()) {
                        return Ok(Some(p));
                    }
                }
                Ok(None)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).at(&projects),
        }
    }

    /// Its folder in `homes.repo`: the one it [holds](Project::held), else `projects/<name>`,
    /// which [`Project::claim`] makes. An error if another project holds the name.
    pub fn folder(&self, homes: &Homes) -> Result<PathBuf> {
        match self.held(homes)? {
            Some(p) => Ok(p),
            None => free(
                homes,
                &self.name,
                &format!("this project ({}, at {})", self.id, pretty(&self.dir)),
                &self.dir.join(FILE),
            ),
        }
    }

    /// Its private settings: the `ai.toml` in the folder it [holds](Project::held); `None`
    /// while it holds none, since only [`Project::claim`] makes them.
    pub fn private(&self, homes: &Homes) -> Result<Option<PathBuf>> {
        Ok(self.held(homes)?.map(|f| f.join(FILE)))
    }

    /// Makes its [folder](Project::folder) and claims it with its id; makes its private settings
    /// there if missing and links them from its `ai.local.toml`. Makes `homes.repo` a git
    /// repository first unless it is one. Returns the folder.
    pub fn claim(&self, homes: &Homes) -> Result<PathBuf> {
        repo::ensure(&homes.repo)?;
        let _lock = repo::lock(&homes.repo)?;
        let named = self.folder(homes)?;
        fs::create_dir_all(&named).at(&named)?;
        let id = named.join(ID);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&id)
        {
            Ok(mut f) => f.write_all(format!("{}\n", self.id).as_bytes()).at(&id)?,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e).at(&id),
        }
        let private = named.join(FILE);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&private)
        {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e).at(&private),
        }
        link(&self.home.join(LOCAL), &private)?;
        Ok(named)
    }
}

/// `projects/<name>` in `homes.repo`, for `whom`, a project that holds no folder yet and whose
/// `ai.toml` is `file`; an error naming both projects if another holds the name.
fn free(homes: &Homes, name: &str, whom: &str, file: &Path) -> Result<PathBuf> {
    check_folder_name(name)?;
    let projects = homes.repo.join(PROJECTS);
    let named = projects.join(name);
    if let Some(other) = read_id(&named)? {
        bail!(
            "Another project ({other}) holds the name {name} in {}, so {whom} can't take it. \
             Give this one another name: set [project] name in {}.",
            pretty(&projects),
            pretty(file)
        );
    }
    Ok(named)
}

/// The name of the project at `dir`: `name`, its `[project] name`, else the directory's name.
fn name_of(dir: &Path, name: Option<String>) -> String {
    name.unwrap_or_else(|| {
        dir.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    })
}

/// Where an older ai kept a project's agents and private settings, in its [home](Project::home).
pub const OLD_STORE: &str = ".ai/local";

/// `name` if it can name a project's folder: letters, digits, `.`, `-` and `_`, but not `.` or
/// `..`.
fn check_folder_name(name: &str) -> Result<()> {
    let ok = !matches!(name, "" | "." | "..")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    if !ok {
        bail!(
            "The project name '{name}' cannot name a folder: use letters, digits, '.', '-' and \
             '_'. Set [project] name in the project's {FILE}."
        );
    }
    Ok(())
}

/// The id of the project holding the folder `dir`; `None` if none holds it.
fn read_id(dir: &Path) -> Result<Option<String>> {
    let p = dir.join(ID);
    match fs::read_to_string(&p) {
        Ok(t) => Ok(Some(t.trim().to_string())),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(e) => Err(e).at(&p),
    }
}

/// Makes `link`, in a project's [home](Project::home), a symbolic link to `target`, replacing a
/// link to anything else; refuses a file of its own there.
fn link(link: &Path, target: &Path) -> Result<()> {
    if fs::read_link(link).ok().as_deref() == Some(target) {
        return Ok(());
    }
    let dir = link.parent().unwrap_or(Path::new("."));
    config::check_link(dir, target)?;
    let tmp = dir.join(format!(".{LOCAL}.{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target, &tmp).at(&tmp)?;
    fs::rename(&tmp, link).at(link)
}

/// Where the checkouts of the project at `dir` meet, holding the link to its private settings:
/// in a linked git worktree, the same directory in the main checkout; else `dir`.
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
    Project::at(p)?.context("ai init wrote no project id.")
}

/// What a project keeps out of git: the link to its private settings.
const UNTRACKED: [&str; 1] = [LOCAL];

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
    /// Makes the current directory a project, printing its id; on a terminal, first offers to
    /// move agents an older ai kept in the project.
    pub fn exec(self) -> Result<ExitCode> {
        let cwd = env::current_dir().context("Cannot read the current directory.")?;
        agent::convert::offer(&cwd)?;
        println!("{}", Project::init(&cwd)?.id);
        Ok(ExitCode::SUCCESS)
    }
}
