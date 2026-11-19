//! `ai convert`: converts what an earlier ai kept elsewhere to `~/ai`, where this ai reads it.
//! Each run first moves the user's memory there from `~/.ai/memory` ([`memory`]), once per
//! machine. It then converts what ai 0.17 kept in a project's `.ai/local`, its agents (store
//! format 4) and its private settings, to the project's folder of `~/ai`. That runs once per
//! project and machine, and never changes or removes `.ai/local`: what else that holds is the
//! project's private working data, and the old agents stay there until the user deletes them.
//!
//! A conversion builds the whole folder in `~/ai`'s git directory, checks it, makes it durable,
//! then moves it into place in one step; a conversion cut short before that step leaves
//! nothing, and is made again from the start. A record in `~/.ai/converted/` lets a later run
//! finish one cut short after the step.

mod memory;
mod old;
#[cfg(test)]
mod tests;

use std::{
    collections::BTreeMap,
    env, fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::ExitCode,
};

use ai_tools::{
    agent::{AGENTS, DIR, ITEMS, LINES, Store, THREADS},
    config::{FILE, Homes},
    error,
    file::{self, AtPath, pretty, sha256},
    project::{OLD_STORE, PROJECTS, Project},
    repo,
};
use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

/// Where this machine records each conversion, in `~/.ai`.
pub const CONVERTED: &str = "converted";

/// Where a conversion builds, in `~/ai`'s git directory, which no commit holds.
const BUILDING: &str = "ai-convert";

/// The file naming the project that holds a folder of `~/ai`.
const ID: &str = "id";

/// `ai convert`.
#[derive(clap::Args)]
#[command(
    after_help = "Every run first moves your memory from ~/.ai/memory, where ai kept it before, \
                  into ~/ai, once per machine: its whole history goes along, this machine's \
                  files of it stay in ~/.ai/memory, and the old repository is kept as \
                  ~/.ai/memory-converted.git, yours to delete once you have checked the \
                  conversion.\n\nRun it once per project on each machine, with the project's agent sessions \
                  closed. With no DIR it converts the current directory's project, and given one \
                  DIR it says what it found there. `ai convert ~/code/*/` converts every project \
                  there, telling of each, and counts in one closing line the directories that \
                  hold no project or nothing to convert. It never changes or removes .ai/local: \
                  once you have checked the conversion, the agents there are no longer read and \
                  are yours to delete."
)]
pub struct Cli {
    /// Build and check each conversion in a temporary directory, change nothing, and tell what
    /// would happen.
    #[arg(long)]
    dry_run: bool,
    /// Each project's directory, or one inside it; the current directory if none.
    #[arg(value_name = "DIR")]
    dirs: Vec<PathBuf>,
}

impl Cli {
    /// Converts the user's memory, then each directory's project, telling what it did; fails if
    /// any conversion failed.
    pub fn exec(self) -> Result<ExitCode> {
        let homes = Homes::user()?;
        let dirs = match self.dirs.is_empty() {
            true => vec![env::current_dir().context("Cannot read the current directory.")?],
            false => self.dirs,
        };
        let opts = Options {
            dry_run: self.dry_run,
        };
        let several = dirs.len() > 1;
        let (mut failed, mut not_projects, mut empty) = (false, 0, 0);
        match memory::convert(&homes, opts) {
            Ok(outcome) => {
                for line in memory::describe(&outcome) {
                    println!("{line}");
                }
            }
            Err(e) => {
                failed = true;
                eprintln!("memory: {}", error::line(&e));
            }
        }
        for dir in &dirs {
            match convert(dir, &homes, opts) {
                Ok(Outcome::NotAProject) if several => not_projects += 1,
                Ok(Outcome::Nothing { .. }) if several => empty += 1,
                Ok(outcome) => {
                    for line in describe(dir, &outcome) {
                        println!("{line}");
                    }
                }
                Err(e) => {
                    failed = true;
                    eprintln!("{}: {}", dir.display(), error::line(&e));
                }
            }
        }
        if let Some(line) = skipped(not_projects, empty) {
            println!("{line}");
        }
        Ok(match failed {
            true => ExitCode::FAILURE,
            false => ExitCode::SUCCESS,
        })
    }
}

/// How to run.
#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    /// Build and check in a temporary directory, change nothing, and tell what would happen.
    pub dry_run: bool,
}

/// What a conversion found to do, or did.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// No `ai.toml` with a project id at or above the directory.
    NotAProject,
    /// The project's `.ai/local` holds no agents and no private settings.
    Nothing { project: String },
    /// Converted earlier on this machine; a conversion cut short is finished.
    Done { project: String, folder: PathBuf },
    /// Converted, or with `dry_run` would be.
    Converted(Report),
}

