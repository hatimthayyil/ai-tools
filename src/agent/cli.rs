use std::{
    cmp::Reverse,
    env, fs,
    io::{self, IsTerminal, Write},
    ops::Range,
    os::{fd::AsRawFd, unix::process::CommandExt},
    path::{Path, PathBuf},
    process::{self, ExitCode},
    sync::Arc,
    time::Instant,
};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand};
use dialoguer::{Confirm, Select};
use jiff::Timestamp;

use crate::{
    agent::{
        claude,
        id::Id,
        record::{Binding, Key},
        rollup::Rollup,
        store::{Snapshot, Store, Thread, ThreadInfo},
        summarise::{Built, OnFail, build, plan, preview, rebuild, rewrite, summarise},
        sync::Synced,
        upgrade,
        view::{Branch, Counts},
    },
    config::FILE,
    file::{self, AtPath},
    memory::{Backend, JOBS, Pattern, broken_pipe, claude::uuid, plural, quote, word},
    page,
    progress::{About, Mode, Past, Progress},
    project::committing,
    summariser::WAIT,
};

/// The exit code of plumbing for a session attached to no thread; it prints nothing.
pub const NOT_ATTACHED: u8 = 3;

#[derive(Parser, Debug)]
#[command(
    name = "agent",
    about = "Agents: named threads that span harness sessions, kept in this project.",
    after_help = "Each agent's thread lives in .ai/local/agents in the project's main checkout."
)]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    #[command(about = "create an agent: an empty thread that sessions attach to.")]
    New {
        #[arg(help = "the agent: letters, digits, '-' and '_'")]
        name: String,
    },
    Resume(Resume),
    #[command(about = "list this project's agents and their sessions.")]
    List {
        #[command(flatten)]
        session: Session,
    },
    #[command(about = "print a thread's view, from the summaries written so far (the default).")]
    View {
        #[arg(help = AGENT_HELP)]
        agent: Option<String>,
        #[command(flatten)]
        session: Session,
    },
    #[command(about = "attach this session to an agent's thread.")]
    Attach {
        #[arg(help = "the agent")]
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
        #[arg(long, help = "include tool output")]
        tool_output: bool,
    },
    #[command(
        about = "search a thread: messages, replies, tool calls and summary lines, newest first."
    )]
    Grep {
        #[command(flatten)]
        targets: Targets,
        #[command(flatten)]
        pat: Pattern,
        #[arg(long, help = "also search tool output")]
        tool_output: bool,
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
    #[command(
        hide = true,
        about = "plumbing: copy the ended session's last entries into its thread, then commit the store."
    )]
    EndSession {
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
        help = "the model for this run [default: summariser.model in ai.toml, else sonnet]"
    )]
    model: Option<String>,
    #[arg(long, help = "print the plan only; call no model and store nothing")]
    dry_run: bool,
    #[arg(
        long,
        conflicts_with = "dry_run",
        help = "write each line with the model and print it beside the line it replaces; store nothing"
    )]
    preview: bool,
    #[arg(
        long,
        hide = true,
        conflicts_with_all = ["id", "rebuild", "dry_run", "preview"],
        help = "run as the background summariser: build what it can past a failed line"
    )]
    background: bool,
}

/// An agent's newest session continued, else a new one started.
#[derive(Args, Debug)]
#[command(
    about = "continue the agent's newest Claude Code session, else start a new one.",
    after_help = "A session resumes where it ran. A new one runs in the agent's worktree when ai.toml's [agents.<name>] names one, after writing the summary lines its view needs; if one fails, Claude Code does not start. Either way it gets the agent's instructions."
)]
pub struct Resume {
    #[arg(help = "the agent")]
    name: String,
    #[arg(long, default_value = claude::HARNESS, value_parser = [claude::HARNESS], help = "the harness to run it in")]
    harness: String,
    #[arg(last = true, help = "arguments for claude, after --")]
    args: Vec<String>,
}

impl Resume {
    /// Runs in the current directory.
    pub fn exec(self) -> Result<ExitCode> {
        let cwd = env::current_dir().context("Cannot read the current directory.")?;
        offer_upgrade(&cwd)?;
        self.run(&cwd)
    }

