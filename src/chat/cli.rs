use std::{
    env,
    io::{self, Write},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{self, ExitCode},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};

use crate::{
    chat::{
        claude,
        id::Id,
        record::Binding,
        rollup::Rollup,
        store::{Chat, Mark, Snapshot, Thread, check_name},
        summarise::{build, dry_run, first, rebuild, rewrite, summarise},
        sync::Synced,
        view::Branch,
    },
    file,
    memory::{Pattern, broken_pipe, page, plural, quote, word},
};

/// The exit code of plumbing for a session attached to no thread; it prints nothing.
pub const NOT_ATTACHED: u8 = 3;
/// The largest part of a paged output, in bytes: under Claude Code's 30,000-character cap on
/// a command's output.
pub const PART: u64 = 20_000;
/// How long `compact-session` waits for the lines its view needs, and `summarise --rebuild`
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
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    #[command(
        about = "start Claude Code in a new session attached to a thread, creating it if new."
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
    #[command(about = "print a thread's view, from the summaries written so far.")]
    View {
        #[command(flatten)]
        target: Target,
        #[arg(long, help = "the part of the view to print, as printed")]
        part: Option<u64>,
        #[arg(long, help = "the thread state the parts come from, as printed")]
        at: Option<String>,
    },
    #[command(about = "attach this session to a thread, creating it if new.")]
    Attach {
        name: String,
        #[command(flatten)]
        session: Session,
    },
    #[command(
        about = "detach this session from its thread; /compact goes back to Claude Code's own."
    )]
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
        #[arg(help = "the part of a long output to show, as printed")]
        part: Option<u64>,
        #[arg(long, help = "include tool output")]
        tool_output: bool,
    },
    #[command(about = "search a thread: messages, replies, tool calls and summary lines.")]
    Grep {
        #[command(flatten)]
        target: Target,
        #[command(flatten)]
        pat: Pattern,
        #[arg(long, help = "also search tool output")]
        tool_output: bool,
        #[arg(
            long,
            value_name = "ID",
            help = "only matches before this id, for the next page"
        )]
        before: Option<String>,
    },
    #[command(about = "write a thread's summary lines that are ready, or one line.")]
    Summarise {
        #[command(flatten)]
        target: Target,
        #[arg(help = "one line: 12 (a turn), 8+4 (a node), 12.3+7 (a chunk)")]
        id: Option<String>,
        #[arg(
            long,
            help = "rewrite lines already written, all or the one named, and drop the lines above them"
        )]
        rebuild: bool,
        #[arg(
            long,
            help = "the model for this run [default: ai.chat.model, else sonnet]"
        )]
        model: Option<String>,
        #[arg(
            long,
            help = "print each line it would write beside the line it replaces; store nothing"
        )]
        dry_run: bool,
    },
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
        about = "plumbing: sync, then print the view this session compacts to."
    )]
    CompactSession {
        #[command(flatten)]
        session: Session,
    },
}

/// The harness session a command is for: its flags, else the harness's environment.
#[derive(Args, Debug)]
struct Session {
    #[arg(long, default_value = claude::HARNESS, help = "the harness running the session")]
    harness: String,
    #[arg(long, help = "the session's id [default: $CLAUDE_CODE_SESSION_ID]")]
    session: Option<String>,
    #[arg(long, help = "the session's transcript [default: found from its id]")]
    transcript: Option<PathBuf>,
}

