use std::{
    cmp::Reverse,
    env,
    io::{self, IsTerminal, Write},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{self, ExitCode},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use dialoguer::Select;
use indicatif::{ProgressBar, ProgressStyle};
use jiff::Timestamp;

use crate::{
    chat::{
        claude,
        id::Id,
        record::{Binding, Key},
        rollup::Rollup,
        store::{Chat, Mark, Snapshot, Thread, ThreadInfo, check_name},
        summarise::{OnFail, build, dry_run, first, rebuild, rewrite, summarise},
        sync::Synced,
        view::Branch,
    },
    file,
    memory::{Pattern, broken_pipe, claude::uuid, plural, quote, word},
    page,
};

/// The exit code of plumbing for a session attached to no thread; it prints nothing.
pub const NOT_ATTACHED: u8 = 3;
/// How long `compact-session` waits for the lines its view needs, and `build --rebuild`
/// for a running summariser.
pub const WAIT: Duration = Duration::from_secs(60);

#[derive(Parser, Debug)]
#[command(
    name = "chat",
    about = "Named conversations that span agent sessions, kept in this repository.",
    after_help = "Threads live in ~/.ai/chat/<root commit>, or under $AI_CHAT_DIR if set."
)]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    #[command(
        about = "start Claude Code in a new session attached to a thread, creating it if new.",
        after_help = "It first writes the summary lines the session's view needs; if one fails, Claude Code does not start."
    )]
    Start {
        #[arg(help = "the thread: letters, digits, '-' and '_'")]
        name: String,
        #[arg(last = true, help = "arguments for claude, after --")]
        args: Vec<String>,
    },
    #[command(about = "continue the thread's newest Claude Code session.")]
    Resume {
        #[arg(help = "the thread")]
        name: String,
        #[arg(last = true, help = "arguments for claude, after --")]
        args: Vec<String>,
    },
    #[command(about = "list this repository's threads and their sessions.")]
    List {
        #[command(flatten)]
        session: Session,
    },
    #[command(about = "print a thread's view, from the summaries written so far (the default).")]
    View {
        #[arg(help = THREAD_HELP)]
        thread: Option<String>,
        #[command(flatten)]
        session: Session,
        #[command(flatten)]
        parts: Parts,
    },
    #[command(about = "attach this session to a thread, creating it if new.")]
    Attach {
        #[arg(help = "the thread: letters, digits, '-' and '_'")]
        name: String,
        #[command(flatten)]
        session: Session,
    },
    #[command(about = "detach this session from its thread.")]
    Detach {
        #[command(flatten)]
        session: Session,
    },
    #[command(about = "open a line of the view, or any turn, item or range.")]
    Zoom {
        #[command(flatten)]
        target: Target,
        #[arg(help = "an id: 12 (turn 12), 8+4 (turns 8 to 11), 12.3 (item 3 of turn 12)")]
        id: String,
        #[command(flatten)]
        parts: Parts,
        #[arg(long, help = "include tool output")]
        tool_output: bool,
    },
    #[command(
        about = "search a thread: messages, replies, tool calls and summary lines, newest first."
    )]
    Grep {
        #[command(flatten)]
        target: Target,
        #[command(flatten)]
        pat: Pattern,
        #[arg(long, help = "also search tool output")]
        tool_output: bool,
        #[command(flatten)]
        parts: Parts,
    },
    #[command(
        about = "write a thread's summary lines that are ready, or one line, stopping at the first that fails."
    )]
    Build(Build),
    #[command(
        hide = true,
        about = "plumbing: copy this session's new entries into its thread."
    )]
    SyncSession {
        #[command(flatten)]
        session: Session,
    },
    #[command(
        hide = true,
        about = "plumbing: sync, then print the view this session compacts to; fails if a line it needs cannot be written."
    )]
    CompactSession {
        #[command(flatten)]
        session: Session,
    },
}