    /// Runs in `cwd`, committing the store before the harness starts, or after a failure.
    fn run(&self, cwd: &Path) -> Result<ExitCode> {
        let msg = format!("ai agent resume {}", self.name);
        committing(cwd, &msg, || resume(cwd, &self.name, &self.args))
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

/// What an agent argument defaults to: [`pick`]'s order.
pub const AGENT_HELP: &str = "the agent [default: the one this session is attached to, else the project's only agent, else one you pick on a terminal]";

/// The thread a command reads, by `-a`, as [`pick`] resolves it.
#[derive(Args, Debug)]
struct Target {
    #[arg(short = 'a', long = "agent", value_name = "NAME", help = AGENT_HELP)]
    agent: Option<String>,
    #[command(flatten)]
    session: Session,
}

/// The threads a search reads: each `-a` once, in order, else [`pick`]'s one.
#[derive(Args, Debug)]
struct Targets {
    #[arg(
        short = 'a',
        long = "agent",
        value_name = "NAME",
        help = "an agent to search; repeat it to search several [default: the one this session is attached to, else the project's only agent, else one you pick on a terminal]"
    )]
    agents: Vec<String>,
    #[command(flatten)]
    session: Session,
}

/// A session a command names.
#[derive(Clone)]
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

/// On a terminal, offers to convert the agent store at or above `cwd` if it awaits that.
fn offer_upgrade(cwd: &Path) -> Result<()> {
    if !asks() {
        return Ok(());
    }
    upgrade::offer(cwd, |q| {
        Ok(Confirm::new().with_prompt(q).default(false).interact()?)
    })
}

/// Whether a question can be put on the terminal: stdin and stderr are one, and reading it will
/// not stop this process as a background job of it.
fn asks() -> bool {
    let stdin = io::stdin();
    if !(stdin.is_terminal() && io::stderr().is_terminal()) {
        return false;
    }
    let group = unsafe { libc::tcgetpgrp(stdin.as_raw_fd()) };
    group < 0 || group == unsafe { libc::getpgrp() }
}

impl Cli {
    /// Runs in the current directory, printing to stdout.
    pub fn exec(self) -> Result<ExitCode> {
        let cwd = env::current_dir().context("Cannot read the current directory.")?;
        if !matches!(
            self.command,
            Some(Command::SyncSession { .. } | Command::CompactSession { .. })
        ) {
            offer_upgrade(&cwd)?;
        }
        let terminal = io::stdout().is_terminal();
        match self.run(&cwd, &mut io::stdout().lock(), terminal) {
            Err(e) if broken_pipe(&e) => Ok(ExitCode::SUCCESS),
            r => r,
        }
    }

    /// Runs in `cwd`, printing to `out`; views, zooms and searches print whole on a `terminal`.
    /// Events commit the store: creating an agent, attaching, detaching, resuming, a build but
    /// the background summariser's, a compaction and a session's end. A turn's sync and a view
    /// leave what they wrote for the next event's commit.
    pub fn run(self, cwd: &Path, out: &mut dyn Write, terminal: bool) -> Result<ExitCode> {
        let command = self.command.unwrap_or(Command::View {
            agent: None,
            session: Session::default(),
        });
        let msg = match &command {
            Command::New { name } => format!("ai agent new {name}"),
            Command::Attach { name, .. } => format!("ai agent attach {name}"),
            Command::Detach { .. } => "ai agent detach".into(),
            Command::Build(b) if !b.background => match &b.target.agent {
                Some(name) => format!("ai agent build {name}"),
                None => "ai agent build".into(),
            },
            Command::CompactSession { .. } => "ai agent compact-session".into(),
            Command::EndSession { .. } => "ai agent end-session".into(),
            Command::Resume(_)
            | Command::List { .. }
            | Command::View { .. }
            | Command::Zoom { .. }
            | Command::Grep { .. }
            | Command::Build(_)
            | Command::SyncSession { .. } => {
                return command.dispatch(cwd, out, terminal);
            }
        };
        committing(cwd, &msg, || command.dispatch(cwd, out, terminal))
    }
}

