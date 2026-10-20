use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{self, ExitCode},
    sync::Arc,
    time::Instant,
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use tracing::{info, info_span};

use crate::{
    file::{AtPath, pretty},
    memory::{
        backend::MakeBackend,
        claude::{self, CALLS, ClaudeCode},
        compact::{Compactor, GaveUp, Options, Spend},
        config::Knob,
        cover::{self, cover},
        grep::{Grep, grep},
        hook::{REMINDED, hook},
        log,
        meta::{Line, Meta, dash, dated, parts},
        prov,
        record::{Kind, Message, Place, Who, midnight},
        stats::{Stats, stats},
        store::{ME, Snapshot, Store},
        tree::Coord,
        usage::{self, Tally, Use},
        view::{Mem, PLACEHOLDER, VIEW},
        zoom::{page, zoom},
    },
};

const TEMPLATE: &str = "\
## Memory

Your memory:
- The tool is `{memo}`
- Your memories are in `{data}`

This memory outlives every session, compaction, model and vendor change.
Without it you do not know who you are, or what was decided and tried.

### At startup: activating memory (mandatory)

Run `{memo} wake` before any other tool call, in every session, and
then do exactly what it prints, to the end of its output.

### While working: register memories (mandatory)

Call `{memo} note \"<1 line, max {chars} bytes>\"` whenever you learn
something new, or something worth keeping happens. That covers a task
worth real effort, a fact or insight the user teaches you, anything you
learn about their life (even indirectly), any event of lasting effect.

Write only what happened or what you learned: the tool records the time,
machine, repo, branch, agent, model and session of every note, so never
put a date, repo or session in it.

Do not register redundant memories.

Never edit or delete anything under `{data}`: the tool manages it.

### Reading it: the view, zoom, grep

`wake` prints the view: the whole memory in a fixed number of one-line
summaries, oldest first, under UTC date headers (`2026-10-11`). A line
is `id|text` for message id, or `id+n|text` for the n messages from id
on; one that runs past its header's day shows its last day,
`256+8|..10-12|text`. A short message is its own line, word for word;
the older the messages, the more a line covers. Notes (what sessions
like this one noted) are bare text; a chat with the user is tagged by
speaker, `user:` and `ai:`. A long view comes in parts: run the
`{memo} wake <id>` each part ends with until it says you are awake.

`{memo} zoom <id+n>` opens a line two levels down: the four lines of n/4
it was made from, or its messages when n is 2 or 4; `{memo} zoom <id>`
gives message id in full. Zoom whenever a line
only mentions something you need, before you act, guess or ask.
`{memo} grep <regex>` searches every message, word for word; `-t` adds
the summaries, `--help` lists the filters. `wake`, `zoom` and `grep`
take `--time` for the time of day and `-o repo,branch,...` for more
fields. `{memo} show <id>` prints where a message or summary came
from.

### If you're a subagent: skip everything above

Parallel sessions on this machine are all you, and may all write memories.
A subagent is not: it must never run `{memo}`, because it cannot judge what
is already known, and its notes would arrive duplicated and incorrectly.
When you spawn one, write: `You are a subagent. Don't run {memo}.`";

#[derive(Parser, Debug)]
#[command(
    name = "memory",
    about = "A permanent, append-only memory for AI agents.",
    after_help = "The memories live in ~/.ai/memory, or in $AI_MEMORY_DIR if set."
)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    #[command(about = "create this memory; print the setup block.")]
    Init,
    #[command(about = "read your memory: print the view. Run first, every session.")]
    Wake {
        #[arg(help = "the message to read on from, as printed")]
        from: Option<u64>,
        #[command(flatten)]
        meta: Meta,
    },
    #[command(about = "record one memory: one short line.")]
    Note {
        #[arg(allow_hyphen_values = true)]
        text: String,
    },
    #[command(about = "build every summary that can be built now, with Claude Code.")]
    Summarise,
    #[command(about = "search every message ever recorded, newest page first.")]
    Grep(Grep),
    #[command(about = "open a line of the view into the four lines two levels under it.")]
    Zoom {
        #[arg(help = "a line's id, as printed: 2184+8, or 7 for message 7")]
        id: String,
        #[arg(help = "the part of a long message to show, as printed")]
        part: Option<u64>,
        #[command(flatten)]
        meta: Meta,
    },
    #[command(about = "show where one message or summary came from.")]
    Show {
        #[arg(help = "a line's id, as printed: 2184+8, or 7 for message 7")]
        id: String,
    },
    #[command(about = "show this memory's sizes, or change one.")]
    Config {
        #[arg(value_name = "NAME=VALUE")]
        sets: Vec<String>,
    },
    #[command(about = "bulk-load dated notes (bootstrap only).")]
    Import { file: PathBuf },
    #[command(about = "count how often agents wake, note, zoom, grep and show.")]
    Stats(Stats),
    #[command(
        hide = true,
        about = "plumbing: the prompt hook for agent harnesses, a memory reminder on long prompts and every few short ones."
    )]
    PromptHook,
}