/// What `build` writes, and how.
#[derive(Args, Debug)]
struct Build {
    #[command(flatten)]
    target: Target,
    #[arg(help = "one line: 12 (a turn), 8+4 (a node), 12.1+7 (a turn's work)")]
    id: Option<String>,
    #[arg(
        long,
        help = "rewrite lines already written, all or the one named, and drop the lines above them"
    )]
    rebuild: bool,
    #[arg(
        long,
        help = "the model for this run [default: chat.model in ai.toml, else sonnet]"
    )]
    model: Option<String>,
    #[arg(
        long,
        help = "print each line it would write beside the line it replaces; store nothing"
    )]
    dry_run: bool,
    #[arg(
        long,
        hide = true,
        conflicts_with_all = ["id", "rebuild", "dry_run"],
        help = "run as the background summariser: build what it can past a failed line"
    )]
    background: bool,
}

/// Which part of a long output to print, and the thread state the parts come from.
#[derive(Args, Debug, Default)]
struct Parts {
    #[arg(long, hide = true, help = "the part to print, as printed")]
    part: Option<u64>,
    #[arg(
        long,
        hide = true,
        help = "the thread state the parts come from, as printed"
    )]
    at: Option<String>,
}

impl Parts {
    /// The state `at` names, else the thread's latest.
    fn snapshot(&self, t: &Thread) -> Result<Snapshot> {
        match &self.at {
            Some(at) => t.snapshot_at(at.parse()?),
            None => t.snapshot(),
        }
    }
}

/// The harness session a command is for: its flags, else the harness's environment.
#[derive(Args, Debug)]
pub struct Session {
    #[arg(long, hide = true, default_value = claude::HARNESS, help = "the harness running the session")]
    harness: String,
    #[arg(
        long,
        hide = true,
        help = "the session's id [default: $CLAUDE_CODE_SESSION_ID]"
    )]
    session: Option<String>,
    #[arg(
        long,
        hide = true,
        help = "the session's transcript [default: found from its id]"
    )]
    transcript: Option<PathBuf>,
}

/// What a thread argument defaults to: [`pick`]'s order.
pub const THREAD_HELP: &str = "the thread [default: the one this session is attached to, else the repository's only thread, else one you pick on a terminal]";

/// The thread a command reads, by `-t`, as [`pick`] resolves it.
#[derive(Args, Debug)]
struct Target {
    #[arg(short = 't', long = "thread", value_name = "NAME", help = THREAD_HELP)]
    thread: Option<String>,
    #[command(flatten)]
    session: Session,
}

/// A session a command names.
struct Current {
    harness: String,
    session: String,
    transcript: Option<PathBuf>,
}

impl Default for Session {
    fn default() -> Session {
        Session {
            harness: claude::HARNESS.into(),
            session: None,
            transcript: None,
        }
    }
}

impl Session {
    fn current(self) -> Option<Current> {
        let session = self.session.or_else(|| match self.harness.as_str() {
            claude::HARNESS => claude::session_id(),
            _ => None,
        })?;
        Some(Current {
            harness: self.harness,
            session,
            transcript: self.transcript,
        })
    }
}

impl Current {
    /// Its transcript: as given, else found by its harness's adapter.
    fn transcript(&self, cwd: &Path) -> Option<PathBuf> {
        self.transcript
            .clone()
            .or_else(|| match self.harness.as_str() {
                claude::HARNESS => claude::transcript(&claude_config()?, &self.session, cwd),
                _ => None,
            })
    }

    fn is(&self, b: &Binding) -> bool {
        b.harness == self.harness && b.session == self.session
    }
}

fn claude_config() -> Option<PathBuf> {
    claude::config_dir(|k| env::var(k).ok())
}

impl Cli {
    /// Runs in the current directory, printing to stdout.
    pub fn exec(self) -> Result<ExitCode> {
        let cwd = env::current_dir().context("Cannot read the current directory.")?;
        let terminal = io::stdout().is_terminal();
        match self.run(&cwd, &mut io::stdout().lock(), terminal) {
            Err(e) if broken_pipe(&e) => Ok(ExitCode::SUCCESS),
            r => r,
        }
    }