/// What a conversion made of one project.
#[derive(Debug, PartialEq, Eq)]
pub struct Report {
    pub project: String,
    pub from: PathBuf,
    pub to: PathBuf,
    pub dry_run: bool,
    /// Whether `.ai/local` held private settings, now in the folder.
    pub settings: bool,
    pub threads: Vec<old::Thread>,
    /// The copies whose files the harness still holds, now pointed at.
    pub pointed: usize,
    /// The copies the harness no longer held whole, written back (with `dry_run`, to be).
    pub restored: Vec<old::Restore>,
}

/// This machine's record of a conversion, in [`CONVERTED`].
#[derive(Debug, Serialize, Deserialize)]
struct Record {
    from: PathBuf,
    to: PathBuf,
    at: Timestamp,
}

/// Converts the project at or above `dir` into `homes`.
pub fn convert(dir: &Path, homes: &Homes, opts: Options) -> Result<Outcome> {
    let Some(project) = Project::find(dir)? else {
        return Ok(Outcome::NotAProject);
    };
    let from = project.home.join(OLD_STORE);
    let agents = from.join(DIR);
    let settings = from.join(FILE);
    let have_settings = fs::metadata(&settings).is_ok_and(|m| m.len() > 0);
    if !agents.exists() && !have_settings {
        return Ok(Outcome::Nothing {
            project: project.name,
        });
    }
    let record = homes
        .machine
        .join(CONVERTED)
        .join(format!("{}.json", project.id));
    if let Some(folder) = project.held(homes)? {
        if converted_from(&record, &from)? {
            if !opts.dry_run {
                finish(&project, homes, &folder)?;
            }
            return Ok(Outcome::Done {
                project: project.name,
                folder,
            });
        }
        bail!(
            "{} already holds this project's agents and private settings, from another checkout \
             or machine, so {} was not converted. What it holds stays there.",
            pretty(&folder),
            pretty(&from)
        );
    }
    if agents.exists() {
        old::check(&agents)?;
    }
    let _locks = match agents.exists() {
        true => old::lock(&agents)?,
        false => Vec::new(),
    };
    let to = project.folder(homes)?;
    let work = workspace(homes, &project, opts)?;
    let staged = work.path().join(PROJECTS).join(&project.name);
    fs::create_dir_all(&staged).at(&staged)?;
    fs::write(staged.join(ID), format!("{}\n", project.id)).at(&staged)?;
    let private = staged.join(FILE);
    match fs::read(&settings) {
        Ok(b) => fs::write(&private, b).at(&private)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => fs::write(&private, "").at(&private)?,
        Err(e) => return Err(e).at(&settings),
    }
    let built = match agents.exists() {
        true => old::build(&agents, &staged.join(DIR), !opts.dry_run)?,
        false => old::Built::default(),
    };
    check(&project, homes, &agents, work.path(), &built, opts)?;
    let report = Report {
        project: project.name.clone(),
        from: from.clone(),
        to: to.clone(),
        dry_run: opts.dry_run,
        settings: have_settings,
        threads: built.threads,
        pointed: built.pointed,
        restored: built.restored,
    };
    if opts.dry_run {
        return Ok(Outcome::Converted(report));
    }
    sync(work.path())?;
    write_record(
        &record,
        &Record {
            from: from.clone(),
            to: to.clone(),
            at: Timestamp::now(),
        },
    )?;
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).at(parent)?;
    }
    file::rename_new(&staged, &to).with_context(|| {
        format!(
            "Cannot move the converted folder to {}; nothing was converted.",
            pretty(&to)
        )
    })?;
    finish(&project, homes, &to)?;
    Ok(Outcome::Converted(report))
}

/// Whether this machine's `record` says `from` was converted.
fn converted_from(record: &Path, from: &Path) -> Result<bool> {
    match fs::read(record) {
        Ok(b) => {
            let r: Record = serde_json::from_slice(&b)
                .with_context(|| format!("{} is corrupt", pretty(record)))?;
            Ok(r.from == from)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).at(record),
    }
}

