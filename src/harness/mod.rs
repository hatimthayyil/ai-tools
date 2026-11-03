//! `ai harness`: every harness's own files kept under `~/.ai/harness/<harness>/`, each linked
//! from where its harness looks for it, so the harness and `ai` keep finding it there.
//!
//! A folder moves only while nothing can write to it: no process of its harness runs and no
//! process has a file in it open. A move copies it, checks every entry's kind, mode, size and
//! hash against the original, swaps in the link, and reads it back through the link before
//! trusting it. The original stays beside the link as a backup until it has been kept for
//! [`KEEP`] with nothing written to it.

mod procs;
mod tree;

use std::{
    ffi::{CString, OsString},
    fmt::Display,
    fs::{self, DirBuilder},
    io::{self, Write},
    mem,
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, PermissionsExt, symlink},
    },
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use jiff::{SignedDuration, Timestamp};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::file::{AtPath, lock, pretty};

pub use procs::{Proc, blockers};
pub use tree::{Entry, Inventory};

/// The folder under `~/.ai` that holds every harness's files.
pub const DIR: &str = "harness";
/// The suffix of an original kept beside its link after a move.
pub const BACKUP: &str = "moved-to-ai";
/// The suffix of a move's record, beside the moved copy.
pub const RECORD: &str = "move.json";
/// The suffix of a copy being made, beside where the moved copy goes.
const INCOMING: &str = "incoming";
/// How long a backup is kept: time to notice a harness misbehaving on the moved copy while the
/// original still exists.
pub const KEEP: SignedDuration = SignedDuration::from_hours(14 * 24);
/// Free space a copy must leave on the disk of `~/.ai/harness`.
const MARGIN: u64 = 1 << 30;

/// Whether a harness keeps a folder or a single file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Dir,
    /// A file a harness writes in place, such as a SQLite database: linked only once it exists,
    /// since SQLite must not meet a link to nothing.
    File,
}

/// A harness and the folders and files of its that `ai` keeps.
pub struct Harness {
    pub name: &'static str,
    /// The environment variable that names the harness's folder, and that folder's path under
    /// the variable's value; an empty or relative value counts as unset.
    pub var: (&'static str, &'static str),
    /// The harness's folder under `$HOME` while the variable is unset.
    pub default: &'static str,
    /// Matches any of the first four arguments of the harness's processes.
    pub procs: &'static str,
    /// Its folders and files, under its folder; each is kept at the same path under
    /// `~/.ai/harness/<name>`.
    pub places: &'static [(Kind, &'static str)],
}

/// Every harness whose files `ai` keeps.
pub const HARNESSES: &[Harness] = &[
    Harness {
        name: "claude",
        var: ("CLAUDE_CONFIG_DIR", ""),
        default: ".claude",
        procs: r"(^|/)(claude|clauth)$",
        places: &[(Kind::Dir, "projects")],
    },
    Harness {
        name: "codex",
        var: ("CODEX_HOME", ""),
        default: ".codex",
        procs: r"(^|/)codex$",
        places: &[(Kind::Dir, "sessions"), (Kind::Dir, "archived_sessions")],
    },
    Harness {
        name: "pi",
        var: ("PI_CODING_AGENT_DIR", ""),
        default: ".pi/agent",
        procs: r"(^|/)\.?pi(-wrapped)?$",
        places: &[(Kind::Dir, "sessions")],
    },
    Harness {
        name: "dsh",
        var: ("DSH_HOME", ""),
        default: ".dsh",
        procs: r"(^|/)\.?dsh(-wrapped)?$|-dsh-[0-9]",
        places: &[(Kind::Dir, "sessions"), (Kind::Dir, "attachments")],
    },
    Harness {
        name: "gemini",
        var: ("GEMINI_CLI_HOME", ".gemini"),
        default: ".gemini",
        procs: r"(^|/)\.?gemini(-wrapped)?$|gemini-cli",
        places: &[(Kind::Dir, "tmp")],
    },
    Harness {
        name: "opencode",
        var: ("XDG_DATA_HOME", "opencode"),
        default: ".local/share/opencode",
        procs: r"(^|/)\.?opencode(-wrapped)?$|-opencode-[0-9]",
        places: &[(Kind::Dir, "storage"), (Kind::File, "opencode.db")],
    },
    Harness {
        name: "hermes",
        var: ("HERMES_HOME", ""),
        default: ".hermes",
        procs: r"(^|/)hermes$|hermes_cli|hermes-agent",
        places: &[(Kind::Dir, "sessions"), (Kind::File, "state.db")],
    },
];