impl Command {
    /// Its name and arguments, as typed; a note's text is in the store.
    fn typed(&self) -> (&'static str, Vec<String>) {
        let part = |p: &Option<u64>| p.map(|p| p.to_string());
        match self {
            Command::Init => ("init", Vec::new()),
            Command::Wake { from, meta } => {
                ("wake", part(from).into_iter().chain(meta.args()).collect())
            }
            Command::Note { .. } => ("note", Vec::new()),
            Command::Summarise => ("summarise", Vec::new()),
            Command::Grep(g) => ("grep", g.typed()),
            Command::Zoom { id, part: p, meta } => (
                "zoom",
                std::iter::once(id.clone())
                    .chain(part(p))
                    .chain(meta.args())
                    .collect(),
            ),
            Command::Show { id } => ("show", vec![id.clone()]),
            Command::Config { sets } => ("config", sets.clone()),
            Command::Import { file } => ("import", vec![file.display().to_string()]),
            Command::Stats(st) => ("stats", st.args()),
            Command::PromptHook => ("prompt-hook", Vec::new()),
        }
    }
}

/// How commands reach the compactor's model and stdin; tests swap in their own.
pub struct Runtime {
    pub backend: Box<MakeBackend>,
    pub opts: Options,
    /// Whether `note` starts a background `summarise`.
    pub summarise_on_note: bool,
    /// Called by `summarise` when a round is done, while it still holds the compactor lock.
    pub before_release: Box<dyn Fn()>,
    pub stdin: Box<dyn Fn() -> io::Result<String>>,
}

impl Default for Runtime {
    fn default() -> Runtime {
        Runtime {
            backend: Box::new(|calls| Ok(Arc::new(ClaudeCode::compactor(calls)?))),
            opts: Options::default(),
            summarise_on_note: std::env::var_os("AI_MEMORY_SUMMARISE").is_none_or(|v| v != "0"),
            before_release: Box::new(|| {}),
            stdin: Box::new(|| io::read_to_string(io::stdin())),
        }
    }
}

/// Starts `ai <tool> summarise` on the store at `dir` (named to it by `$var`), detached; it logs to
/// `summarise.log` in `log_dir`, its stderr too.
pub fn spawn_summariser(tool: &str, var: &str, dir: &Path, log_dir: &Path) -> Result<()> {
    log::spawn(&log_dir.join(log::SUMMARISE_LOG), |c| {
        c.args([tool, "summarise"]).env(var, dir)
    })
}

/// `$var`, else `~/.ai/<name>`.
pub fn store_dir(var: &str, name: &str) -> Result<PathBuf> {
    if let Some(d) = std::env::var_os(var).filter(|d| !d.is_empty()) {
        return Ok(d.into());
    }
    let home = std::env::home_dir()
        .with_context(|| format!("No home directory. Point {var} at the {name}."))?;
    Ok(home.join(".ai").join(name))
}

/// `$AI_MEMORY_DIR`, else `~/.ai/memory`.
pub fn default_dir() -> Result<PathBuf> {
    store_dir("AI_MEMORY_DIR", "memory")
}

impl Cli {
    /// Runs against the default memory, printing to stdout.
    pub fn exec(self) -> Result<ExitCode> {
        match self.run(&default_dir()?, &mut io::stdout().lock()) {
            Err(e) if broken_pipe(&e) => Ok(ExitCode::SUCCESS),
            r => r,
        }
    }