/// The thread a command reads: `-t`, else the one this session is attached to.
#[derive(Args, Debug)]
struct Target {
    #[arg(
        short = 't',
        long = "thread",
        value_name = "NAME",
        help = "the thread [default: the one this session is attached to]"
    )]
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
        match self.run(&cwd, &mut io::stdout().lock()) {
            Err(e) if broken_pipe(&e) => Ok(ExitCode::SUCCESS),
            r => r,
        }
    }

    pub fn run(self, cwd: &Path, out: &mut dyn Write) -> Result<ExitCode> {
        match self.command {
            Command::Start { name, args } => launch(cwd, &name, &args),
            Command::Resume { name, args } => resume(cwd, &name, &args),
            Command::List { session } => list(cwd, out, session),
            Command::View { target, part, at } => {
                view(cwd, out, target, part.unwrap_or(1), at.as_deref())
            }
            Command::Attach { name, session } => attach(cwd, out, &name, session),
            Command::Detach { session } => detach(cwd, out, session),
            Command::Zoom {
                target,
                id,
                part,
                tool_output,
            } => zoom(cwd, out, target, &id, part.unwrap_or(1), tool_output),
            Command::Grep {
                target,
                pat,
                tool_output,
                before,
            } => grep(cwd, out, target, &pat, tool_output, before.as_deref()),
            Command::Summarise {
                target,
                id,
                rebuild,
                model,
                dry_run,
            } => summarise_cmd(
                cwd,
                out,
                target,
                id.as_deref(),
                rebuild,
                model.as_deref(),
                dry_run,
            ),
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

/// The thread `-t` names, else the one the session is attached to; with the session when it is
/// attached to that thread.
fn pick(cwd: &Path, t: Target) -> Result<(Chat, Thread, Option<Current>)> {
    let chat = find(cwd)?;
    let current = t.session.current();
    let attached = match &current {
        Some(c) => chat.attached(&c.harness, &c.session)?.map(|(a, _)| a),
        None => None,
    };
    match t.thread {
        Some(name) => {
            let thread = chat.thread(&name)?;
            if !thread.exists()? {
                bail!("No thread {name} in this repository. List the threads: ai chat list");
            }
            let current = current.filter(|_| attached.is_some_and(|a| a.name() == name));
            Ok((chat, thread, current))
        }
        None => match attached {
            Some(thread) => Ok((chat, thread, current)),
            None if current.is_some() => bail!(
                "This session is not attached to a thread. Attach it with `ai chat attach <name>`, or name a thread with -t."
            ),
            None => bail!(
                "Name a thread with -t; this shell is outside an agent session. List the threads: ai chat list"
            ),
        },
    }
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

/// The view of the branch `c` reads in the thread's latest snapshot, once the lines it needs
/// are written: built within [`WAIT`], waiting first for a running summariser. With the state
/// it was read from; `None` while that branch holds no item.
fn fresh_view(chat: &Chat, t: &Thread, c: Option<&Current>) -> Result<Option<(String, Mark)>> {
    let snap = t.snapshot()?;
    let Some(branch) = read_branch(&snap, c)? else {
        return Ok(None);
    };
    let keys = branch.needed();
    let snap = match keys.is_empty() {
        true => snap,
        false => {
            build(t, chat.backend(None)?, &keys, Instant::now() + WAIT)?;
            t.snapshot()?
        }
    };
    let mark = snap.mark();
    Ok(view_in(t, &snap, c)?.map(|v| (v, mark)))
}

/// Starts the thread's background summariser, unless `ai.chat.summarise` is false.
fn start_summariser(cwd: &Path, thread: &str) -> Result<()> {
    let chat = find(cwd)?;
    chat.spawn_summariser(&chat.thread(thread)?)?;
    Ok(())
}

/// A random (version 4) UUID, as Claude Code's session ids.
fn uuid() -> Result<String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b)?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    ))
}

/// Runs `claude args` in `dir`, in place of this process.
fn exec_claude(dir: &Path, args: &[&str]) -> Result<ExitCode> {
    let e = process::Command::new("claude")
        .args(args)
        .current_dir(dir)
        .exec();
    Err(e).context("Cannot start claude.")
}