/// Reads an environment variable.
pub type Vars = Box<dyn Fn(&str) -> Option<String>>;

/// What `ai harness` reads of the machine.
pub struct Env {
    /// The user's home, under which each harness keeps its folder by default.
    pub home: PathBuf,
    /// `~/.ai`.
    pub ai: PathBuf,
    /// The environment, for each harness's own variable.
    pub var: Vars,
    /// The folder listing processes: `/proc`.
    pub proc: PathBuf,
    /// The bytes free to the user on the disk that holds a path.
    pub free: fn(&Path) -> io::Result<u64>,
    pub now: Timestamp,
}

impl Env {
    /// This machine, as the running process sees it.
    pub fn current() -> Result<Env> {
        let home = std::env::home_dir().context("No home directory to find harness files in.")?;
        Ok(Env {
            ai: home.join(".ai"),
            home,
            var: Box::new(|k| std::env::var(k).ok()),
            proc: PathBuf::from("/proc"),
            free,
            now: Timestamp::now(),
        })
    }

    /// `~/.ai/harness`.
    pub fn base(&self) -> PathBuf {
        self.ai.join(DIR)
    }

    /// Every folder and file `ai` keeps, in [`HARNESSES`] order.
    pub fn places(&self) -> Vec<Place> {
        let base = self.base();
        HARNESSES
            .iter()
            .flat_map(|h| {
                let root = h.root(self);
                let base = base.join(h.name);
                h.places.iter().map(move |&(kind, rel)| {
                    let path = root.join(rel);
                    let label = match path.strip_prefix(&self.home) {
                        Ok(rest) => format!("~/{}", rest.display()),
                        Err(_) => path.display().to_string(),
                    };
                    Place {
                        harness: h,
                        kind,
                        path,
                        target: base.join(rel),
                        label,
                    }
                })
            })
            .collect()
    }
}

impl Harness {
    /// The harness's folder: its variable's value, else its default under `$HOME`.
    fn root(&self, env: &Env) -> PathBuf {
        let (var, under) = self.var;
        match (env.var)(var).filter(|v| Path::new(v).is_absolute()) {
            Some(v) if under.is_empty() => PathBuf::from(v),
            Some(v) => Path::new(&v).join(under),
            None => env.home.join(self.default),
        }
    }
}

/// One folder or file of a harness.
pub struct Place {
    pub harness: &'static Harness,
    pub kind: Kind,
    /// Where the harness looks for it.
    pub path: PathBuf,
    /// Where it is kept, under `~/.ai/harness`.
    pub target: PathBuf,
    /// Its path as the user reads it, `~` for the home.
    pub label: String,
}

/// Where a folder or file stands.
#[derive(Debug, PartialEq, Eq)]
pub enum State {
    /// The folder that should hold it does not exist.
    NoParent,
    /// It sits under `~/.ai/harness` already, as an agent's own harness folder does.
    Inside(PathBuf),
    /// A link to its place under `~/.ai/harness`.
    Linked(PathBuf),
    /// A link to somewhere else, with where it points.
    Elsewhere(PathBuf, PathBuf),
    /// A folder or file still holding the harness's data where the harness put it.
    Unmoved(PathBuf),
    /// Gone with its backup left: a move stopped between keeping the original and linking.
    CutShort(PathBuf),
    /// Neither it nor a backup exists.
    Missing(PathBuf),
}

impl Place {
    /// Where it really sits: its parent resolved, so the link replaces it there, and every path
    /// recorded to it, whether through `~` or through a linked parent, keeps resolving.
    fn real(&self) -> Option<PathBuf> {
        let parent = fs::canonicalize(self.path.parent()?).ok()?;
        Some(parent.join(self.path.file_name()?))
    }