    pub fn run(self, dir: &Path, out: &mut dyn Write) -> Result<ExitCode> {
        self.run_with(dir, out, &Runtime::default())
    }

    pub fn run_with(self, dir: &Path, out: &mut dyn Write, rt: &Runtime) -> Result<ExitCode> {
        if let Command::Init = self.command {
            return init(dir, out);
        }
        let s = Store::open(dir)?;
        let cwd = std::env::current_dir().ok();
        let mut place = cwd.as_deref().map(prov::place).unwrap_or_default();
        if let (Command::Note { .. }, Some(cwd)) = (&self.command, &cwd) {
            place.root = prov::root(cwd);
        }
        let mut who = prov::env_who();
        let (cmd, mut args) = self.command.typed();
        let mut out = Tally::new(out);
        let quiet = matches!(self.command, Command::PromptHook);
        let r = match self.command {
            Command::PromptHook => hook(&mut out, (rt.stdin)(), dir, &mut who).map(|rule| {
                args.extend(rule.into_iter().flat_map(|r| [REMINDED.into(), r.into()]));
                ExitCode::SUCCESS
            }),
            c => c.run(&s, &mut out, rt, &place, &who),
        };
        let error = match &r {
            Ok(c) if *c == ExitCode::SUCCESS => None,
            Ok(_) => Some("exit 1".into()),
            Err(e) => Some(format!("{e:#}")),
        };
        let _ = usage::record(dir, &Use::new(cmd, args, &place, &who, &out, error));
        if quiet { Ok(ExitCode::SUCCESS) } else { r }
    }
}

impl Command {
    fn run(
        self,
        s: &Store,
        out: &mut dyn Write,
        rt: &Runtime,
        place: &Place,
        who: &Who,
    ) -> Result<ExitCode> {
        match self {
            Command::Init | Command::PromptHook => unreachable!(),
            Command::Wake { from, meta } => wake(s, out, from.unwrap_or(0), &meta),
            Command::Note { text } => note(s, out, &text, rt, place, who),
            Command::Summarise => summarise(s, out, rt),
            Command::Grep(g) => grep(&s.snapshot()?, out, &g),
            Command::Zoom { id, part, meta } => {
                zoom_cmd(&s.snapshot()?, out, &id, part.unwrap_or(1), &meta)
            }
            Command::Show { id } => show(&s.snapshot()?, out, &id),
            Command::Config { sets } => config(s, out, &sets),
            Command::Import { file } => import(s, out, &file),
            Command::Stats(st) => stats(&s.snapshot()?, s.dir(), out, &st),
        }
    }
}

pub fn broken_pipe(e: &anyhow::Error) -> bool {
    e.chain()
        .filter_map(|c| c.downcast_ref::<io::Error>())
        .any(|e| e.kind() == io::ErrorKind::BrokenPipe)
}

pub fn plural(n: u64, word: &str) -> String {
    if n == 1 {
        return format!("1 {word}");
    }
    let word = match word.strip_suffix('y') {
        Some(stem) => format!("{stem}ie"),
        None if word.ends_with(['s', 'h', 'x']) => format!("{word}e"),
        None => word.to_string(),
    };
    format!("{n} {word}s")
}

/// A printed line id, `id+n` or a bare `id` for one message, that names a line within `t` messages.
pub fn line_id(s: &str, t: u64) -> Result<Coord> {
    let Some((id, n)) = Coord::parse(s) else {
        bail!("'{s}' is not a line id. Copy one from the view, like 2184+8 or 7.");
    };
    Coord::at(id, n, t).with_context(|| format!("No line {s}."))
}

fn check(text: &str, limit: u64) -> Result<&str> {
    let text = text.trim();
    if text.is_empty() {
        bail!("Empty. A memory is one line of text.");
    }
    if text.contains(['\n', '\r']) {
        bail!(
            "{} lines. A memory is one line: merge them, or note them separately.",
            text.matches('\n').count() + 1
        );
    }
    let n = text.len() as u64;
    if n > limit {
        bail!(
            "Too long: {n} bytes, limit {limit}. Accented characters cost 2 bytes. Compress it, or split it into separate notes."
        );
    }
    Ok(text)
}