impl Command {
    /// Runs in `cwd`, printing to `out`.
    fn dispatch(self, cwd: &Path, out: &mut dyn Write, terminal: bool) -> Result<ExitCode> {
        match self {
            Command::New { name } => new(cwd, out, &name),
            Command::Resume(r) => r.run(cwd),
            Command::List { session } => list(cwd, out, session),
            Command::View { agent, session } => view(cwd, out, agent, session, terminal),
            Command::Attach { name, session } => attach(cwd, out, &name, session),
            Command::Detach { session } => detach(cwd, out, session),
            Command::Zoom {
                target,
                id,
                tool_output,
            } => zoom(cwd, out, target, &id, terminal, tool_output),
            Command::Grep {
                targets,
                pat,
                tool_output,
            } => grep(cwd, out, targets, &pat, tool_output, terminal),
            Command::Build(a) => build_cmd(cwd, out, a),
            Command::SyncSession { session } | Command::EndSession { session } => {
                sync_session(cwd, session)
            }
            Command::CompactSession { session } => compact_session(cwd, out, session),
        }
    }
}

fn not_attached() -> Result<ExitCode> {
    Ok(ExitCode::from(NOT_ATTACHED))
}

/// The thread `name`, else the one session `c` is attached to, else [`choose`]'s; with the
/// session when it is attached to that thread. `usage` shows how to name a thread.
fn pick(
    cwd: &Path,
    name: Option<String>,
    c: Option<Current>,
    usage: &str,
) -> Result<(Store, Thread, Option<Current>)> {
    let store = Store::open(cwd)?;
    let attached = match &c {
        Some(c) => store.attached(&c.harness, &c.session)?.map(|(a, _)| a),
        None => None,
    };
    match (name, attached) {
        (Some(name), attached) => {
            let thread = store.existing(&name)?;
            let c = c.filter(|_| attached.is_some_and(|a| a.name() == name));
            Ok((store, thread, c))
        }
        (None, Some(thread)) => Ok((store, thread, c)),
        (None, None) => {
            let thread = choose(&store, c.is_some(), usage)?;
            Ok((store, thread, None))
        }
    }
}