    pub fn state(&self, base: Option<&Path>) -> Result<State> {
        let Some(real) = self.real() else {
            return Ok(State::NoParent);
        };
        if base.is_some_and(|b| real.starts_with(b)) {
            return Ok(State::Inside(real));
        }
        match fs::symlink_metadata(&real) {
            Ok(m) if m.file_type().is_symlink() => {
                let to = fs::read_link(&real).at(&real)?;
                Ok(if self.links_here(&real, &to) {
                    State::Linked(real)
                } else {
                    State::Elsewhere(real, to)
                })
            }
            Ok(_) => Ok(State::Unmoved(real)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(if exists(&backup(&real)) {
                State::CutShort(real)
            } else {
                State::Missing(real)
            }),
            Err(e) => Err(e).at(&real),
        }
    }

    fn links_here(&self, real: &Path, to: &Path) -> bool {
        to == self.target
            || fs::canonicalize(real)
                .is_ok_and(|r| fs::canonicalize(&self.target).is_ok_and(|t| r == t))
    }

    fn record(&self) -> PathBuf {
        suffixed(&self.target, RECORD)
    }
}

/// What a move did, kept beside the moved copy until its backup is pruned.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// Where the original was.
    pub from: PathBuf,
    pub moved: Timestamp,
    /// Whether the moved copy has been read back through the link.
    pub verified: bool,
    /// The original's entries, which the copy, the link and the backup must each match.
    pub entries: Inventory,
}

impl Record {
    pub fn load(p: &Path) -> Result<Option<Record>> {
        match fs::read(p) {
            Ok(b) => Ok(Some(
                serde_json::from_slice(&b)
                    .with_context(|| format!("{}: unreadable.", pretty(p)))?,
            )),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).at(p),
        }
    }

    fn save(&self, p: &Path) -> Result<()> {
        let dir = p.parent().context("A record needs a folder.")?;
        let mut f = tempfile::NamedTempFile::new_in(dir).at(dir)?;
        serde_json::to_writer(&mut f, self)?;
        f.as_file().sync_all().at(p)?;
        f.persist(p).map_err(|e| e.error).at(p)?;
        Ok(())
    }
}

/// Where `ai harness` reports: progress to `out`, problems to `err`.
pub struct Out<'a> {
    out: &'a mut dyn Write,
    err: &'a mut dyn Write,
    /// Whether something needs the user: the command exits with failure.
    pub failed: bool,
}

impl<'a> Out<'a> {
    pub fn new(out: &'a mut dyn Write, err: &'a mut dyn Write) -> Out<'a> {
        Out {
            out,
            err,
            failed: false,
        }
    }

    fn say(&mut self, s: impl Display) {
        let _ = writeln!(self.out, "{s}");
    }

    /// A problem that does not need the user.
    fn note(&mut self, s: impl Display) {
        let _ = writeln!(self.err, "{s}");
    }

    /// A problem that needs the user.
    fn warn(&mut self, s: impl Display) {
        self.note(s);
        self.failed = true;
    }
}

/// `ai harness`: keep every harness's own files under `~/.ai/harness`.
#[derive(Args, Debug)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    #[command(about = "Show where each harness folder stands.")]
    Status,
    #[command(
        about = "Link each harness folder that does not exist yet into ~/.ai/harness. Never moves data; home-manager runs it on every switch."
    )]
    Link,
    #[command(
        about = "Move each harness folder that still holds data into ~/.ai/harness, finish moves cut short, and delete backups kept 14 days that nothing wrote to. A folder whose harness runs, or that has a file open, stays where it is. Runs at boot."
    )]
    Move,
    #[command(
        about = "Delete every backup that nothing wrote to since its move, without waiting 14 days."
    )]
    Prune,
}

impl Cli {
    pub fn exec(self) -> Result<ExitCode> {
        let env = Env::current()?;
        let (mut o, mut e) = (io::stdout().lock(), io::stderr().lock());
        let mut out = Out::new(&mut o, &mut e);
        match self.command {
            Command::Status => status(&env, &mut out),
            Command::Link => link(&env, &mut out),
            Command::Move => run(&env, &mut out),
            Command::Prune => prune(&env, &mut out),
        }?;
        Ok(if out.failed {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        })
    }
}

/// Prints where each folder and file stands, and any backup beside it.
pub fn status(env: &Env, out: &mut Out) -> Result<()> {
    let base = fs::canonicalize(env.base()).ok();
    for p in env.places() {
        let label = &p.label;
        let line = match p.state(base.as_deref())? {
            State::NoParent => "its folder is missing".to_owned(),
            State::Inside(_) => "already under ~/.ai/harness".to_owned(),
            State::Linked(_) => format!("linked, {}", size(&p.target)),
            State::Elsewhere(_, to) => format!("links elsewhere: {}", to.display()),
            State::Unmoved(real) => format!("not moved, {} at {}", size(&real), real.display()),
            State::CutShort(_) => "a move was cut short; the next boot finishes it".to_owned(),
            State::Missing(_) => "missing".to_owned(),
        };
        out.say(format_args!("{label:<44} {line}"));
        if let Some(real) = p.real() {
            let b = backup(&real);
            if exists(&b) {
                let kept = match Record::load(&p.record()) {
                    Ok(Some(r)) if !r.verified => ", not yet read back".to_owned(),
                    Ok(Some(r)) => format!(", moved {}", r.moved.strftime("%Y-%m-%d")),
                    _ => String::new(),
                };
                out.say(format_args!(
                    "{:<44} backup {}, {}{kept}",
                    "",
                    b.display(),
                    size(&b)
                ));
            }
        }
    }
    Ok(())
}

