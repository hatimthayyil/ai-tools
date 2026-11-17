//! `ai backup`: every harness's transcripts backed up with restic, which `ai` runs as a program.
//!
//! A run backs up each harness's folders ([`harness::HARNESSES`], under each harness's default
//! folder) and the folders `[backup] folders` names, each where it really sits, into the local
//! repository as one snapshot of this machine ([`host`]). A harness's database goes in as a copy
//! made first ([`db`]), never as the file its harness is writing. With `[backup.remote]`, the
//! run then copies every snapshot the remote repository lacks into it. Nothing is ever forgotten
//! or pruned.
//!
//! What a reader took of a harness's file is read back from the file while it still holds it,
//! else from the repositories ([`read`]); `ai backup threads` tells where the bytes every thread
//! read are ([`threads`]).
//!
//! One run goes at a time, under `~/.ai/backup.lock`; a second waits for the first. Each run
//! leaves what it did in `~/.ai/backup.json`, so `ai backup status` tells of a failed run that
//! had no terminal to say so on: one `ai backup watch` made, or one a session's end started. One
//! `ai backup watch` goes at a time, holding `~/.ai/backup-watch.lock` while it lasts; `status`
//! tells by that lock whether one is running.

pub mod config;
pub mod db;
pub mod harness;
pub mod read;
pub mod restic;
pub mod threads;

use std::{
    collections::BTreeSet,
    convert::Infallible,
    ffi::CStr,
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{self, ExitCode, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Subcommand};
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

pub use self::config::Config;
use self::{
    config::Remote,
    harness::{HARNESSES, Kind},
    restic::{Password, Repo},
};
use crate::{
    command::{self, LIMIT},
    config::Homes,
    error,
    file::{self, AtPath, human, pretty},
};

/// The lock a run holds, in `~/.ai`.
const LOCK: &str = "backup.lock";
/// The lock `ai backup watch` holds while it lasts, in `~/.ai`.
const WATCH_LOCK: &str = "backup-watch.lock";
/// The local repository's password, in `~/.ai`, while no `password-command` names another.
const KEY: &str = "backup.key";
/// The folder of the databases' copies, in `~/.ai`.
const STAGING: &str = "backup-staging";
/// What the last run did, in `~/.ai`.
const STATE: &str = "backup.json";

#[derive(Args)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Back up what every harness wrote since the last run, once.")]
    Run,
    #[command(about = "Run again and again, [backup] every apart; its service runs this.")]
    Watch,
    #[command(about = "What is backed up, where to, and how the last run went.")]
    Status,
    #[command(about = "Have restic check the local repository, and the remote one if set.")]
    Check {
        #[arg(long, help = "Read and verify every byte the repositories hold.")]
        read_data: bool,
    },
    #[command(
        about = "Where the bytes every thread in ~/ai read of harness files are: backed up, not yet, or lost.",
        after_help = "A file counts once for each thread that read it. Bytes a file still holds are read and compared in full; bytes it no longer holds are read back from the local repository, then the remote one. It fails if any are lost."
    )]
    Threads,
}

impl Cli {
    pub fn exec(self) -> Result<ExitCode> {
        let homes = Homes::user()?;
        let mut out = io::stdout().lock();
        match self.command {
            Command::Run => run(&homes, &mut out).map(|()| ExitCode::SUCCESS),
            Command::Watch => watch(&homes, &mut out, &mut io::stderr()).map(|n| match n {}),
            Command::Status => status(&homes, &mut out),
            Command::Check { read_data } => check(&homes, &mut out, read_data),
            Command::Threads => threads::check(&homes, &mut out, &mut io::stderr()),
        }
    }
}

/// What the last run did: `~/.ai/backup.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct State {
    /// When the last run ended.
    pub ran: Option<Timestamp>,
    /// When the last run that met no problem ended.
    pub ok: Option<Timestamp>,
    /// The last run's problem.
    pub error: Option<String>,
    /// The newest snapshot a run made in the local repository.
    pub snapshot: Option<String>,
    /// The last copy to a remote repository that went through.
    pub copied: Option<Copied>,
}