/// Where to build: with `dry_run`, a temporary directory of the system's; else a new directory
/// in `~/ai`'s git directory, on the same file system as the folder it becomes, after removing
/// what a conversion of the project cut short left there.
fn workspace(homes: &Homes, project: &Project, opts: Options) -> Result<TempDir> {
    if opts.dry_run {
        return tempfile::Builder::new()
            .prefix("ai-convert-")
            .tempdir()
            .context("Cannot make a temporary directory.");
    }
    repo::ensure(&homes.repo)?;
    let base = repo::git_dir(&homes.repo).join(BUILDING);
    fs::create_dir_all(&base).at(&base)?;
    let prefix = format!("{}-", project.id);
    for e in fs::read_dir(&base).at(&base)? {
        let e = e.at(&base)?;
        if e.file_name().to_string_lossy().starts_with(&prefix) {
            fs::remove_dir_all(e.path()).at(&e.path())?;
        }
    }
    tempfile::Builder::new()
        .prefix(&prefix)
        .tempdir_in(&base)
        .at(&base)
}

/// Checks the folder built in `work` before it moves: each thread's files hold the old ones'
/// bytes, each record of a session's file finds the bytes it names, and the store reads whole
/// through `ai`'s own reader, with no session attached to two threads.
fn check(
    project: &Project,
    homes: &Homes,
    agents: &Path,
    work: &Path,
    built: &old::Built,
    opts: Options,
) -> Result<()> {
    let staged = work.join(PROJECTS).join(&project.name).join(DIR);
    for t in &built.threads {
        let (src, dst) = (
            agents.join(THREADS).join(&t.name),
            staged.join(THREADS).join(&t.name),
        );
        for f in [ITEMS, LINES, AGENTS] {
            let have = match fs::read(dst.join(f)) {
                Ok(b) => b,
                Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
                Err(e) => return Err(e).at(&dst.join(f)),
            };
            if have != old::joined(&src.join(f))? {
                bail!(
                    "Converting agent {}: {f} does not hold the old store's bytes. Nothing was \
                     converted.",
                    t.name
                );
            }
        }
    }
    let unwritten: Vec<&Path> = match opts.dry_run {
        true => built.restored.iter().map(|r| r.to.as_path()).collect(),
        false => Vec::new(),
    };
    let built_homes = Homes {
        repo: work.to_path_buf(),
        machine: work.join(".machine"),
        home: homes.home.clone(),
    };
    let store = Store::find_in(&built_homes, &project.home)?
        .context("The converted folder holds no project.")?;
    let mut attached: BTreeMap<(String, String), String> = BTreeMap::new();
    for thread in store.threads()? {
        let snap = thread.snapshot()?;
        let want = built
            .threads
            .iter()
            .find(|t| t.name == thread.name())
            .map_or(0, |t| t.items);
        let items = snap.items()?.len() as u64;
        if items != want {
            bail!(
                "Converting agent {}: {items} items read back, {want} expected. Nothing was \
                 converted.",
                thread.name()
            );
        }
        snap.agents()?;
        snap.all_lines()?;
        for b in snap.bindings()? {
            if b.detached.is_none()
                && let Some(other) = attached.insert(
                    (b.harness.clone(), b.session.clone()),
                    thread.name().to_string(),
                )
            {
                bail!(
                    "Converting: session {} is attached to agents {other} and {}. Nothing was \
                     converted.",
                    b.session,
                    thread.name()
                );
            }
        }
        for v in snap.versions()? {
            let path = &v.read.path;
            if unwritten.contains(&path.as_path()) {
                continue;
            }
            if prefix_hash(path, v.read.len)?.as_deref() != Some(v.read.sha256.as_str()) {
                bail!(
                    "Converting agent {}: {} no longer holds the {} bytes its record names. \
                     Nothing was converted.",
                    thread.name(),
                    pretty(path),
                    v.read.len
                );
            }
        }
    }
    Ok(())
}

/// The SHA-256 of the first `len` bytes of the file at `p`; `None` if it is gone or shorter.
fn prefix_hash(p: &Path, len: u64) -> Result<Option<String>> {
    let f = match fs::File::open(p) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).at(p),
    };
    let mut b = Vec::new();
    f.take(len).read_to_end(&mut b).at(p)?;
    Ok((b.len() as u64 == len).then(|| sha256(&b)))
}

/// Writes everything of the file system holding `dir` to the disk.
fn sync(dir: &Path) -> Result<()> {
    let f = fs::File::open(dir).at(dir)?;
    rustix::fs::syncfs(&f).map_err(io::Error::from).at(dir)
}