/// A new Claude Code session attached to `name`, bound with its future transcript.
fn launch(cwd: &Path, name: &str, args: &[String]) -> Result<ExitCode> {
    let chat = find(cwd)?;
    check_name(name)?;
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
    let when = |ts: &jiff::Timestamp| ts.strftime("%Y-%m-%d %H:%M").to_string();
    for t in threads {
        let last = t.newest.as_ref().map_or("-".into(), |h| when(&h.ts));
        let branches = match t.tips.len() {
            0 | 1 => String::new(),
            n => format!("  {n} branches"),
        };
        writeln!(
            out,
            "{}  {}  {last}{branches}",
            t.name,
            plural(t.turns as u64, "turn")
        )?;
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

/// Sync, the lines the view needs built within [`WAIT`], then the view; nothing for a session
/// whose branch holds no item yet.
fn compact_session(cwd: &Path, out: &mut dyn Write, s: Session) -> Result<ExitCode> {
    let (Some(c), Some(chat)) = (s.current(), Chat::find(cwd)?) else {
        return not_attached();
    };
    if chat.sync(&c.harness, &c.session, c.transcript.as_deref())? == Synced::NotAttached {
        return not_attached();
    }
    let Some((t, _)) = chat.attached(&c.harness, &c.session)? else {
        return not_attached();
    };
    if let Some((view, _)) = fresh_view(&chat, &t, Some(&c))? {
        write!(out, "{view}")?;
    }
    Ok(ExitCode::SUCCESS)
}

/// The thread's view, in parts from one state of it: of the session's branch when the
/// session is attached to it (synced first), else of its newest branch. It writes no line: a
/// turn not summarised yet shows its message; `--at` reads the state it names.
fn view(
    cwd: &Path,
    out: &mut dyn Write,
    target: Target,
    part: u64,
    at: Option<&str>,
) -> Result<ExitCode> {
    let (chat, t, session) = pick(cwd, target)?;
    let snap = match at {
        Some(at) => t.snapshot_at(at.parse()?)?,
        None => {
            if let Some(c) = &session {
                chat.sync(&c.harness, &c.session, c.transcript.as_deref())?;
            }
            t.snapshot()?
        }
    };
    let shown = view_in(&t, &snap, session.as_ref())?.map(|v| (v, snap.mark()));
    let Some((text, mark)) = shown else {
        writeln!(out, "Thread {} holds nothing yet.", t.name())?;
        return Ok(ExitCode::SUCCESS);
    };
    let (text, n) = page(&text, PART, part)?;
    writeln!(out, "{}", text.strip_suffix('\n').unwrap_or(text))?;
    if part < n {
        let next = part + 1;
        writeln!(
            out,
            "Part {part} of {n}. Next: ai chat view -t {} --part {next} --at {mark}",
            t.name()
        )?;
    } else if n > 1 {
        writeln!(out, "Part {part} of {n}.")?;
    }
    Ok(ExitCode::SUCCESS)
}

/// What `id` holds on the picked thread's branch (the session's, else the newest), in parts.
fn zoom(
    cwd: &Path,
    out: &mut dyn Write,
    target: Target,
    id: &str,
    part: u64,
    tool_output: bool,
) -> Result<ExitCode> {
    let (_, t, session) = pick(cwd, target)?;
    let Some(parsed) = Id::parse(id) else {
        bail!(
            "'{id}' is not an id. Ids look like 12 (a turn), 8+4 (turns 8 to 11) or 12.3 (an item)."
        );
    };
    let snap = t.snapshot()?;
    let branch = read_branch(&snap, session.as_ref())?;
    let Some(text) = branch.and_then(|b| b.zoom(parsed, tool_output)) else {
        bail!("This branch has no {parsed}.");
    };
    let (text, n) = page(&text, PART, part)?;
    writeln!(out, "{}", text.strip_suffix('\n').unwrap_or(text))?;
    if part < n {
        let flag = if tool_output { " --tool-output" } else { "" };
        writeln!(
            out,
            "Part {part} of {n}. Next: ai chat zoom -t {} {parsed} {}{flag}",
            t.name(),
            part + 1
        )?;
    }
    Ok(ExitCode::SUCCESS)
}

/// The matches of `pat` on the picked thread's branch (the session's, else the newest), oldest
/// first: the newest that fit in [`PART`] bytes, then how to get the older ones.
fn grep(
    cwd: &Path,
    out: &mut dyn Write,
    target: Target,
    pat: &Pattern,
    tool_output: bool,
    before: Option<&str>,
) -> Result<ExitCode> {
    let (_, t, session) = pick(cwd, target)?;
    let re = pat.regex()?;
    let before = match before {
        Some(v) => {
            Some(Id::parse(v).with_context(|| format!("'{v}' is not an id. Copy it as printed."))?)
        }
        None => None,
    };
    let snap = t.snapshot()?;
    let hits = match read_branch(&snap, session.as_ref())? {
        Some(b) => b.grep(&re, tool_output, before),
        None => before.is_none().then(Vec::new),
    };
    let Some(hits) = hits else {
        bail!(
            "This branch has no {}.",
            before.expect("only a bad --before")
        );
    };
    if hits.is_empty() {
        writeln!(out, "No match.")?;
        return Ok(ExitCode::SUCCESS);
    }
    let mut from = hits.len();
    let mut size = 0;
    while from > 0 && (from == hits.len() || size + hits[from - 1].line.len() < PART as usize) {
        from -= 1;
        size += hits[from].line.len() + 1;
    }
    for h in &hits[from..] {
        writeln!(out, "{}", h.line)?;
    }
    if from == 0 {
        writeln!(out, "{}.", plural(hits.len() as u64, "match"))?;
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
    writeln!(
        out,
        "Newest {} of {}. Older: {cmd} --before {}",
        hits.len() - from,
        hits.len(),
        hits[from].id
    )?;
    Ok(ExitCode::SUCCESS)
}

/// Writes the thread's ready lines, or with `again` rewrites them all; with `id`, that one
/// line. With `dry`, prints what it would write and stores nothing.
fn summarise_cmd(
    cwd: &Path,
    out: &mut dyn Write,
    target: Target,
    id: Option<&str>,
    again: bool,
    model: Option<&str>,
    dry: bool,
) -> Result<ExitCode> {
    let (chat, t, session) = pick(cwd, target)?;
    let line = match id {
        Some(v) => {
            let id = Id::parse(v).with_context(|| {
                format!("'{v}' is not an id. Ids look like 12 (a turn), 8+4 (turns 8 to 11) or 12.3+7 (a chunk).")
            })?;
            let key = read_branch(&t.snapshot()?, session.as_ref())?
                .and_then(|b| b.line_key(id))
                .with_context(|| {
                    format!("{id} is not a line on this branch. Lines are closed turns (12), aligned nodes over closed turns (8+4) and chunks (12.3+7).")
                })?;
            Some((id, key))
        }
        None => None,
    };
    let backend = chat.backend(model)?;
    let built = match (dry, again, line) {
        (false, true, Some((id, key))) => {
            let Some(r) = rewrite(&t, backend, key, Instant::now() + WAIT)? else {
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
                    "The next ai chat summarise -t {} writes the {} above {id} again.",
                    t.name(),
                    plural(r.above as u64, "line")
                )?;
            }
            r.built
        }
        (false, false, Some((id, key))) => {
            let b = build(&t, backend, &[key], Instant::now() + WAIT)?;
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
            let Some((dropped, b)) = rebuild(&t, backend, Instant::now() + WAIT)? else {
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
            let Some(b) = summarise(&t, backend)? else {
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
        let tree = t.snapshot()?.tree()?;
        let roll = Rollup::new(&tree);
        let place = |seq: u64| tree.place(seq as usize);
        for (k, e) in &built.failed {
            let id = Id::line(k.level, place(first(&roll, *k)), place(k.last));
            writeln!(out, "Failed {id}: {e}")?;
        }
    }
    Ok(match built.failed.is_empty() {
        true => ExitCode::SUCCESS,
        false => ExitCode::FAILURE,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuids_are_version_4() {
        let u = uuid().unwrap();
        assert_eq!(u.len(), 36);
        let parts: Vec<&str> = u.split('-').collect();
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            [8, 4, 4, 4, 12]
        );
        assert!(parts[2].starts_with('4'));
        assert!("89ab".contains(&parts[3][..1]));
        assert_ne!(uuid().unwrap(), u);
    }
}