/// A copy to a remote repository that went through.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Copied {
    /// The repository, as [`restic::shown`] prints it.
    pub remote: String,
    /// The local repository's newest snapshot then.
    pub snapshot: Option<String>,
}

impl State {
    /// The state `file` holds; the default without the file.
    pub fn load(file: &Path) -> Result<State> {
        let text = match fs::read(file) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(State::default()),
            Err(e) => return Err(e).at(file),
        };
        serde_json::from_slice(&text).map_err(|e| {
            anyhow!(
                "{} is no record of a run ({e}). Delete it: the next run writes it again.",
                pretty(file)
            )
        })
    }

    fn save(&self, file: &Path) -> Result<()> {
        let dir = file.parent().unwrap_or(Path::new(""));
        let mut tmp = tempfile::NamedTempFile::new_in(dir).at(dir)?;
        serde_json::to_writer_pretty(&mut tmp, self)?;
        tmp.write_all(b"\n").at(tmp.path())?;
        tmp.persist(file).map_err(|e| e.error).at(file)?;
        Ok(())
    }
}

/// A folder to back up, or a database to copy and back up.
struct Place {
    /// Where its harness or the settings name it.
    path: PathBuf,
    /// Whether it is a database.
    db: bool,
    /// Whether `[backup] folders` names it.
    named: bool,
}

/// Everything to back up: each harness's places under its default folder, in [`HARNESSES`]
/// order, then the folders the settings name. No environment variable counts, so the service
/// and a command run from any shell back up the same folders.
fn places(homes: &Homes, cfg: &Config) -> Vec<Place> {
    let harnesses = HARNESSES.iter().flat_map(|h| {
        h.places.iter().map(|&(kind, rel)| Place {
            path: homes.home.join(h.default).join(rel),
            db: kind == Kind::File,
            named: false,
        })
    });
    let named = cfg.folders.iter().map(|f| Place {
        path: f.clone(),
        db: false,
        named: true,
    });
    harnesses.chain(named).collect()
}

/// Where `p` really sits, its links resolved; `None` if it does not exist.
fn real(p: &Path) -> Result<Option<PathBuf>> {
    match fs::canonicalize(p) {
        Ok(r) => Ok(Some(r)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).at(p),
    }
}

/// This machine's name, which its snapshots carry and a record of what was read of a file
/// here names.
pub fn host() -> Result<String> {
    let mut buf = [0u8; 256];
    // SAFETY: gethostname writes at most the length given into the buffer, which it borrows
    // for the call alone.
    let r = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len() - 1) };
    if r != 0 {
        return Err(io::Error::last_os_error()).context("Cannot read this machine's name.");
    }
    let name = CStr::from_bytes_until_nul(&buf)
        .ok()
        .and_then(|c| c.to_str().ok())
        .filter(|n| !n.is_empty())
        .context("This machine's name is not text.")?;
    Ok(name.to_owned())
}

/// The password the command of the setting `key` prints: what it printed, without the blanks
/// around it, as restic reads one.
fn password(key: &str, command: &str) -> Result<Password> {
    let printed = command::printed(key, command, LIMIT)?;
    match printed.trim() {
        "" => bail!("`{key} = {command:?}` printed no password."),
        p => Ok(Password::Given(p.to_owned())),
    }
}

/// The local repository of `cfg`, and what opens it; runs its `password-command`.
fn local(homes: &Homes, cfg: &Config) -> Result<Repo> {
    let password = match &cfg.password_command {
        Some(c) => password("[backup] password-command", c)?,
        None => Password::File(homes.machine.join(KEY)),
    };
    Ok(Repo::local(&cfg.repository, password))
}

/// Whether the local repository of `cfg` has been made.
fn made(cfg: &Config) -> bool {
    cfg.repository.join("config").is_file()
}