/// Links each folder that does not exist yet, so its harness writes straight into
/// `~/.ai/harness`. Never moves data.
pub fn link(env: &Env, out: &mut Out) -> Result<()> {
    let (_lock, base) = prepare(env)?;
    for p in env.places() {
        if let Err(e) = link_one(&p, &base, out) {
            out.warn(format_args!("{e:#}"));
        }
    }
    Ok(())
}

/// Moves what can be moved, finishes moves cut short, then deletes backups kept [`KEEP`] that
/// nothing wrote to.
pub fn run(env: &Env, out: &mut Out) -> Result<()> {
    let (_lock, base) = prepare(env)?;
    for p in env.places() {
        if let Err(e) = move_one(env, &p, &base, out) {
            out.warn(format_args!("{e:#}"));
        }
    }
    for p in env.places() {
        if let Err(e) = prune_one(env, &p, &base, true, out) {
            out.warn(format_args!("{e:#}"));
        }
    }
    Ok(())
}

/// Deletes every backup that nothing wrote to since its move.
pub fn prune(env: &Env, out: &mut Out) -> Result<()> {
    let (_lock, base) = prepare(env)?;
    for p in env.places() {
        if let Err(e) = prune_one(env, &p, &base, false, out) {
            out.warn(format_args!("{e:#}"));
        }
    }
    Ok(())
}

/// The lock every changing command holds, and `~/.ai/harness` made private and resolved.
fn prepare(env: &Env) -> Result<(crate::file::Lock, PathBuf)> {
    let base = env.base();
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&base)
        .at(&base)?;
    fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).at(&base)?;
    let lock = lock(&env.ai.join("harness.lock"))?;
    Ok((lock, fs::canonicalize(&base).at(&base)?))
}

fn link_one(p: &Place, base: &Path, out: &mut Out) -> Result<()> {
    if let Some(parent) = p.path.parent() {
        fs::create_dir_all(parent).at(parent)?;
    }
    match p.state(Some(base))? {
        State::Inside(_) | State::Linked(_) => {}
        State::Elsewhere(real, to) => out.note(format_args!(
            "{} links to {}, not {}; left alone.",
            real.display(),
            to.display(),
            pretty(&p.target)
        )),
        State::Unmoved(real) => out.say(format_args!(
            "{} still holds {}; the next boot moves it.",
            real.display(),
            size(&real)
        )),
        State::CutShort(real) => out.say(format_args!(
            "A move of {} was cut short; the next boot finishes it.",
            real.display()
        )),
        State::NoParent => out.warn(format_args!("{}: its folder is missing.", p.label)),
        State::Missing(real) => {
            if p.kind == Kind::File && !exists(&p.target) {
                return Ok(());
            }
            if let Some(parent) = p.target.parent() {
                fs::create_dir_all(parent).at(parent)?;
            }
            if p.kind == Kind::Dir {
                fs::create_dir_all(&p.target).at(&p.target)?;
            }
            symlink(&p.target, &real).at(&real)?;
            out.say(format_args!(
                "Linked {} to {}.",
                real.display(),
                pretty(&p.target)
            ));
        }
    }
    Ok(())
}

fn move_one(env: &Env, p: &Place, base: &Path, out: &mut Out) -> Result<()> {
    match p.state(Some(base))? {
        State::NoParent | State::Missing(_) => link_one(p, base, out),
        State::Inside(_) => Ok(()),
        State::Elsewhere(..) => link_one(p, base, out),
        State::Linked(real) => match Record::load(&p.record())? {
            Some(r) if !r.verified => read_back(p, &real, r, out),
            _ => Ok(()),
        },
        State::CutShort(real) => relink(env, p, &real, out),
        State::Unmoved(real) => copy_over(env, p, &real, base, out),
    }
}