/// Writes `r` at `p` by rename, through to the disk.
fn write_record(p: &Path, r: &Record) -> Result<()> {
    let dir = p.parent().expect("a record sits in a directory");
    fs::create_dir_all(dir).at(dir)?;
    let mut f = tempfile::NamedTempFile::new_in(dir).at(dir)?;
    serde_json::to_writer_pretty(&mut f, r)?;
    f.as_file().sync_all().at(f.path())?;
    f.persist(p).map_err(|e| e.error).at(p)?;
    Ok(())
}

/// Links the project's `ai.local.toml` to the converted `folder`'s private settings and commits
/// the folder; once done, doing it again changes nothing.
fn finish(project: &Project, homes: &Homes, folder: &Path) -> Result<()> {
    project.claim(homes)?;
    let rel = folder
        .strip_prefix(&homes.repo)
        .expect("a project's folder is in the user's repository");
    repo::commit_paths(
        &homes.repo,
        &[rel],
        "ai",
        &format!(
            "ai convert: {}'s agents and private settings from {OLD_STORE}",
            project.name
        ),
    )
}

/// The closing line of a run over several directories, counting what it named no line for: the
/// directories that hold no ai project and the projects whose `.ai/local` holds nothing to convert;
/// `None` if neither.
fn skipped(not_projects: usize, empty: usize) -> Option<String> {
    let parts: Vec<String> = [
        match not_projects {
            0 => None,
            1 => Some("1 directory that holds no ai project".to_string()),
            n => Some(format!("{n} directories that hold no ai project")),
        },
        match empty {
            0 => None,
            1 => Some(format!("1 project with nothing to convert in {OLD_STORE}")),
            n => Some(format!(
                "{n} projects with nothing to convert in {OLD_STORE}"
            )),
        },
    ]
    .into_iter()
    .flatten()
    .collect();
    match parts.is_empty() {
        true => None,
        false => Some(format!("Skipped {}.", parts.join(", and "))),
    }
}

/// The sentence telling that `pointed` of the agents' session files stay where the harness keeps
/// them; `others` when the rest are written back, so that `pointed` is not all of them.
fn kept(agents: usize, pointed: usize, others: bool) -> String {
    let (whose, whose_title) = match agents {
        1 => ("its", "Its"),
        _ => ("their", "Their"),
    };
    let (files, stay, them) = match pointed {
        1 => ("session file", "stays", "it"),
        _ => ("session files", "stay", "them"),
    };
    let rest = format!("{stay} where the harness keeps {them}.");
    match (others, pointed) {
        (false, 1) => format!("{whose_title} only {files} {rest}"),
        (false, n) => format!("{whose_title} {n} {files} {rest}"),
        (true, 1) => format!("One of {whose} session files {rest}"),
        (true, n) => format!("{n} of {whose} session files {rest}"),
    }
}

/// One line per outcome, as `ai convert` prints it.
pub fn describe(dir: &Path, outcome: &Outcome) -> Vec<String> {
    match outcome {
        Outcome::NotAProject => vec![format!(
            "{}: not a project (no ai.toml with a project id), nothing to convert.",
            pretty(dir)
        )],
        Outcome::Nothing { project } => vec![format!(
            "{project}: {} holds no agents and no private settings, nothing to convert.",
            OLD_STORE
        )],
        Outcome::Done { project, folder } => vec![format!(
            "{project}: already converted to {}.",
            pretty(folder)
        )],
        Outcome::Converted(r) => {
            let verb = match r.dry_run {
                true => "would convert",
                false => "converted",
            };
            let items: u64 = r.threads.iter().map(|t| t.items).sum();
            let agents = r.threads.len();
            let what = match (agents, r.settings) {
                (0, true) => "private settings".to_string(),
                _ => format!(
                    "{agents} {} ({items} items){}",
                    if agents == 1 { "agent" } else { "agents" },
                    if r.settings {
                        " and private settings"
                    } else {
                        ""
                    },
                ),
            };
            let mut first = format!(
                "{}: {verb} {what} from {} to {}.",
                r.project,
                pretty(&r.from),
                pretty(&r.to),
            );
            if r.pointed > 0 {
                first.push(' ');
                first.push_str(&kept(agents, r.pointed, !r.restored.is_empty()));
            }
            let mut out = vec![first];
            for t in &r.restored {
                out.push(format!(
                    "  {} {} from its copy {}.",
                    match r.dry_run {
                        true => "Would restore",
                        false => "Restored",
                    },
                    pretty(&t.to),
                    pretty(&t.copy)
                ));
            }
            if !r.dry_run {
                out.push(format!(
                    "  {} is unchanged; its agents are no longer read, and once you have checked \
                     the conversion, are yours to delete.",
                    pretty(&r.from)
                ));
            }
            out
        }
    }
}