/// The project's only thread, else one picked on a terminal, newest first (Esc cancels);
/// else an error listing them, newest first. `session`: the caller is an agent session
/// attached to none.
fn choose(store: &Store, session: bool, usage: &str) -> Result<Thread> {
    let mut threads = store.threads()?;
    match threads.len() {
        0 if session => bail!(
            "This session is not attached to an agent, and this project has none. Create one, then attach this session: ai agent new <name>; ai agent attach <name>"
        ),
        0 => bail!("No agents in this project. Create one: ai agent new <name>"),
        1 => return Ok(threads.remove(0)),
        _ => {}
    }
    let mut infos = store.list()?;
    infos.sort_by_key(|t| Reverse(t.newest.as_ref().map(|h| h.ts)));
    let rows: Vec<String> = infos.iter().map(row).collect();
    if io::stdin().is_terminal() && io::stderr().is_terminal() {
        let picked = Select::new()
            .with_prompt("Which agent?")
            .items(&rows)
            .default(0)
            .interact_opt()?;
        let Some(i) = picked else {
            bail!("No agent chosen.");
        };
        return store.thread(&infos[i].name);
    }
    let mut msg = match session {
        true => format!(
            "This session is not attached to an agent. Attach it with `ai agent attach <name>`, or name one: {usage}."
        ),
        false => format!("Name an agent: {usage}."),
    };
    msg += " This project's agents, newest first:";
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

/// How many entries the view shows at each level where `store` was found: `[agent.view]`.
fn counts(store: &Store) -> Result<Counts> {
    store.settings()?.agent.view.counts()
}

/// The branch a command reads in `s`: session `c`'s (to its newest item, else its graft), else
/// the thread's newest, its view by `counts`; `None` while it holds no item.
fn read_branch(s: &Snapshot, c: Option<&Current>, counts: Counts) -> Result<Option<Branch>> {
    match c {
        Some(c) => match s.binding(&c.harness, &c.session)? {
            Some(b) if b.tip.or(b.graft).is_some() => Ok(Some(Branch::session(s, &b, counts)?)),
            _ => Ok(None),
        },
        None => match s.tips()?.last() {
            Some(&tip) => Ok(Some(Branch::load(s, Some(tip), counts)?)),
            None => Ok(None),
        },
    }
}

/// The view of the branch `c` reads in snapshot `s`, by `counts`, as its [`Branch::entries`];
/// `None` while that branch holds no item.
fn view_in(
    t: &Thread,
    s: &Snapshot,
    c: Option<&Current>,
    counts: Counts,
) -> Result<Option<Vec<String>>> {
    Ok(read_branch(s, c, counts)?.map(|b| b.entries(t.name())))
}

/// The thread's latest snapshot, once the lines the view of `branch` (read in it) needs are
/// written: built, waiting first for a running summariser, however long either takes, again
/// while a build writes some and the branch still needs more. Else an error naming each line
/// that failed, or how many lines a build could not write.
fn ready(
    store: &Store,
    t: &Thread,
    branch: impl Fn(&Snapshot) -> Result<Option<Branch>>,
) -> Result<Snapshot> {
    let mut built: Option<Built> = None;
    loop {
        let snap = t.snapshot()?;
        let keys = branch(&snap)?.map_or_else(Vec::new, |b| b.needed());
        if keys.is_empty() {
            return Ok(snap);
        }
        if let Some(b) = &built
            && (b.lines == 0 || !b.failed.is_empty())
        {
            let name = t.name();
            let mut msg = match b.failed.len() {
                0 => format!(
                    "Cannot build the view of thread {name}: it still lacks {}.",
                    plural(keys.len() as u64, "line")
                ),
                n => format!(
                    "Cannot build the view of thread {name}: {} failed.",
                    plural(n as u64, "line")
                ),
            };
            for f in failures(&snap, &b.failed)? {
                msg += &format!("\n{f}");
            }
            bail!("{msg}\nWrite them, then try again: ai agent build -a {name}")
        }
        built = Some(build(
            t,
            store.backend(t, None)?,
            &keys,
            &Progress::quiet(),
        )?);
    }
}

/// Each failed line as `Failed <id>: <error>`, by its id in `snap`.
fn failures(snap: &Snapshot, failed: &[(Key, String)]) -> Result<Vec<String>> {
    let roll = Rollup::new(&snap.tree()?);
    Ok(failed
        .iter()
        .map(|(k, e)| format!("Failed {}: {e}", roll.id(*k)))
        .collect())
}

/// Starts the thread's background summariser, unless `ai.toml` sets `chat.background` false.
fn start_summariser(cwd: &Path, thread: &str) -> Result<()> {
    let store = Store::open(cwd)?;
    store.spawn_summariser(&store.thread(thread)?)?;
    Ok(())
}

/// Commits the store as resuming agent `name`, then runs `claude args` in `dir`, in place of
/// this process.
fn exec_claude(store: &Store, name: &str, dir: &Path, args: &[&str]) -> Result<ExitCode> {
    store.project().commit(&format!("ai agent resume {name}"))?;
    let e = process::Command::new("claude")
        .args(args)
        .current_dir(dir)
        .exec();
    Err(e).context("Cannot start claude.")
}

/// Creates agent `name`.
fn new(cwd: &Path, out: &mut dyn Write, name: &str) -> Result<ExitCode> {
    Store::open(cwd)?.create(name)?;
    writeln!(
        out,
        "Created agent {name}. Start it: ai agent resume {name}"
    )?;
    Ok(ExitCode::SUCCESS)
}

/// Agent `name`'s `[agents.<name>]` instructions, read now and joined by blank lines in the
/// settings' order; `None` when they name none.
fn instructions(store: &Store, name: &str) -> Result<Option<String>> {
    let settings = store.settings()?;
    let paths = settings.agent(name).map_or(&[][..], |a| &a.instructions);
    if paths.is_empty() {
        return Ok(None);
    }
    paths
        .iter()
        .map(|p| {
            fs::read_to_string(p)
                .at(p)
                .map_err(|e| anyhow!("Cannot read agent {name}'s instructions: {e}"))
        })
        .collect::<Result<Vec<_>>>()
        .map(|texts| Some(texts.join("\n\n")))
}

/// Where agent `name` starts: its `[agents.<name>]` worktree, canonical as the harness sees it,
/// else `cwd`. A worktree must exist and be a git worktree of this checkout, so the session's
/// hooks find this store.
fn worktree(store: &Store, cwd: &Path, name: &str) -> Result<PathBuf> {
    let settings = store.settings()?;
    let Some(dir) = settings.agent(name).and_then(|a| a.worktree.as_deref()) else {
        return Ok(cwd.to_path_buf());
    };
    if !dir.is_dir() {
        bail!(
            "Agent {name}'s worktree {} does not exist. Create it: git worktree add {}",
            file::pretty(dir),
            dir.display()
        );
    }
    let dir = fs::canonicalize(dir).at(dir)?;
    if !store.holds(&dir) {
        bail!(
            "Agent {name}'s worktree {} does not share this project's agents: make it a git \
             worktree of {}, holding an {FILE} with the id {}.",
            file::pretty(&dir),
            file::pretty(&store.project().home),
            store.project().id
        );
    }
    Ok(dir)
}

/// `claude`'s arguments: `first`, the agent's `instructions` as an added system prompt, then
/// the launcher's `args`.
fn claude_args<'a>(
    first: [&'a str; 2],
    instructions: &'a Option<String>,
    args: &'a [String],
) -> Vec<&'a str> {
    let mut argv = first.to_vec();
    if let Some(text) = instructions {
        argv.extend(["--append-system-prompt", text]);
    }
    argv.extend(args.iter().map(String::as_str));
    argv
}