    /// Runs in `cwd`, printing to `out`; views, zooms and searches print whole on a `terminal`.
    pub fn run(self, cwd: &Path, out: &mut dyn Write, terminal: bool) -> Result<ExitCode> {
        let command = self.command.unwrap_or(Command::View {
            thread: None,
            session: Session::default(),
            parts: Parts::default(),
        });
        match command {
            Command::Start { name, args } => launch(cwd, &name, &args),
            Command::Resume { name, args } => resume(cwd, &name, &args),
            Command::List { session } => list(cwd, out, session),
            Command::View {
                thread,
                session,
                parts,
            } => view(cwd, out, thread, session, &parts, terminal),
            Command::Attach { name, session } => attach(cwd, out, &name, session),
            Command::Detach { session } => detach(cwd, out, session),
            Command::Zoom {
                target,
                id,
                parts,
                tool_output,
            } => zoom(cwd, out, target, &id, &parts, terminal, tool_output),
            Command::Grep {
                target,
                pat,
                tool_output,
                parts,
            } => grep(cwd, out, target, &pat, tool_output, &parts, terminal),
            Command::Build(a) => build_cmd(cwd, out, a),
            Command::SyncSession { session } => sync_session(cwd, session),
            Command::CompactSession { session } => compact_session(cwd, out, session),
        }
    }
}

fn not_attached() -> Result<ExitCode> {
    Ok(ExitCode::from(NOT_ATTACHED))
}

/// The threads of the repository at `cwd`.
fn find(cwd: &Path) -> Result<Chat> {
    Chat::find(cwd)?.with_context(|| match gix::discover(cwd) {
        Ok(_) => {
            "This repository has no commits yet. Its threads are kept under its first \
                  commit; commit once, then retry."
        }
        Err(_) => "Not in a git repository. Run this inside the repository that holds the thread.",
    })
}

/// The thread `name`, else the one session `c` is attached to, else [`choose`]'s; with the
/// session when it is attached to that thread. `usage` shows how to name a thread.
fn pick(
    cwd: &Path,
    name: Option<String>,
    c: Option<Current>,
    usage: &str,
) -> Result<(Chat, Thread, Option<Current>)> {
    let chat = find(cwd)?;
    let attached = match &c {
        Some(c) => chat.attached(&c.harness, &c.session)?.map(|(a, _)| a),
        None => None,
    };
    match (name, attached) {
        (Some(name), attached) => {
            let thread = chat.thread(&name)?;
            if !thread.exists()? {
                bail!("No thread {name} in this repository. List the threads: ai chat list");
            }
            let c = c.filter(|_| attached.is_some_and(|a| a.name() == name));
            Ok((chat, thread, c))
        }
        (None, Some(thread)) => Ok((chat, thread, c)),
        (None, None) => {
            let thread = choose(&chat, c.is_some(), usage)?;
            Ok((chat, thread, None))
        }
    }
}

/// The repository's only thread, else one picked on a terminal, newest first (Esc cancels);
/// else an error listing them, newest first. `session`: the caller is an agent session
/// attached to none.
fn choose(chat: &Chat, session: bool, usage: &str) -> Result<Thread> {
    let mut threads = chat.threads()?;
    match threads.len() {
        0 if session => bail!(
            "This session is not attached to a thread, and this repository has none. Attach it to a new one: ai chat attach <name>"
        ),
        0 => bail!("No threads in this repository. Start one: ai chat start <name>"),
        1 => return Ok(threads.remove(0)),
        _ => {}
    }
    let mut infos = chat.list()?;
    infos.sort_by_key(|t| Reverse(t.newest.as_ref().map(|h| h.ts)));
    let rows: Vec<String> = infos.iter().map(row).collect();
    if io::stdin().is_terminal() && io::stderr().is_terminal() {
        let picked = Select::new()
            .with_prompt("Which thread?")
            .items(&rows)
            .default(0)
            .interact_opt()?;
        let Some(i) = picked else {
            bail!("No thread chosen.");
        };
        return chat.thread(&infos[i].name);
    }
    let mut msg = match session {
        true => format!(
            "This session is not attached to a thread. Attach it with `ai chat attach <name>`, or name one: {usage}."
        ),
        false => format!("Name a thread: {usage}."),
    };
    msg += " This repository's threads, newest first:";
    for r in rows {
        msg += &format!("\n  {r}");
    }
    bail!(msg)
}

/// A minute of `ts`, in UTC.
fn when(ts: &Timestamp) -> String {
    ts.strftime("%Y-%m-%d %H:%M").to_string()
}