/// Moves a folder still holding data: refuses while anything could write to it, copies it,
/// checks the copy, swaps in the link and reads it back.
fn copy_over(env: &Env, p: &Place, real: &Path, base: &Path, out: &mut Out) -> Result<()> {
    let b = backup(real);
    if exists(&b) {
        out.warn(format_args!(
            "{} is left from an earlier move; check it and remove it first.",
            b.display()
        ));
        return Ok(());
    }
    // SQLite keeps committed pages in the -wal beside the database until a checkpoint; a
    // database moved without them loses those transactions.
    let wal = suffixed_raw(real, "-wal");
    if p.kind == Kind::File && fs::metadata(&wal).is_ok_and(|m| m.len() > 0) {
        out.warn(format_args!(
            "{} is not empty; open and quit {} once so it checkpoints.",
            wal.display(),
            p.harness.name
        ));
        return Ok(());
    }
    let rel = p.target.strip_prefix(env.base()).unwrap_or(&p.target);
    let watched = [real.to_path_buf(), base.join(rel)];
    if left(env, p, &watched, real, out)? {
        return Ok(());
    }
    let need = tree::size(real)?;
    let free = (env.free)(base).at(base)?;
    if need.saturating_add(MARGIN) > free {
        out.warn(format_args!(
            "{} needs {} on the disk of ~/.ai/harness, which has less than that and 1G free; nothing moved.",
            real.display(),
            tree::human(need)
        ));
        return Ok(());
    }
    let orig = tree::inventory(real)?;
    if let Some(parent) = p.target.parent() {
        fs::create_dir_all(parent).at(parent)?;
    }
    let incoming = suffixed(&p.target, INCOMING);
    if exists(&incoming) {
        tree::remove(&incoming)?;
    }
    let copied = if exists(&p.target) {
        if tree::inventory(&p.target)? != orig {
            out.warn(format_args!(
                "{} already holds other data than {}; nothing moved.",
                pretty(&p.target),
                real.display()
            ));
            return Ok(());
        }
        false
    } else {
        if let Err(e) = tree::copy(real, &incoming) {
            let _ = tree::remove(&incoming);
            return Err(e);
        }
        if tree::inventory(&incoming)? != orig {
            tree::remove(&incoming)?;
            out.warn(format_args!(
                "{} changed while it was copied; nothing moved.",
                real.display()
            ));
            return Ok(());
        }
        // The copy took time: a harness may have started since.
        if left(env, p, &watched, real, out)? {
            tree::remove(&incoming)?;
            return Ok(());
        }
        true
    };
    let record = Record {
        from: real.to_path_buf(),
        moved: env.now,
        verified: false,
        entries: orig,
    };
    record.save(&p.record())?;
    {
        let _quiet = Quiet::new();
        if copied {
            fs::rename(&incoming, &p.target).at(&p.target)?;
        }
        fs::rename(real, &b).at(real)?;
        symlink(&p.target, real).at(real)?;
    }
    read_back(p, real, record, out)
}

/// Whether a process could write to the folder, which then stays where it is: one of its
/// harness's, or one with a file open in it or where it goes.
fn left(env: &Env, p: &Place, watched: &[PathBuf], real: &Path, out: &mut Out) -> Result<bool> {
    let re = Regex::new(p.harness.procs).context("A harness's process pattern is invalid.")?;
    let busy = procs::blockers(&env.proc, &re, watched)?;
    if busy.is_empty() {
        return Ok(false);
    }
    let list: Vec<String> = busy
        .iter()
        .map(|b| format!("  {} {}", b.pid, b.args.join(" ")))
        .collect();
    out.say(format_args!(
        "Left {} in place for now; in use by:\n{}",
        real.display(),
        list.join("\n")
    ));
    Ok(true)
}

/// Finishes a move stopped between keeping the original and linking, when the moved copy holds
/// what the original held.
fn relink(env: &Env, p: &Place, real: &Path, out: &mut Out) -> Result<()> {
    let b = backup(real);
    let held = tree::inventory(&b)?;
    let record = match Record::load(&p.record())? {
        Some(r) => r,
        None => Record {
            from: real.to_path_buf(),
            moved: env.now,
            verified: false,
            entries: held.clone(),
        },
    };
    if !exists(&p.target) || held != record.entries || tree::inventory(&p.target)? != held {
        out.warn(format_args!(
            "{} is gone and {} does not match {}; resolve it by hand.",
            real.display(),
            b.display(),
            pretty(&p.target)
        ));
        return Ok(());
    }
    {
        let _quiet = Quiet::new();
        symlink(&p.target, real).at(real)?;
    }
    record.save(&p.record())?;
    read_back(p, real, record, out)
}