/// A new Claude Code session attached to agent `name`, in its worktree with its instructions,
/// bound with its future transcript, once the lines its view needs are written.
fn launch(cwd: &Path, name: &str, args: &[String]) -> Result<ExitCode> {
    let store = Store::open(cwd)?;
    let t = store.existing(name)?;
    let dir = worktree(&store, cwd, name)?;
    let instructions = instructions(&store, name)?;
    let counts = counts(&store)?;
    ready(&store, &t, |s| {
        s.graft()?
            .map(|g| Branch::load(s, Some(g), counts))
            .transpose()
    })?;
    let session = uuid()?;
    let config = claude_config().context("No home directory: set CLAUDE_CONFIG_DIR.")?;
    let path = config
        .join("projects")
        .join(claude::project_dir(&dir))
        .join(format!("{session}.jsonl"));
    store.attach(name, claude::HARNESS, &session, Some(&path))?;
    exec_claude(
        &store,
        name,
        &dir,
        &claude_args(["--session-id", &session], &instructions, args),
    )
}

/// The thread's newest Claude Code session, resumed attached in the directory it ran in when
/// it can be, with the agent's instructions; else a new one, saying why when there was one.
fn resume(cwd: &Path, name: &str, args: &[String]) -> Result<ExitCode> {
    let store = Store::open(cwd)?;
    let t = store.existing(name)?;
    let newest = t
        .snapshot()?
        .bindings()?
        .into_iter()
        .filter(|b| b.harness == claude::HARNESS)
        .max_by_key(|b| (b.tip, b.attached));
    let why = match &newest {
        None => return launch(cwd, name, args),
        Some(b) => match &b.transcript {
            Some(p) if p.is_file() => match claude::session_dir(p) {
                Some(dir) if store.holds(&dir) => {
                    let instructions = instructions(&store, name)?;
                    store.attach(name, claude::HARNESS, &b.session, Some(p))?;
                    return exec_claude(
                        &store,
                        name,
                        &dir,
                        &claude_args(["--resume", &b.session], &instructions, args),
                    );
                }
                Some(dir) => format!(
                    "session {} ran in {}, outside this project",
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
    let threads = Store::open(cwd)?.list()?;
    if threads.is_empty() {
        writeln!(
            out,
            "No agents in this project. Create one: ai agent new <name>"
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
    let a = Store::open(cwd)?.attach(name, &c.harness, &c.session, c.transcript(cwd).as_deref())?;
    writeln!(out, "Attached to {}.", a.thread)?;
    if !a.added.is_empty() {
        start_summariser(cwd, &a.thread)?;
    }
    Ok(ExitCode::SUCCESS)
}

fn detach(cwd: &Path, out: &mut dyn Write, s: Session) -> Result<ExitCode> {
    let Some(c) = s.current() else {
        bail!("No session: run this inside an agent session, or pass --session.");
    };
    match Store::open(cwd)?.detach(&c.harness, &c.session)? {
        Some(t) => writeln!(out, "Detached from {t}.")?,
        None => bail!("This session is not attached to an agent."),
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
/// agent session or a project, or for an unattached session.
fn synced(cwd: &Path, s: Session) -> Result<Option<(Store, Thread, Current)>> {
    let (Some(c), Some(store)) = (s.current(), Store::find(cwd)?) else {
        return Ok(None);
    };
    if store.sync(&c.harness, &c.session, c.transcript.as_deref())? == Synced::NotAttached {
        return Ok(None);
    }
    Ok(store
        .attached(&c.harness, &c.session)?
        .map(|(t, _)| (store, t, c)))
}

/// Sync, the lines the view needs built however long they take, then the view; nothing for a
/// session whose branch holds no item yet. An error, and no view, if a line it needs is not
/// written.
fn compact_session(cwd: &Path, out: &mut dyn Write, s: Session) -> Result<ExitCode> {
    let Some((store, t, c)) = synced(cwd, s)? else {
        return not_attached();
    };
    let counts = counts(&store)?;
    let snap = ready(&store, &t, |s| read_branch(s, Some(&c), counts))?;
    if let Some(view) = view_in(&t, &snap, Some(&c), counts)? {
        write!(out, "{}", view.concat())?;
    }
    Ok(ExitCode::SUCCESS)
}

/// The thread's latest snapshot, session `c` synced into it first when given.
fn latest(store: &Store, t: &Thread, c: Option<&Current>) -> Result<Snapshot> {
    if let Some(c) = c {
        store.sync(&c.harness, &c.session, c.transcript.as_deref())?;
    }
    t.snapshot()
}

/// The thread view `ai view` prints after the memory, with the thread's name: [`pick`]'s
/// thread, of the session's branch (synced first) when it is attached there, else of the
/// newest, as its [`Branch::entries`]. Unnamed, an agent session reads only the thread it is
/// attached to, and outside a project with threads there is none. `None` too while the branch
/// holds no item. It writes no line.
pub fn thread_view(
    cwd: &Path,
    name: Option<String>,
    s: Session,
) -> Result<Option<(Vec<String>, String)>> {
    let c = s.current();
    if name.is_none() {
        let Some(store) = Store::find(cwd)? else {
            return Ok(None);
        };
        let none = match &c {
            Some(c) => store.attached(&c.harness, &c.session)?.is_none(),
            None => store.threads()?.is_empty(),
        };
        if none {
            return Ok(None);
        }
    }
    let (store, t, c) = pick(cwd, name, c, "ai view <name>")?;
    let snap = latest(&store, &t, c.as_ref())?;
    let view = view_in(&t, &snap, c.as_ref(), counts(&store)?)?;
    Ok(view.map(|v| (v, t.name().to_string())))
}

/// The thread's view: of the session's branch when the session is attached to it (synced
/// first), else of its newest branch; whole on a `terminal`, else in parts read on with `ai
/// page`. It writes no line: a turn not summarised yet shows its message.
fn view(
    cwd: &Path,
    out: &mut dyn Write,
    name: Option<String>,
    s: Session,
    terminal: bool,
) -> Result<ExitCode> {
    let (store, t, session) = pick(cwd, name, s.current(), "ai agent view <name>")?;
    let snap = latest(&store, &t, session.as_ref())?;
    let Some(entries) = view_in(&t, &snap, session.as_ref(), counts(&store)?)? else {
        writeln!(out, "Thread {} holds nothing yet.", t.name())?;
        return Ok(ExitCode::SUCCESS);
    };
    let command = format!("ai agent view {}", t.name());
    page::print_entries(out, &page::cache()?, &command, &entries, terminal)?;
    Ok(ExitCode::SUCCESS)
}

/// What `id` holds on the picked thread's branch (the session's, else the newest): whole on a
/// `terminal`, else in parts read on with `ai page`.
fn zoom(
    cwd: &Path,
    out: &mut dyn Write,
    target: Target,
    id: &str,
    terminal: bool,
    tool_output: bool,
) -> Result<ExitCode> {
    let usage = "ai agent zoom -a <name> <id>";
    let (store, t, session) = pick(cwd, target.agent, target.session.current(), usage)?;
    let Some(parsed) = Id::parse(id) else {
        bail!(
            "'{id}' is not an id. Ids look like 12 (a turn), 8+4 (turns 8 to 11) or 12.3 (an item)."
        );
    };
    let snap = t.snapshot()?;
    let branch = read_branch(&snap, session.as_ref(), counts(&store)?)?;
    let Some(text) = branch.and_then(|b| b.zoom(parsed, tool_output)) else {
        bail!("This branch has no {parsed}.");
    };
    let flag = if tool_output { " --tool-output" } else { "" };
    let command = format!("ai agent zoom -a {} {parsed}{flag}", t.name());
    page::print(out, &page::cache()?, &command, &text, terminal)?;
    Ok(ExitCode::SUCCESS)
}

/// The matches of `pat` on each picked thread's branch (the session's, else the newest), thread
/// by thread in the order named, oldest first within each, and each line led by its agent's
/// name when there are several: all on a `terminal`, else in parts newest first, read on with
/// `ai page`.
fn grep(
    cwd: &Path,
    out: &mut dyn Write,
    targets: Targets,
    pat: &Pattern,
    tool_output: bool,
    terminal: bool,
) -> Result<ExitCode> {
    let usage = "ai agent grep -a <name> <pattern>";
    let c = targets.session.current();
    let mut names: Vec<String> = Vec::new();
    for n in targets.agents {
        if !names.contains(&n) {
            names.push(n);
        }
    }
    let picked = match names.is_empty() {
        true => vec![pick(cwd, None, c, usage)?],
        false => names
            .into_iter()
            .map(|n| pick(cwd, Some(n), c.clone(), usage))
            .collect::<Result<_>>()?,
    };
    let re = pat.regex()?;
    let several = picked.len() > 1;
    let (mut hits, mut agents) = (Vec::new(), Vec::new());
    for (store, t, session) in &picked {
        if let Some(b) = read_branch(&t.snapshot()?, session.as_ref(), counts(store)?)? {
            hits.extend(b.grep(&re, tool_output).into_iter().map(|h| match several {
                true => format!("{} {}", t.name(), h.line),
                false => h.line,
            }));
        }
        agents.push(format!("-a {}", t.name()));
    }
    if hits.is_empty() {
        writeln!(out, "No match.")?;
        return Ok(ExitCode::SUCCESS);
    }
    let a = pat.args();
    let mut cmd = format!("ai agent grep {} {}", agents.join(" "), quote(&a[0]));
    for v in &a[1..] {
        cmd += &format!(" {}", word(v));
    }
    if tool_output {
        cmd += " --tool-output";
    }
    let sizes: Vec<u64> = hits.iter().map(|h| h.len() as u64 + 1).collect();
    let render = |shown: Range<usize>| hits[shown].iter().map(|h| format!("{h}\n")).collect();
    page::search(
        out,
        &page::cache()?,
        &cmd,
        &sizes,
        hits.len() as u64,
        terminal,
        render,
    )?;
    Ok(ExitCode::SUCCESS)
}

/// What one line of level `l` of a thread covers: a turn's work, a turn, or turns.
fn covers(l: u32) -> String {
    match l {
        0 => "work".into(),
        _ => plural(1 << (l - 1), "turn"),
    }
}

/// Writes the thread's ready lines, or with `rebuild` rewrites them all; with `id`, that one
/// line. It shows its plan, its progress and what it did, unless it is the `background`
/// summariser, and stops at the first line that fails unless it is. With `dry_run`, prints its
/// plan alone; with `preview`, prints what it would write instead, and stores nothing.
fn build_cmd(cwd: &Path, out: &mut dyn Write, a: Build) -> Result<ExitCode> {
    let Build {
        target,
        id,
        rebuild: again,
        model,
        dry_run: dry,
        preview,
        background,
    } = a;
    let (id, model) = (id.as_deref(), model.as_deref());
    let usage = "ai agent build -a <name>";
    let (store, t, session) = pick(cwd, target.agent, target.session.current(), usage)?;
    let line = match id {
        Some(v) => {
            let id = Id::parse(v).with_context(|| {
                format!("'{v}' is not an id. Ids look like 12 (a turn), 8+4 (turns 8 to 11) or 12.1+7 (the work of turn 12).")
            })?;
            let key = read_branch(&t.snapshot()?, session.as_ref(), counts(&store)?)?
                .and_then(|b| b.line_key(id))
                .with_context(|| {
                    format!("{id} is not a line on this branch. Lines are closed turns (12), aligned nodes over closed turns (8+4) and their work (12.1+7).")
                })?;
            Some((id, key))
        }
        None => None,
    };
    if preview {
        let backend = store.backend(&t, model)?;
        return preview_cmd(&t, out, backend, again, line.map(|l| l.1));
    }
    let name = t.name();
    let title = match (again, line) {
        (true, Some((id, _))) => format!("Rewriting line {id} of agent {name}"),
        (false, Some((id, _))) => format!("Building line {id} of agent {name}"),
        (true, None) => format!("Rebuilding agent {name}"),
        (false, None) => format!("Building agent {name}"),
    };
    if dry {
        let about = about(&t, title, store.model(model)?)?;
        let progress = Progress::new(&mut *out, Mode::Plain, about);
        if !progress.plan(&plan(&t, again, line.map(|l| l.1))?)? {
            drop(progress);
            nothing(out, line.map(|l| l.0))?;
        }
        return Ok(ExitCode::SUCCESS);
    }
    let backend = store.backend(&t, model)?;
    let about = about(&t, title, backend.model().into())?;
    let progress = Progress::new(&mut *out, Mode::of(!background), about);
    let built = match (again, line) {
        (true, Some((id, key))) => {
            let Some(r) = rewrite(&t, backend, key, Instant::now() + WAIT, &progress)? else {
                bail!("A summariser is still running on {name}. Try again when it ends.");
            };
            progress.end(Some(r.dropped as u64), r.built.spent)?;
            if r.above > 0 {
                writeln!(
                    out,
                    "The next ai agent build -a {name} writes the {} above {id} again.",
                    plural(r.above as u64, "line")
                )?;
            }
            r.built
        }
        (false, Some((id, key))) => {
            let b = build(&t, backend, &[key], &progress)?;
            match progress.planned() {
                true => progress.end(None, b.spent)?,
                false => {
                    drop(progress);
                    nothing(out, Some(id))?
                }
            }
            b
        }
        (true, None) => {
            let Some((dropped, b)) = rebuild(&t, backend, Instant::now() + WAIT, &progress)? else {
                bail!("A summariser is still running on {name}. Try again when it ends.");
            };
            progress.end(Some(dropped as u64), b.spent)?;
            b
        }
        (false, None) => {
            let on_fail = match background {
                true => OnFail::Continue,
                false => OnFail::Stop,
            };
            let Some(b) = summarise(&t, backend, on_fail, &progress)? else {
                drop(progress);
                writeln!(
                    out,
                    "A summariser is already running on {name}; it writes these lines too."
                )?;
                return Ok(ExitCode::SUCCESS);
            };
            match progress.planned() {
                true => progress.end(None, b.spent)?,
                false => {
                    drop(progress);
                    nothing(out, None)?
                }
            }
            b
        }
    };
    outcome(&t, out, &built)
}

/// What a build of thread `t` titled `title` shows, `model` writing its lines.
fn about(t: &Thread, title: String, model: String) -> Result<About> {
    let log = t.log_path()?;
    Ok(About {
        title,
        unit: "line",
        covers,
        first_bar: 0,
        model,
        jobs: JOBS,
        past: Past::read(&log, |id| id.split_once('@')?.0.parse().ok())?,
        log,
    })
}

/// Says a build of line `id`, else of the thread, has nothing to write.
fn nothing(out: &mut dyn Write, id: Option<Id>) -> io::Result<()> {
    match id {
        Some(id) => writeln!(out, "{id} is written already. Rewrite it with --rebuild."),
        None => writeln!(out, "Nothing to write."),
    }
}

/// `build --preview`: each line a build would write, or with `again` a rebuild, of `line` and
/// what it is made from if given, written by the model and printed beside the line it
/// replaces; stores nothing.
fn preview_cmd(
    t: &Thread,
    out: &mut dyn Write,
    backend: Arc<dyn Backend>,
    again: bool,
    line: Option<Key>,
) -> Result<ExitCode> {
    let p = preview(t, backend, again, line)?;
    let tree = t.snapshot()?.tree()?;
    for l in &p.lines {
        let place = |seq: u64| tree.place(seq as usize);
        let id = Id::line(l.key.level, place(l.first), place(l.key.last));
        writeln!(out, "{id}|{}", l.text)?;
        if let Some(was) = &l.was {
            writeln!(out, "  was: {was}")?;
        }
    }
    writeln!(
        out,
        "Preview: {}, {} by the model; nothing stored.",
        plural(p.built.lines, "line"),
        p.built.calls
    )?;
    outcome(t, out, &p.built)
}

/// Prints each line of `built` that failed; failure if any did.
fn outcome(t: &Thread, out: &mut dyn Write, built: &Built) -> Result<ExitCode> {
    if built.failed.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }
    for f in failures(&t.snapshot()?, &built.failed)? {
        writeln!(out, "{f}")?;
    }
    Ok(ExitCode::FAILURE)
}