/// A thread's row in `list` and [`choose`]: its name, turns, newest item and, when more than
/// one, branches.
fn row(t: &ThreadInfo) -> String {
    let last = t.newest.as_ref().map_or("-".into(), |h| when(&h.ts));
    let branches = match t.tips.len() {
        0 | 1 => String::new(),
        n => format!("  {n} branches"),
    };
    format!(
        "{}  {}  {last}{branches}",
        t.name,
        plural(t.turns as u64, "turn")
    )
}

/// The branch a command reads in `s`: session `c`'s (to its newest item, else its graft), else
/// the thread's newest; `None` while it holds no item.
fn read_branch(s: &Snapshot, c: Option<&Current>) -> Result<Option<Branch>> {
    match c {
        Some(c) => match s.binding(&c.harness, &c.session)? {
            Some(b) if b.tip.or(b.graft).is_some() => Ok(Some(Branch::session(s, &b)?)),
            _ => Ok(None),
        },
        None => match s.tips()?.last() {
            Some(&tip) => Ok(Some(Branch::load(s, Some(tip))?)),
            None => Ok(None),
        },
    }
}

/// The view of the branch `c` reads in snapshot `s`; `None` while that branch holds no item.
fn view_in(t: &Thread, s: &Snapshot, c: Option<&Current>) -> Result<Option<String>> {
    Ok(read_branch(s, c)?.map(|b| b.view(t.name())))
}

/// The thread's latest snapshot, once the lines the view of `branch` (read in it) needs are
/// written: built within [`WAIT`], waiting first for a running summariser, stopping at the
/// first line that fails. Else an error naming each failed line, or how many lines are missing.
fn ready(
    chat: &Chat,
    t: &Thread,
    branch: impl Fn(&Snapshot) -> Result<Option<Branch>>,
) -> Result<Snapshot> {
    let snap = t.snapshot()?;
    let keys = branch(&snap)?.map_or_else(Vec::new, |b| b.needed());
    if keys.is_empty() {
        return Ok(snap);
    }
    let built = build(
        t,
        chat.backend(t, None)?,
        &keys,
        Instant::now() + WAIT,
        &ProgressBar::hidden(),
    )?;
    let snap = t.snapshot()?;
    let missing = branch(&snap)?.map_or(0, |b| b.needed().len());
    if missing == 0 {
        return Ok(snap);
    }
    let name = t.name();
    let mut msg = match built.failed.len() {
        0 => format!(
            "Cannot build the view of thread {name}: after {} s it still lacks {}.",
            WAIT.as_secs(),
            plural(missing as u64, "line")
        ),
        n => format!(
            "Cannot build the view of thread {name}: {} failed.",
            plural(n as u64, "line")
        ),
    };
    for f in failures(&snap, &built.failed)? {
        msg += &format!("\n{f}");
    }
    bail!("{msg}\nWrite them, then try again: ai chat build -t {name}")
}

/// Each failed line as `Failed <id>: <error>`, by its id in `snap`.
fn failures(snap: &Snapshot, failed: &[(Key, String)]) -> Result<Vec<String>> {
    let tree = snap.tree()?;
    let roll = Rollup::new(&tree);
    let place = |seq: u64| tree.place(seq as usize);
    Ok(failed
        .iter()
        .map(|(k, e)| {
            let id = Id::line(k.level, place(first(&roll, *k)), place(k.last));
            format!("Failed {id}: {e}")
        })
        .collect())
}

/// Starts the thread's background summariser, unless `ai.toml` sets `chat.background` false.
fn start_summariser(cwd: &Path, thread: &str) -> Result<()> {
    let chat = find(cwd)?;
    chat.spawn_summariser(&chat.thread(thread)?)?;
    Ok(())
}

/// Runs `claude args` in `dir`, in place of this process.
fn exec_claude(dir: &Path, args: &[&str]) -> Result<ExitCode> {
    let e = process::Command::new("claude")
        .args(args)
        .current_dir(dir)
        .exec();
    Err(e).context("Cannot start claude.")
}