/// The remote repository `r`, and what opens it; runs its `password-command` and `env-command`.
fn remote(r: &Remote) -> Result<Repo> {
    Ok(Repo {
        address: r.repository.clone().into(),
        password: password("[backup.remote] password-command", &r.password_command)?,
        env: match &r.env_command {
            Some(c) => variables(c)?,
            None => Vec::new(),
        },
    })
}

/// The variables `command` prints ([`command::printed`]): a `NAME=value` on each line, the value
/// as it stands but for one pair of quotes around it; blank lines and lines that start with `#`
/// are skipped. Its errors name the command, never what it printed: those are secrets.
fn variables(command: &str) -> Result<Vec<(String, String)>> {
    const KEY: &str = "[backup.remote] env-command";
    let says = || format!("`{KEY} = {command:?}`");
    let text = command::printed(KEY, command, LIMIT)?;
    let mut vars = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let pair = line.split_once('=').filter(|(name, _)| {
            !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        });
        let Some((name, value)) = pair else {
            bail!(
                "{} printed, on line {}, something that is no NAME=value.",
                says(),
                n + 1
            );
        };
        let quoted = ['"', '\'']
            .into_iter()
            .find_map(|q| value.strip_prefix(q).and_then(|v| v.strip_suffix(q)));
        vars.push((name.to_owned(), quoted.unwrap_or(value).to_owned()));
    }
    Ok(vars)
}

/// Makes the local repository if there is none, and with no `password-command`, its key file
/// first: 32 random bytes in hex, readable by the user alone.
fn ensure(homes: &Homes, cfg: &Config, repo: &Repo, out: &mut dyn Write) -> Result<()> {
    let key = homes.machine.join(KEY);
    let keyless = cfg.password_command.is_none() && !key.try_exists().at(&key)?;
    if made(cfg) {
        if keyless {
            bail!(
                "{} is gone, and it holds the password of the repository {}. Put it back, or \
                 name the command that prints the password in [backup] password-command.",
                pretty(&key),
                pretty(&cfg.repository)
            );
        }
        return Ok(());
    }
    if keyless {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).context("Cannot make the repository's key.")?;
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&key)
            .at(&key)?;
        writeln!(f, "{hex}").at(&key)?;
        f.sync_all().at(&key)?;
    }
    restic::init(repo, None)?;
    writeln!(out, "Made the repository {}.", pretty(&cfg.repository))?;
    Ok(())
}

/// The real paths a run backs up, the databases among the places copied first into the staging
/// folder, which is one of the paths. A named folder that is missing and a database that can't
/// be copied go to `problems`, and the rest goes on.
fn gather(homes: &Homes, cfg: &Config, problems: &mut Vec<String>) -> Result<Vec<PathBuf>> {
    let staging = homes.machine.join(STAGING);
    let repository = real(&cfg.repository)?.unwrap_or_else(|| cfg.repository.clone());
    let mut paths = BTreeSet::new();
    for place in places(homes, cfg) {
        let Some(at) = real(&place.path)? else {
            if place.named {
                problems.push(format!(
                    "[backup] folders names {}, which does not exist.",
                    pretty(&place.path)
                ));
            }
            continue;
        };
        if at.starts_with(&repository) || repository.starts_with(&at) {
            bail!(
                "{} holds or sits in the repository {}, so it can't be backed up into it. Set \
                 [backup] repository to a folder apart from it.",
                pretty(&place.path),
                pretty(&cfg.repository)
            );
        }
        match place.db {
            false => {
                paths.insert(at);
            }
            true => {
                if let Err(e) = db::stage(&staging, &at) {
                    problems.push(error::line(&e));
                }
            }
        }
    }
    paths.extend(real(&staging)?);
    Ok(paths.into_iter().collect())
}