/// Reads the moved copy back through the link: on a match the move is done, else the original
/// goes back where it was.
fn read_back(p: &Place, real: &Path, mut record: Record, out: &mut Out) -> Result<()> {
    let b = backup(real);
    if tree::inventory(real)? == record.entries {
        record.verified = true;
        record.save(&p.record())?;
        out.say(format_args!(
            "Moved {} to {} ({}); the original stays at {} for 14 days.",
            real.display(),
            pretty(&p.target),
            size(&p.target),
            b.display()
        ));
        return Ok(());
    }
    {
        let _quiet = Quiet::new();
        fs::remove_file(real).at(real)?;
        fs::rename(&b, real).at(&b)?;
    }
    fs::remove_file(p.record()).at(&p.record())?;
    out.warn(format_args!(
        "{} did not read back the same through its link; put back as it was.",
        real.display()
    ));
    Ok(())
}

/// Deletes the backup beside a linked folder once nothing has written to it since its move, and,
/// when `aged`, once it has been kept [`KEEP`].
fn prune_one(env: &Env, p: &Place, base: &Path, aged: bool, out: &mut Out) -> Result<()> {
    let Some(real) = p.real() else {
        return Ok(());
    };
    let b = backup(&real);
    if !exists(&b) {
        return Ok(());
    }
    if !matches!(p.state(Some(base))?, State::Linked(_)) {
        out.warn(format_args!(
            "{} is not linked to {}; keeping {}.",
            real.display(),
            pretty(&p.target),
            b.display()
        ));
        return Ok(());
    }
    let Some(record) = Record::load(&p.record())? else {
        out.warn(format_args!(
            "No record of the move of {} at {}; keeping {}.",
            real.display(),
            pretty(&p.record()),
            b.display()
        ));
        return Ok(());
    };
    if !record.verified {
        out.note(format_args!(
            "{} is not yet read back; ai harness move does that before its backup can go.",
            real.display()
        ));
        return Ok(());
    }
    if aged && env.now < record.moved + KEEP {
        return Ok(());
    }
    if tree::inventory(&b)? != record.entries {
        out.warn(format_args!(
            "Keeping {}: something changed it after its move.",
            b.display()
        ));
        return Ok(());
    }
    tree::remove(&b)?;
    fs::remove_file(p.record()).at(&p.record())?;
    out.say(format_args!("Deleted {}.", b.display()));
    Ok(())
}

/// Where the original of `real` is kept after its move.
pub fn backup(real: &Path) -> PathBuf {
    suffixed(real, BACKUP)
}

/// `p` with `.<s>` appended to its last component.
fn suffixed(p: &Path, s: &str) -> PathBuf {
    suffixed_raw(p, &format!(".{s}"))
}

fn suffixed_raw(p: &Path, s: &str) -> PathBuf {
    let mut o = OsString::from(p.as_os_str());
    o.push(s);
    PathBuf::from(o)
}

/// Whether anything, a dangling link included, is at `p`.
fn exists(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}

/// What `p` holds, for a person to read; `?` when it can't be counted.
fn size(p: &Path) -> String {
    tree::size(p).map_or_else(|_| "?".to_owned(), tree::human)
}

/// The bytes free to the user on the disk holding `p`.
fn free(p: &Path) -> io::Result<u64> {
    let c = CString::new(p.as_os_str().as_bytes())?;
    // SAFETY: `c` is a valid C string; `s` is written by `statvfs` before it is read.
    unsafe {
        let mut s: libc::statvfs = mem::zeroed();
        if libc::statvfs(c.as_ptr(), &mut s) != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(s.f_bavail as u64 * s.f_frsize as u64)
    }
}

/// Termination signals held back while it lives, so a stop cannot leave a folder neither where
/// it was nor linked; one that arrived meanwhile takes effect when it is dropped.
struct Quiet(libc::sigset_t);

impl Quiet {
    fn new() -> Quiet {
        // SAFETY: both sets are initialised by `sigemptyset` or `pthread_sigmask` before use.
        unsafe {
            let mut set: libc::sigset_t = mem::zeroed();
            libc::sigemptyset(&mut set);
            for s in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
                libc::sigaddset(&mut set, s);
            }
            let mut old: libc::sigset_t = mem::zeroed();
            libc::pthread_sigmask(libc::SIG_BLOCK, &set, &mut old);
            Quiet(old)
        }
    }
}

impl Drop for Quiet {
    fn drop(&mut self) {
        // SAFETY: restores the mask saved by `new`.
        unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &self.0, std::ptr::null_mut());
        }
    }
}