fn init(dir: &Path, out: &mut dyn Write) -> Result<ExitCode> {
    let (s, fresh) = Store::create(dir)?;
    let at = pretty(dir);
    if fresh {
        writeln!(out, "Created {at}: your memory.")?;
    } else {
        let n = s.snapshot()?.log_len()?;
        writeln!(out, "Found {at}: {}.", plural(n, "message"))?;
    }
    writeln!(
        out,
        "Sizes live in {at}/config as ai.memory.*; the defaults are fine.\n"
    )?;
    writeln!(
        out,
        "Paste this at the top of your agent's AGENTS.md (or CLAUDE.md), done:\n"
    )?;
    let block = TEMPLATE
        .replace("{memo}", ME)
        .replace("{data}", &at)
        .replace("{chars}", &s.cfg.get(Knob::EntryChars).to_string());
    writeln!(out, "{block}")?;
    Ok(ExitCode::SUCCESS)
}

/// Printed at the top of every part of a read that takes more than one.
const IN_PARTS: &str = "Your memory comes in parts, oldest first. Run the command at the end of each part until you see \"You are awake.\"";

fn wake(s: &Store, out: &mut dyn Write, from: u64, meta: &Meta) -> Result<ExitCode> {
    let snap = s.snapshot()?;
    let t = snap.log_len()?;
    if t == 0 {
        writeln!(
            out,
            "No memories yet. Record the first with: {ME} note \"<one line>\""
        )?;
        writeln!(out, "You are awake.")?;
        return Ok(ExitCode::SUCCESS);
    }
    if from >= t {
        bail!(
            "No message {from}: the memory has {}. Run: {ME} wake",
            plural(t, "message")
        );
    }
    let mut lines = Vec::new();
    for c in cover::from(&cover(t, s.cfg.get(Knob::WakeLines)), from) {
        wake_lines(&snap, c, meta, &mut lines)?;
    }
    let mut parts = parts(
        lines,
        s.cfg.get(Knob::PartLines),
        s.cfg.get(Knob::PartChars),
    )
    .into_iter();
    let part = parts.next().unwrap_or_default();
    let more = parts.next().is_some();
    if from > 0 || more {
        writeln!(out, "{IN_PARTS}")?;
    }
    for l in dated(&part) {
        writeln!(out, "{l}")?;
    }
    match part.last().filter(|_| more) {
        Some(l) => writeln!(
            out,
            "Not awake yet. Run: {ME} wake {}{}",
            l.c.end(),
            meta.flags()
        )?,
        None => writeln!(
            out,
            "You are awake. Zoom the marked lines that touch your task before you act."
        )?,
    }
    Ok(ExitCode::SUCCESS)
}

/// Line `c`, or its halves' lines while its summary is not built; tool calls have none.
fn wake_lines(snap: &Snapshot, c: Coord, meta: &Meta, out: &mut Vec<Line>) -> Result<()> {
    let text = match snap.node(c)? {
        Some(n) if n.text.is_empty() => return Ok(()),
        Some(n) => n.text,
        None if c.l == 0 => PLACEHOLDER.into(),
        None => {
            for k in c.children() {
                wake_lines(snap, k, meta, out)?;
            }
            return Ok(());
        }
    };
    out.push(meta.line_at(snap, c, &text)?);
    Ok(())
}

fn note(
    s: &Store,
    out: &mut dyn Write,
    text: &str,
    rt: &Runtime,
    place: &Place,
    who: &Who,
) -> Result<ExitCode> {
    let text = check(text, s.cfg.get(Knob::EntryChars))?;
    let m = Message {
        place: place.clone(),
        who: who.clone(),
        ..Message::new(Kind::Note, text)
    };
    let (i, _, _) = s.append("note", &[m])?;
    writeln!(out, "Saved as {i}.")?;
    if rt.summarise_on_note && pending(s)? {
        spawn_summariser("memory", "AI_MEMORY_DIR", s.dir(), s.dir())?;
    }
    Ok(ExitCode::SUCCESS)
}