/// Backs up once, waiting for a run in progress to end first, and leaves what it did in the
/// state file. An error if anything went wrong, after doing all that could be done.
pub fn run(homes: &Homes, out: &mut dyn Write) -> Result<()> {
    fs::create_dir_all(&homes.machine).at(&homes.machine)?;
    let lock = homes.machine.join(LOCK);
    let _lock = match file::try_lock(&lock)? {
        Some(l) => l,
        None => {
            writeln!(out, "Waiting for another backup run to end.")?;
            file::lock(&lock)?
        }
    };
    let file = homes.machine.join(STATE);
    let mut state = State::load(&file)?;
    let result = attempt(homes, &mut state, out);
    let now = Timestamp::now();
    state.ran = Some(now);
    match &result {
        Ok(()) => {
            state.ok = Some(now);
            state.error = None;
        }
        Err(e) => state.error = Some(error::line(e)),
    }
    state.save(&file)?;
    result
}

fn attempt(homes: &Homes, state: &mut State, out: &mut dyn Write) -> Result<()> {
    let cfg = Config::load(homes)?;
    let host = host()?;
    let local = local(homes, &cfg)?;
    ensure(homes, &cfg, &local, out)?;
    let mut problems = Vec::new();
    let paths = gather(homes, &cfg, &mut problems)?;
    if paths.is_empty() {
        writeln!(out, "Nothing to back up: no harness folder exists.")?;
    } else {
        // A snapshot holds its folders' parents too, whose times change with anything written
        // beside them, so restic's own --skip-if-unchanged would skip next to nothing.
        let would = restic::backup(&local, &host, &paths, true)?;
        if would.changed == 0 {
            writeln!(out, "Nothing changed since the last snapshot.")?;
            problems.extend(would.unread);
        } else {
            let did = restic::backup(&local, &host, &paths, false)?;
            let id = did.snapshot.context("restic backup named no snapshot.")?;
            writeln!(
                out,
                "Snapshot {}: {} new or changed, {} added to {}.",
                short(&id),
                match did.changed {
                    1 => "1 file".to_owned(),
                    n => format!("{n} files"),
                },
                human(did.added),
                pretty(&cfg.repository)
            )?;
            state.snapshot = Some(id);
            problems.extend(did.unread);
        }
    }
    if let Some(r) = &cfg.remote {
        let now = Copied {
            remote: restic::shown(r.repository.as_ref()),
            snapshot: state.snapshot.clone(),
        };
        if state.copied.as_ref() != Some(&now) {
            match copy(r, &local, &now.remote, out) {
                Ok(()) => state.copied = Some(now),
                Err(e) => problems.push(error::line(&e)),
            }
        }
    }
    match problems.is_empty() {
        true => Ok(()),
        false => bail!("{}", problems.join("\n")),
    }
}

/// Copies every snapshot of `local` that the remote repository `r`, shown as `name`, lacks into
/// it, making it first if there is none.
fn copy(r: &Remote, local: &Repo, name: &str, out: &mut dyn Write) -> Result<()> {
    let to = remote(r)?;
    if !restic::exists(&to)? {
        restic::init(&to, Some(local))?;
        writeln!(out, "Made the remote repository {name}.")?;
    }
    restic::copy(&to, local)?;
    writeln!(out, "Copied every snapshot to {name}.")?;
    Ok(())
}

/// A snapshot's id as restic shortens it.
fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// Runs again and again, `[backup] every` apart, read anew before each run. A run that fails is
/// told on `err` and the next still comes; broken settings end the watch.
pub fn watch(homes: &Homes, out: &mut dyn Write, err: &mut dyn Write) -> Result<Infallible> {
    fs::create_dir_all(&homes.machine).at(&homes.machine)?;
    let lock = homes.machine.join(WATCH_LOCK);
    // `status` takes a free lock for an instant to test it; waiting a moment keeps that from
    // turning a watch away.
    let Some(_watching) = file::lock_before(&lock, Instant::now() + Duration::from_secs(1))? else {
        bail!(
            "Another `ai backup watch` is running: it holds {}.",
            pretty(&lock)
        );
    };
    loop {
        let every = Config::load(homes)?.every;
        if let Err(e) = run(homes, out) {
            writeln!(err, "{}", error::line(&e))?;
        }
        thread::sleep(every);
    }
}