/// A new Claude Code session attached to `name`, bound with its future transcript, once the
/// lines its view needs are written.
fn launch(cwd: &Path, name: &str, args: &[String]) -> Result<ExitCode> {
    let chat = find(cwd)?;
    check_name(name)?;
    let t = chat.thread(name)?;
    if t.exists()? {
        ready(&chat, &t, |s| {
            s.graft()?.map(|g| Branch::load(s, Some(g))).transpose()
        })?;
    }
    let session = uuid()?;
    let config = claude_config().context("No home directory: set CLAUDE_CONFIG_DIR.")?;
    let path = config
        .join("projects")
        .join(claude::project_dir(cwd))
        .join(format!("{session}.jsonl"));
    chat.attach(name, claude::HARNESS, &session, Some(&path))?;
    let mut argv = vec!["--session-id", &session];
    argv.extend(args.iter().map(String::as_str));
    exec_claude(cwd, &argv)
}

/// The thread's newest Claude Code session, resumed attached in the directory it ran in when
/// it can be; else a new one.
fn resume(cwd: &Path, name: &str, args: &[String]) -> Result<ExitCode> {
    let chat = find(cwd)?;
    let t = chat.thread(name)?;
    if !t.exists()? {
        bail!("No thread {name} in this repository. Start one: ai chat start {name}");
    }
    let newest = t
        .snapshot()?
        .bindings()?
        .into_iter()
        .filter(|b| b.harness == claude::HARNESS)
        .max_by_key(|b| (b.tip, b.attached));
    let why = match &newest {
        None => "it has no Claude Code session".to_string(),
        Some(b) => match &b.transcript {
            Some(p) if p.is_file() => match claude::session_dir(p) {
                Some(dir) if chat.holds(&dir) => {
                    chat.attach(name, claude::HARNESS, &b.session, Some(p))?;
                    let mut argv = vec!["--resume", &b.session];
                    argv.extend(args.iter().map(String::as_str));
                    return exec_claude(&dir, &argv);
                }
                Some(dir) => format!(
                    "session {} ran in {}, outside this repository",
                    b.session,
                    file::pretty(&dir)
                ),
                None => format!("the transcript of session {} names no directory", b.session),
            },
            Some(_) => format!("the transcript of session {} is gone", b.session),
            None => format!("session {} has no transcript", b.session),
        },
    };
    eprintln!("Cannot resume {name}: {why}. Starting a new session.");
    launch(cwd, name, args)
}