/// The compactor's lock is taken: another summariser or chat is building, and will see the new messages.
const BUSY: &str = "A compactor is already running; it will build these too.";

/// Whether a node can be built now on the latest snapshot.
fn pending(s: &Store) -> Result<bool> {
    let mem = Mem::load(&s.snapshot()?, VIEW)?;
    Ok(!mem.due(|_| false, 1).is_empty())
}

fn summarise(s: &Store, out: &mut dyn Write, rt: &Runtime) -> Result<ExitCode> {
    let _log = log::init(&s.dir().join(log::SUMMARISE_LOG))?;
    let _span = info_span!(
        "summarise",
        pid = process::id(),
        model = %claude::model(),
        dir = ?pretty(s.dir())
    )
    .entered();
    let start = Instant::now();
    info!("start");
    let mut spent = Spend::default();
    let e = match rounds(s, out, rt, &mut spent) {
        Ok(built) => {
            log::spend!(info, spent, { built, secs = log::secs(start.elapsed()) }, "end");
            return Ok(ExitCode::SUCCESS);
        }
        Err(e) => e,
    };
    let Some(g) = e.downcast_ref::<GaveUp>() else {
        log::spend!(error, spent, { error = format!("{e:#}") }, "failed");
        return Err(e);
    };
    log::spend!(error, spent, { node = %g.node, tries = g.tries, error = g.error }, "gave up");
    writeln!(
        out,
        "{g}. It stays pending; the summariser after the next note tries again."
    )?;
    Ok(ExitCode::FAILURE)
}

/// Compactor rounds until nothing is due or another compactor holds the lock; the nodes built.
/// What their requests took goes into `spent`, failed or not.
fn rounds(s: &Store, out: &mut dyn Write, rt: &Runtime, spent: &mut Spend) -> Result<u64> {
    let mut built = 0;
    while pending(s)? {
        let backend = (rt.backend)(&s.dir().join(CALLS))?;
        let Some(mut c) = Compactor::new(s, backend, rt.opts)? else {
            writeln!(out, "{BUSY}")?;
            info!("busy: another compactor holds the lock; exiting");
            return Ok(built);
        };
        let retries = rt.opts.retries;
        let ran = c.run(|c| {
            for r in c.take_reports() {
                writeln!(
                    out,
                    "Failed {r}. Retrying up to {retries} times, waiting longer each time."
                )?;
            }
            Ok(())
        });
        *spent += c.spent();
        ran?;
        built += c.built();
        (rt.before_release)();
        drop(c);
    }
    match built {
        0 => writeln!(out, "Nothing to build.")?,
        n => writeln!(out, "Built {}.", plural(n, "summary"))?,
    }
    Ok(built)
}

fn config(s: &Store, out: &mut dyn Write, sets: &[String]) -> Result<ExitCode> {
    let mut cfg = s.cfg.clone();
    for a in sets {
        let knob = a
            .split_once('=')
            .and_then(|(k, v)| Some((Knob::parse(&k.trim().to_uppercase())?, v.trim())));
        let Some((k, v)) = knob else {
            bail!(
                "usage: {ME} config [NAME=VALUE ...]   # NAME one of {}",
                Knob::names()
            );
        };
        cfg.set(
            k,
            if v.is_empty() {
                None
            } else {
                Some(k.validate(v, k.name())?)
            },
        );
    }
    if !sets.is_empty() {
        s.save_config(&cfg)?;
    }
    for k in Knob::ALL {
        let note = match cfg.overridden(k) {
            Some(_) => format!(" (default {})", k.default()),
            None => String::new(),
        };
        writeln!(out, "{:<12} {:<7} {}{note}", k.name(), cfg.get(k), k.what())?;
    }
    Ok(ExitCode::SUCCESS)
}