/// Starts a run of its own, in the background, if `[backup] on-session-end` asks for one and the
/// local repository has been made: backups begin with the first `ai backup run`, not with a
/// session. The run is not waited for; `ai backup status` tells how it went.
pub fn at_session_end(homes: &Homes) -> Result<()> {
    let cfg = Config::load(homes)?;
    if !cfg.on_session_end || !made(&cfg) {
        return Ok(());
    }
    let ai = std::env::current_exe().context("Cannot find this ai to start a backup run.")?;
    process::Command::new(ai)
        .args(["backup", "run"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .context("Cannot start a backup run.")?;
    Ok(())
}

/// Prints what is backed up, where to, and how the last run went; fails if it went wrong.
pub fn status(homes: &Homes, out: &mut dyn Write) -> Result<ExitCode> {
    let cfg = Config::load(homes)?;
    let state = State::load(&homes.machine.join(STATE))?;
    let staging = homes.machine.join(STAGING);
    writeln!(
        out,
        "Repository: {}{}",
        pretty(&cfg.repository),
        match made(&cfg) {
            true => "",
            false => " (not made yet: `ai backup run` makes it)",
        }
    )?;
    match &cfg.remote {
        Some(r) => writeln!(out, "Remote: {}", restic::shown(r.repository.as_ref()))?,
        None => writeln!(out, "Remote: none")?,
    }
    let every = SignedDuration::try_from(cfg.every).context("[backup] every is out of range.")?;
    let watching = file::held(&homes.machine.join(WATCH_LOCK))?;
    writeln!(out, "Runs: {}", runs(every, watching, cfg.on_session_end))?;
    writeln!(out, "Backs up:")?;
    for place in places(homes, &cfg) {
        let note = match (real(&place.path)?, place.db) {
            (None, _) => " (missing)".to_owned(),
            (Some(at), true) => format!(" (as its copy {})", pretty(&db::staged(&staging, &at))),
            (Some(at), false) if at != place.path => format!(" (at {})", pretty(&at)),
            (Some(_), false) => String::new(),
        };
        writeln!(out, "  {}{note}", pretty(&place.path))?;
    }
    if file::held(&homes.machine.join(LOCK))? {
        writeln!(out, "A run is in progress.")?;
    }
    let Some(ran) = state.ran else {
        writeln!(out, "No run yet.")?;
        return Ok(ExitCode::SUCCESS);
    };
    let snapshot = match &state.snapshot {
        Some(id) => format!("newest snapshot {}", short(id)),
        None => "no snapshot yet".to_owned(),
    };
    let copied = match (&cfg.remote, &state.copied) {
        (Some(r), Some(c))
            if c.remote == restic::shown(r.repository.as_ref()) && c.snapshot == state.snapshot =>
        {
            ", copied to the remote"
        }
        (Some(_), _) => ", not yet copied to the remote",
        (None, _) => "",
    };
    writeln!(out, "Last run: {ran:.0}, {snapshot}{copied}.")?;
    match &state.error {
        None => Ok(ExitCode::SUCCESS),
        Some(e) => {
            match state.ok {
                Some(ok) => writeln!(out, "It failed; the last run without a problem: {ok:.0}.")?,
                None => writeln!(out, "It failed; no run has gone without a problem yet.")?,
            }
            bail!("{e}")
        }
    }
}

/// When runs come, as `status` tells it: `every` apart under a watch while one is `watching`,
/// and at each session's end with `on_session_end`.
fn runs(every: SignedDuration, watching: bool, on_session_end: bool) -> String {
    let watch = "`ai backup watch`";
    match (watching, on_session_end) {
        (true, true) => {
            format!("every {every:#} under {watch} (running), and at each session's end")
        }
        (true, false) => format!("every {every:#} under {watch} (running)"),
        (false, ends) => format!(
            "{}; {watch} (every {every:#}) isn't running: start it, or enable \
             services.ai-tools.backup in Home Manager",
            match ends {
                true => "at each session's end and on `ai backup run`",
                false => "on `ai backup run` alone",
            }
        ),
    }
}

/// Has restic check the local repository, then the remote one if set, waiting for a run in
/// progress to end first. Fails if either check does.
pub fn check(homes: &Homes, out: &mut dyn Write, read_data: bool) -> Result<ExitCode> {
    let cfg = Config::load(homes)?;
    if !made(&cfg) {
        bail!(
            "No repository at {} to check: `ai backup run` makes it.",
            pretty(&cfg.repository)
        );
    }
    let _lock = file::lock(&homes.machine.join(LOCK))?;
    let mut repos = vec![(pretty(&cfg.repository), local(homes, &cfg)?)];
    if let Some(r) = &cfg.remote {
        repos.push((restic::shown(r.repository.as_ref()), remote(r)?));
    }
    let mut ok = true;
    for (name, repo) in &repos {
        writeln!(out, "Checking {name}.")?;
        out.flush()?;
        ok &= restic::check(repo, read_data)?.success();
    }
    Ok(match ok {
        true => ExitCode::SUCCESS,
        false => ExitCode::FAILURE,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_env_command_prints_variables_and_its_errors_keep_them_secret() {
        let vars = variables(
            "printf '%s\\n' '# keys' '' 'A_1=one' ' B=\"two words\"' \"C='three'\" 'D=a=b' 'E=' 'F=\"'",
        )
        .unwrap();
        let want = [
            ("A_1", "one"),
            ("B", "two words"),
            ("C", "three"),
            ("D", "a=b"),
            ("E", ""),
            ("F", "\""),
        ];
        assert_eq!(
            vars,
            want.map(|(k, v)| (k.to_owned(), v.to_owned())).to_vec()
        );

        let e = variables("echo A=1; echo \"$((40 + 2)) apples\"")
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("printed, on line 2, something that is no NAME=value"),
            "{e}"
        );
        assert!(!e.contains("42 apples"), "{e}");
        let e = variables("echo A=1; exit 7").unwrap_err().to_string();
        assert!(e.contains("failed: exit status: 7"), "{e}");
    }

    #[test]
    fn runs_names_the_watch_as_running_only_while_it_is() {
        let every = SignedDuration::from_mins(5);
        assert_eq!(
            runs(every, true, true),
            "every 5m under `ai backup watch` (running), and at each session's end"
        );
        assert_eq!(
            runs(every, true, false),
            "every 5m under `ai backup watch` (running)"
        );
        let off = "`ai backup watch` (every 5m) isn't running: start it, or enable \
                   services.ai-tools.backup in Home Manager";
        assert_eq!(
            runs(every, false, true),
            format!("at each session's end and on `ai backup run`; {off}")
        );
        assert_eq!(
            runs(every, false, false),
            format!("on `ai backup run` alone; {off}")
        );
    }

    #[test]
    fn a_state_file_that_is_no_state_is_an_error_naming_it() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join(STATE);
        assert_eq!(State::load(&file).unwrap(), State::default());
        let state = State {
            ran: Some(Timestamp::from_second(1_800_000_000).unwrap()),
            snapshot: Some("abc".into()),
            copied: Some(Copied {
                remote: "s3:x".into(),
                snapshot: None,
            }),
            ..State::default()
        };
        state.save(&file).unwrap();
        assert_eq!(State::load(&file).unwrap(), state);
        fs::write(&file, "{").unwrap();
        let e = State::load(&file).unwrap_err().to_string();
        assert!(e.contains(STATE) && e.contains("Delete it"), "{e}");
    }
}