/// Every thread: its turns, newest activity and branches; under it, its sessions.
fn list(cwd: &Path, out: &mut dyn Write, s: Session) -> Result<ExitCode> {
    let threads = find(cwd)?.list()?;
    if threads.is_empty() {
        writeln!(
            out,
            "No threads in this repository. Start one: ai chat start <name>"
        )?;
        return Ok(ExitCode::SUCCESS);
    }
    let current = s.current();
    for t in threads {
        writeln!(out, "{}", row(&t))?;
        let wh = t.sessions.iter().map(|(b, _)| b.harness.len()).max();
        let ws = t.sessions.iter().map(|(b, _)| b.session.len()).max();
        let (wh, ws) = (wh.unwrap_or(0), ws.unwrap_or(0));
        for (b, at) in &t.sessions {
            let state = match b.is_attached() {
                true => "attached",
                false => "detached",
            };
            let here = match current.as_ref().is_some_and(|c| c.is(b)) {
                true => "  this session",
                false => "",
            };
            writeln!(
                out,
                "  {:<wh$}  {:<ws$}  {state}  {}{here}",
                b.harness,
                b.session,
                when(at)
            )?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn attach(cwd: &Path, out: &mut dyn Write, name: &str, s: Session) -> Result<ExitCode> {
    let Some(c) = s.current() else {
        bail!("No session: run this inside an agent session, or pass --session.");
    };
    let a = find(cwd)?.attach(name, &c.harness, &c.session, c.transcript(cwd).as_deref())?;
    let mut msg = format!("Attached to {}", a.thread);
    if a.created {
        msg += ", a new thread";
    }
    writeln!(out, "{msg}.")?;
    if !a.added.is_empty() {
        start_summariser(cwd, &a.thread)?;
    }
    Ok(ExitCode::SUCCESS)
}

fn detach(cwd: &Path, out: &mut dyn Write, s: Session) -> Result<ExitCode> {
    let Some(c) = s.current() else {
        bail!("No session: run this inside an agent session, or pass --session.");
    };
    match find(cwd)?.detach(&c.harness, &c.session)? {
        Some(t) => writeln!(out, "Detached from {t}.")?,
        None => bail!("This session is not attached to a thread."),
    }
    Ok(ExitCode::SUCCESS)
}

fn sync_session(cwd: &Path, s: Session) -> Result<ExitCode> {
    let Some(c) = s.current() else {
        return not_attached();
    };
    match super::sync(cwd, &c.harness, &c.session, c.transcript.as_deref())? {
        Synced::NotAttached => not_attached(),
        Synced::Attached { thread, added } => {
            if !added.is_empty() {
                start_summariser(cwd, &thread)?;
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Session `s`, synced, with its threads and the thread it is attached to; `None` outside an
/// agent session or a repository with commits, or for an unattached session.
fn synced(cwd: &Path, s: Session) -> Result<Option<(Chat, Thread, Current)>> {
    let (Some(c), Some(chat)) = (s.current(), Chat::find(cwd)?) else {
        return Ok(None);
    };
    if chat.sync(&c.harness, &c.session, c.transcript.as_deref())? == Synced::NotAttached {
        return Ok(None);
    }
    Ok(chat
        .attached(&c.harness, &c.session)?
        .map(|(t, _)| (chat, t, c)))
}

/// Sync, the lines the view needs built within [`WAIT`], then the view; nothing for a session
/// whose branch holds no item yet. An error, and no view, if a line it needs is not written.
fn compact_session(cwd: &Path, out: &mut dyn Write, s: Session) -> Result<ExitCode> {
    let Some((chat, t, c)) = synced(cwd, s)? else {
        return not_attached();
    };
    let snap = ready(&chat, &t, |s| read_branch(s, Some(&c)))?;
    if let Some(view) = view_in(&t, &snap, Some(&c))? {
        write!(out, "{view}")?;
    }
    Ok(ExitCode::SUCCESS)
}

/// The thread's latest snapshot, session `c` synced into it first when given.
fn latest(chat: &Chat, t: &Thread, c: Option<&Current>) -> Result<Snapshot> {
    if let Some(c) = c {
        chat.sync(&c.harness, &c.session, c.transcript.as_deref())?;
    }
    t.snapshot()
}

/// The thread view `ai view` prints after the memory, with the thread's name and state:
/// [`pick`]'s thread, of the session's branch (synced first) when it is attached there, else of
/// the newest; with `at` (a state it gave), of that state, unsynced. Unnamed, an agent session
/// reads only the thread it is attached to, and outside a repository with threads there is
/// none. `None` too while the branch holds no item. It writes no line.
pub fn thread_view(
    cwd: &Path,
    name: Option<String>,
    s: Session,
    at: Option<Mark>,
) -> Result<Option<(String, String, Mark)>> {
    let c = s.current();
    if name.is_none() {
        let Some(chat) = Chat::find(cwd)? else {
            return Ok(None);
        };
        let none = match &c {
            Some(c) => chat.attached(&c.harness, &c.session)?.is_none(),
            None => chat.threads()?.is_empty(),
        };
        if none {
            return Ok(None);
        }
    }
    let (chat, t, c) = pick(cwd, name, c, "ai view <name>")?;
    let snap = match at {
        Some(at) => t.snapshot_at(at)?,
        None => latest(&chat, &t, c.as_ref())?,
    };
    let view = view_in(&t, &snap, c.as_ref())?;
    Ok(view.map(|v| (v, t.name().to_string(), snap.mark())))
}

/// The thread's view, from one state of it: of the session's branch when the session is
/// attached to it (synced first), else of its newest branch; whole on a `terminal`, else in
/// parts. It writes no line: a turn not summarised yet shows its message; `--at` reads the state
/// it names.
fn view(
    cwd: &Path,
    out: &mut dyn Write,
    name: Option<String>,
    s: Session,
    parts: &Parts,
    terminal: bool,
) -> Result<ExitCode> {
    let (chat, t, session) = pick(cwd, name, s.current(), "ai chat view <name>")?;
    let snap = match &parts.at {
        Some(_) => parts.snapshot(&t)?,
        None => latest(&chat, &t, session.as_ref())?,
    };
    let shown = view_in(&t, &snap, session.as_ref())?.map(|v| (v, snap.mark()));
    let Some((text, mark)) = shown else {
        writeln!(out, "Thread {} holds nothing yet.", t.name())?;
        return Ok(ExitCode::SUCCESS);
    };
    page::print(out, &text, page::pick(parts.part, terminal), |k| {
        format!("ai chat view {} --part {k} --at {mark}", t.name())
    })?;
    Ok(ExitCode::SUCCESS)
}

/// What `id` holds on the picked thread's branch (the session's, else the newest): whole on a
/// `terminal`, else in parts, each cut from the state the first read.
fn zoom(
    cwd: &Path,
    out: &mut dyn Write,
    target: Target,
    id: &str,
    parts: &Parts,
    terminal: bool,
    tool_output: bool,
) -> Result<ExitCode> {
    let usage = "ai chat zoom -t <name> <id>";
    let (_, t, session) = pick(cwd, target.thread, target.session.current(), usage)?;
    let Some(parsed) = Id::parse(id) else {
        bail!(
            "'{id}' is not an id. Ids look like 12 (a turn), 8+4 (turns 8 to 11) or 12.3 (an item)."
        );
    };
    let snap = parts.snapshot(&t)?;
    let branch = read_branch(&snap, session.as_ref())?;
    let Some(text) = branch.and_then(|b| b.zoom(parsed, tool_output)) else {
        bail!("This branch has no {parsed}.");
    };
    let (mark, flag) = (snap.mark(), if tool_output { " --tool-output" } else { "" });
    page::print(out, &text, page::pick(parts.part, terminal), |k| {
        format!(
            "ai chat zoom -t {} {parsed} --part {k} --at {mark}{flag}",
            t.name()
        )
    })?;
    Ok(ExitCode::SUCCESS)
}

/// The matches of `pat` on the picked thread's branch (the session's, else the newest), oldest
/// first: all on a `terminal`, else in parts newest first, each cut from the state the first
/// read.
fn grep(
    cwd: &Path,
    out: &mut dyn Write,
    target: Target,
    pat: &Pattern,
    tool_output: bool,
    parts: &Parts,
    terminal: bool,
) -> Result<ExitCode> {
    let usage = "ai chat grep -t <name> <pattern>";
    let (_, t, session) = pick(cwd, target.thread, target.session.current(), usage)?;
    let re = pat.regex()?;
    let snap = parts.snapshot(&t)?;
    let hits = match read_branch(&snap, session.as_ref())? {
        Some(b) => b.grep(&re, tool_output),
        None => Vec::new(),
    };
    if hits.is_empty() {
        writeln!(out, "No match.")?;
        return Ok(ExitCode::SUCCESS);
    }
    let a = pat.args();
    let mut cmd = format!("ai chat grep -t {} {}", t.name(), quote(&a[0]));
    for v in &a[1..] {
        cmd += &format!(" {}", word(v));
    }
    if tool_output {
        cmd += " --tool-output";
    }
    let sizes: Vec<u64> = hits.iter().map(|h| h.line.len() as u64 + 1).collect();
    let mark = snap.mark();
    let (shown, footer) = page::search(
        &sizes,
        hits.len() as u64,
        page::pick(parts.part, terminal),
        |k| format!("{cmd} --part {k} --at {mark}"),
    )?;
    for h in &hits[shown] {
        writeln!(out, "{}", h.line)?;
    }
    writeln!(out, "{footer}")?;
    Ok(ExitCode::SUCCESS)
}

/// A bar on stderr for a summariser run when `show` and stderr is a terminal: lines written of
/// those planned, calls running, failures and time so far. Else hidden.
fn progress(show: bool) -> ProgressBar {
    if !show || !io::stderr().is_terminal() {
        return ProgressBar::hidden();
    }
    let style =
        ProgressStyle::with_template("{spinner} {bar:30} {pos}/{len} lines · {msg} · {elapsed}")
            .expect("a valid template");
    let bar = ProgressBar::no_length()
        .with_style(style)
        .with_message("reading the thread");
    bar.enable_steady_tick(Duration::from_millis(100));
    bar
}

/// Writes the thread's ready lines, or with `rebuild` rewrites them all; with `id`, that one
/// line. With `dry_run`, prints what it would write and stores nothing. It stops at the first
/// line that fails, unless it is the `background` summariser. On a terminal, all but a dry
/// run and the background summariser show their progress on stderr.
fn build_cmd(cwd: &Path, out: &mut dyn Write, a: Build) -> Result<ExitCode> {
    let Build {
        target,
        id,
        rebuild: again,
        model,
        dry_run: dry,
        background,
    } = a;
    let (id, model) = (id.as_deref(), model.as_deref());
    let usage = "ai chat build -t <name>";
    let (chat, t, session) = pick(cwd, target.thread, target.session.current(), usage)?;
    let line = match id {
        Some(v) => {
            let id = Id::parse(v).with_context(|| {
                format!("'{v}' is not an id. Ids look like 12 (a turn), 8+4 (turns 8 to 11) or 12.1+7 (the work of turn 12).")
            })?;
            let key = read_branch(&t.snapshot()?, session.as_ref())?
                .and_then(|b| b.line_key(id))
                .with_context(|| {
                    format!("{id} is not a line on this branch. Lines are closed turns (12), aligned nodes over closed turns (8+4) and their work (12.1+7).")
                })?;
            Some((id, key))
        }
        None => None,
    };
    let backend = chat.backend(&t, model)?;
    let bar = progress(!dry && !background);
    let built = match (dry, again, line) {
        (false, true, Some((id, key))) => {
            let Some(r) = rewrite(&t, backend, key, Instant::now() + WAIT, &bar)? else {
                bail!(
                    "A summariser is still running on {}. Try again when it ends.",
                    t.name()
                );
            };
            writeln!(
                out,
                "Dropped {}; wrote {}, {} by the model.",
                plural(r.dropped as u64, "line"),
                r.built.lines,
                r.built.calls
            )?;
            if r.above > 0 {
                writeln!(
                    out,
                    "The next ai chat build -t {} writes the {} above {id} again.",
                    t.name(),
                    plural(r.above as u64, "line")
                )?;
            }
            r.built
        }
        (false, false, Some((id, key))) => {
            let b = build(&t, backend, &[key], Instant::now() + WAIT, &bar)?;
            match b.lines {
                0 if b.failed.is_empty() => {
                    writeln!(out, "{id} is written already. Rewrite it with --rebuild.")?
                }
                n => writeln!(
                    out,
                    "Wrote {}, {} by the model.",
                    plural(n, "line"),
                    b.calls
                )?,
            }
            b
        }
        (true, _, line) => {
            let d = dry_run(&t, backend, again, line.map(|l| l.1))?;
            let tree = t.snapshot()?.tree()?;
            for l in &d.lines {
                let place = |seq: u64| tree.place(seq as usize);
                let id = Id::line(l.key.level, place(l.first), place(l.key.last));
                writeln!(out, "{id}|{}", l.text)?;
                if let Some(was) = &l.was {
                    writeln!(out, "  was: {was}")?;
                }
            }
            writeln!(
                out,
                "Dry run: {}, {} by the model; nothing stored.",
                plural(d.built.lines, "line"),
                d.built.calls
            )?;
            d.built
        }
        (false, true, None) => {
            let Some((dropped, b)) = rebuild(&t, backend, Instant::now() + WAIT, &bar)? else {
                bail!(
                    "A summariser is still running on {}. Try again when it ends.",
                    t.name()
                );
            };
            writeln!(
                out,
                "Dropped {}; wrote {}, {} by the model.",
                plural(dropped as u64, "line"),
                b.lines,
                b.calls
            )?;
            b
        }
        (false, false, None) => {
            let on_fail = match background {
                true => OnFail::Continue,
                false => OnFail::Stop,
            };
            let Some(b) = summarise(&t, backend, on_fail, &bar)? else {
                writeln!(
                    out,
                    "A summariser is already running on {}; it writes these lines too.",
                    t.name()
                )?;
                return Ok(ExitCode::SUCCESS);
            };
            match b.lines {
                0 => writeln!(out, "Nothing to write.")?,
                n => writeln!(
                    out,
                    "Wrote {}, {} by the model.",
                    plural(n, "line"),
                    b.calls
                )?,
            }
            b
        }
    };
    if !built.failed.is_empty() {
        for f in failures(&t.snapshot()?, &built.failed)? {
            writeln!(out, "{f}")?;
        }
    }
    Ok(match built.failed.is_empty() {
        true => ExitCode::SUCCESS,
        false => ExitCode::FAILURE,
    })
}