fn zoom_cmd(
    s: &Snapshot,
    out: &mut dyn Write,
    id: &str,
    part: u64,
    meta: &Meta,
) -> Result<ExitCode> {
    let c = line_id(id, s.log_len()?)?;
    let Some(text) = zoom(s, c.id(), c.n(), Some(meta))? else {
        bail!("No line {c}.");
    };
    let (text, n) = page(&text, s.cfg().get(Knob::PartChars), part)?;
    writeln!(out, "{}", text.strip_suffix('\n').unwrap_or(text))?;
    if part < n {
        writeln!(
            out,
            "Part {part} of {n}. Next: {ME} zoom {c} {}{}",
            part + 1,
            meta.flags()
        )?;
    }
    Ok(ExitCode::SUCCESS)
}

fn show(s: &Snapshot, out: &mut dyn Write, id: &str) -> Result<ExitCode> {
    let c = line_id(id, s.log_len()?)?;
    let (fields, text): (Vec<(&str, String)>, String) = if c.l == 0 {
        let m = s.message(c.i)?;
        let (p, w) = (m.place, m.who);
        (
            vec![
                ("ts", m.ts),
                ("kind", m.kind.name().into()),
                ("origin", m.origin),
                ("repo", p.repo),
                ("root", p.root),
                ("head", p.head),
                ("branch", p.branch),
                ("agent", w.agent),
                ("model", w.model),
                ("session", w.session),
            ],
            m.text,
        )
    } else {
        let Some(n) = s.node(c)? else {
            bail!("{c} is not summarized yet. Run: {ME} zoom {c}");
        };
        (
            vec![
                ("ts", n.ts),
                ("origin", n.origin),
                ("agent", n.who.agent),
                ("model", n.who.model),
                ("session", n.who.session),
            ],
            n.text,
        )
    };
    writeln!(out, "{c}")?;
    for (k, v) in fields {
        writeln!(out, "{k:<8}{}", dash(&v))?;
    }
    writeln!(out, "text    {text}")?;
    Ok(ExitCode::SUCCESS)
}

fn is_iso_date(d: &str) -> bool {
    d.len() == 10
        && d.bytes().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
}

fn import(s: &Store, out: &mut dyn Write, file: &Path) -> Result<ExitCode> {
    let Ok(src) = String::from_utf8(fs::read(file).at(file)?) else {
        bail!(
            "{} is not UTF-8 text. Convert it, then import again.",
            pretty(file)
        );
    };
    let mut last = String::new();
    let mut items = Vec::new();
    for (i, line) in (1..).zip(src.lines()) {
        if line.trim().is_empty() {
            continue;
        }
        let (date, text) = line.split_once(' ').unwrap_or((line, ""));
        if !is_iso_date(date) {
            bail!("line {i}: expected 'YYYY-MM-DD <text>', got: {line}");
        }
        let Ok(day) = date.parse::<jiff::civil::Date>() else {
            bail!("line {i}: {date} is not a real date.");
        };
        if *date < *last {
            bail!("line {i}: date {date} precedes the previous line's ({last}).");
        }
        let text = text.trim();
        if text.is_empty() {
            bail!("line {i}: no text.");
        }
        last = date.to_string();
        items.push(Message {
            ts: midnight(day),
            ..Message::new(Kind::Note, text)
        });
    }
    if items.is_empty() {
        bail!("{} has no notes.", file.display());
    }
    let (base, _, _) = s.append("import", &items)?;
    let k = items.len() as u64;
    writeln!(
        out,
        "Imported {}, {base} to {}.",
        plural(k, "note"),
        base + k - 1
    )?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plurals() {
        assert_eq!(plural(1, "message"), "1 message");
        assert_eq!(plural(2, "memory"), "2 memories");
        assert_eq!(plural(5, "match"), "5 matches");
        assert_eq!(plural(0, "part"), "0 parts");
    }

    #[test]
    fn line_ids() {
        assert_eq!(line_id("16+16", 32).unwrap(), Coord::new(4, 1));
        assert_eq!(line_id("7", 8).unwrap(), Coord::leaf(7));
        assert_eq!(line_id("7+1", 8).unwrap(), Coord::leaf(7));
        for bad in ["16+16", "3+2", "4+3", "x", "#7", "7-8", "31+1", "31"] {
            assert!(line_id(bad, 31).is_err(), "{bad}");
        }
        let c = Coord::leaf(7);
        assert_eq!(
            line_id(&c.to_string(), 8).unwrap(),
            c,
            "a printed id reads back"
        );
    }
}
