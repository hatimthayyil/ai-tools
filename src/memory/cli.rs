use std::{
    fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    process::{self, ExitCode},
    time::Instant,
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use tracing::{info, info_span};

use crate::{
    config::{self as settings, Settings},
    file::{AtPath, pretty},
    memory::{
        backend::MakeBackend,
        claude::CALLS,
        compact::{Compactor, Event, GaveUp, Options, Spend},
        config::{Config, Knob, note_limit},
        cover::cover,
        grep::{Grep, grep},
        hook::{REMINDED, hook},
        log,
        meta::{Line, Meta, dash, dated},
        prov,
        record::{Kind, Message, Node, Place, Who, midnight},
        stats::{Stats, stats},
        store::{ME, Snapshot, Store},
        tree::{Coord, LEVELS, ancestors, overlapping},
        usage::{self, Tally, Use},
        view::{Mem, PLACEHOLDER, VIEW},
        zoom::zoom,
    },
    page,
    progress::{self, About, Level, Mode, Past, Progress},
    project::Project,
    summariser,
};

/// The view's opening: what the memory is and how to read it; `{memo}` is [`ME`].
const HEADER: &str = include_str!("prompts/view.txt");

/// The view's last line; `ai view` moves it after the thread.
pub const AWAKE: &str = "You are awake.\n";

const TEMPLATE: &str = "\
## Memory

`{memo}` is your memory. It outlives every session, compaction,
model and vendor change. Without it you do not know who you are, what
was decided, or what was tried.

At the start of every session and after every compaction, run
`{memo} view` before you do anything else, and read the whole view,
to its last line, \"You are awake.\" Skip this only if your context
already holds the whole view. If your brief says not to run
`{memo}`, ignore this section.

The memory view records the past. Rules and preferences in it still
hold; a request made for one task was for that task. The user's latest
words override it.

Once you have understood something and acted on it, note what you or
the user will still need in months, with
`{memo} note \"<one line, at most {chars} characters>\"`:
- a decision the user made or approved, and its reason;
- a rule or preference the user states for future work;
- a fact the user teaches you, or anything that comes up about their
  life, even in passing;
- what a task of real effort found, or a failure a later session could
  repeat, and its cause.

Name what a later search would look for: projects, files, commands,
people. Leave out the time, machine, repo, branch, agent, model and
session, which the tool records, and whether work is pushed or merged.
Name a repo only when the note is about another one. To correct a
note, note the correction.

A fork ends its report with the notes it registered; do not register
them again. When you spawn a fresh subagent, write: `You are a
subagent. Don't run {memo}.`";

#[derive(Parser, Debug)]
#[command(
    name = "memory",
    about = "A permanent, append-only memory for AI agents.",
    after_help = "The memories live in ~/.ai/memory, or in $AI_MEMORY_DIR if set."
)]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    #[command(about = "create this memory; print the setup block.")]
    Init,
    #[command(about = "read your memory: print the view (the default).")]
    View {
        #[arg(
            long,
            hide = true,
            help = "plumbing: the whole view at once, not in parts"
        )]
        whole: bool,
        #[arg(long, value_name = "N", help = "read tree N, not the current one")]
        tree: Option<u64>,
        #[command(flatten)]
        meta: Meta,
    },
    #[command(about = "record one memory: one short line.")]
    Note {
        #[arg(allow_hyphen_values = true)]
        text: String,
    },
    #[command(
        about = "build every summary that can be built now, with Claude Code.",
        after_help = "It builds the tree being built (see `tree rebuild`), else the current one. It \
                      prints its plan first: per level of the tree, the summaries it holds, those \
                      written and those to write, with the calls, cost and time that takes at the \
                      average of past calls. On a terminal it then shows a bar per level; off one, \
                      a line per summary. The full record is in summarise.log."
    )]
    Build {
        #[arg(long, help = "print the plan only; call no model and store nothing")]
        dry_run: bool,
    },
    #[command(
        about = "list the memory's trees, or change them: rebuild, reroll, use, drop, diff.",
        after_help = "The current tree is what view, zoom and grep read. A rebuild writes into \
                      a copy of it, the tree being built, which stays until `tree use` makes it \
                      current. Old trees are kept until dropped."
    )]
    Tree {
        #[command(subcommand)]
        cmd: Option<TreeCmd>,
    },
    #[command(about = "search every message ever recorded, newest first.")]
    Grep(Grep),
    #[command(about = "open a line of the view into the four lines two levels under it.")]
    Zoom {
        #[arg(help = "a line's id, as printed: 2184+8, or 7 for message 7")]
        id: String,
        #[arg(long, value_name = "N", help = "read tree N, not the current one")]
        tree: Option<u64>,
        #[command(flatten)]
        meta: Meta,
    },
    #[command(about = "show where one message or summary came from.")]
    Show {
        #[arg(help = "a line's id, as printed: 2184+8, or 7 for message 7")]
        id: String,
    },
    #[command(
        about = "show the memory's sizes.",
        after_help = "Set them under [memory] in ~/.ai/ai.toml, a project's ai.toml, or its \
                      ai.local.toml."
    )]
    Config,
    #[command(about = "bulk-load dated notes (bootstrap only).")]
    Import { file: PathBuf },
    #[command(about = "count how often agents view, note, zoom, grep and show.")]
    Stats(Stats),
    #[command(
        hide = true,
        about = "plumbing: the prompt hook for agent harnesses, a memory reminder on long prompts and every few short ones."
    )]
    PromptHook,
}

#[derive(Subcommand, Debug)]
enum TreeCmd {
    #[command(
        about = "mark notes stale in the tree being built, and the summaries above them",
        after_help = "A range `a..b` covers notes a up to b, b not included; a line id like \
                      2184+8 covers the notes under that summary; a bare id covers one note. \
                      Notes are never rebuilt: the summaries above them are. With no range, every \
                      note. Summaries that are not built yet are left for `build`."
    )]
    Rebuild {
        #[arg(
            value_name = "RANGE",
            help = "notes a..b, or a line id; every note if omitted"
        )]
        range: Option<String>,
    },
    #[command(
        about = "mark one summary stale, keeping its children: rebuilt from them, as a re-roll",
        after_help = "The summaries above it are marked stale too. Build then rewrites them."
    )]
    Reroll {
        #[arg(help = "a summary's line id, like 2184+8")]
        id: String,
    },
    #[command(
        about = "make tree N the current tree, once it is built",
        after_help = "An older tree is copied first: the copy is built with what its notes need, \
                      then made current, and the old tree stays as it was."
    )]
    Use {
        #[arg(help = "a tree's number, as `tree` lists it")]
        n: u64,
    },
    #[command(
        about = "drop tree N: the current tree cannot be dropped; the tree being built is cancelled"
    )]
    Drop {
        #[arg(help = "a tree's number, as `tree` lists it")]
        n: u64,
    },
    #[command(about = "show the summaries that differ between two trees")]
    Diff {
        #[arg(help = "a tree's number")]
        a: u64,
        #[arg(help = "another tree's number")]
        b: u64,
    },
}

impl TreeCmd {
    /// Its name and arguments, as typed.
    fn typed(&self) -> Vec<String> {
        match self {
            TreeCmd::Rebuild { range } => ["rebuild".into()]
                .into_iter()
                .chain(range.clone())
                .collect(),
            TreeCmd::Reroll { id } => vec!["reroll".into(), id.clone()],
            TreeCmd::Use { n } => vec!["use".into(), n.to_string()],
            TreeCmd::Drop { n } => vec!["drop".into(), n.to_string()],
            TreeCmd::Diff { a, b } => vec!["diff".into(), a.to_string(), b.to_string()],
        }
    }
}

impl Command {
    /// Its name and arguments, as typed; a note's text is in the store.
    fn typed(&self) -> (&'static str, Vec<String>) {
        match self {
            Command::Init => ("init", Vec::new()),
            Command::View { whole, tree, meta } => (
                "view",
                whole
                    .then(|| "--whole".into())
                    .into_iter()
                    .chain(tree.map(|n| format!("--tree={n}")))
                    .chain(meta.args())
                    .collect(),
            ),
            Command::Note { .. } => ("note", Vec::new()),
            Command::Build { dry_run } => (
                "build",
                dry_run.then(|| "--dry-run".into()).into_iter().collect(),
            ),
            Command::Tree { cmd } => ("tree", cmd.as_ref().map(TreeCmd::typed).unwrap_or_default()),
            Command::Grep(g) => ("grep", g.typed()),
            Command::Zoom { id, tree, meta } => (
                "zoom",
                std::iter::once(id.clone())
                    .chain(tree.map(|n| format!("--tree={n}")))
                    .chain(meta.args())
                    .collect(),
            ),
            Command::Show { id } => ("show", vec![id.clone()]),
            Command::Config => ("config", Vec::new()),
            Command::Import { file } => ("import", vec![file.display().to_string()]),
            Command::Stats(st) => ("stats", st.args()),
            Command::PromptHook => ("prompt-hook", Vec::new()),
        }
    }
}

/// How commands reach the compactor's model, stdin and stdout; tests swap in their own.
pub struct Runtime {
    pub backend: Box<MakeBackend>,
    /// Whether stdout is a terminal, where views print whole.
    pub terminal: bool,
    /// Whether stderr is a terminal, where `build` draws its bars.
    pub bars: bool,
    pub opts: Options,
    /// The sizes `ai.toml` sets.
    pub config: Config,
    /// What `ai.toml` sets for the summariser, which `backend` makes.
    pub summariser: summariser::Config,
    /// Whether `note` starts a background `build`.
    pub summarise_on_note: bool,
    /// Called by `build` when a round is done, while it still holds the compactor lock.
    pub before_release: Box<dyn Fn()>,
    pub stdin: Box<dyn Fn() -> io::Result<String>>,
    /// Where paged output is saved for `ai page`.
    pub views: Box<dyn Fn() -> Result<PathBuf>>,
}

impl Default for Runtime {
    fn default() -> Runtime {
        Runtime {
            backend: summariser::Config::default().maker(),
            terminal: false,
            bars: false,
            opts: Options::default(),
            config: Config::default(),
            summariser: summariser::Config::default(),
            summarise_on_note: std::env::var_os("AI_MEMORY_SUMMARISE").is_none_or(|v| v != "0"),
            before_release: Box::new(|| {}),
            stdin: Box::new(|| io::read_to_string(io::stdin())),
            views: Box::new(page::cache),
        }
    }
}

/// Starts `ai <tool> build` on the store at `dir` (named to it by `$var`), detached; it logs to
/// `summarise.log` in `log_dir`, its stderr too.
pub fn spawn_summariser(tool: &str, var: &str, dir: &Path, log_dir: &Path) -> Result<()> {
    log::spawn(&log_dir.join(log::SUMMARISE_LOG), |c| {
        c.args([tool, "build"]).env(var, dir)
    })
}

/// `$var`, else `~/.ai/<name>`.
pub fn store_dir(var: &str, name: &str) -> Result<PathBuf> {
    if let Some(d) = std::env::var_os(var).filter(|d| !d.is_empty()) {
        return Ok(d.into());
    }
    let home = settings::home()
        .with_context(|| format!("No home directory. Point {var} at the {name}."))?;
    Ok(home.join(name))
}

/// `$AI_MEMORY_DIR`, else `~/.ai/memory`.
pub fn default_dir() -> Result<PathBuf> {
    store_dir("AI_MEMORY_DIR", "memory")
}

impl Cli {
    /// `view --whole`: the whole view, read and recorded as any view.
    pub fn view_whole() -> Cli {
        Cli {
            command: Some(Command::View {
                whole: true,
                tree: None,
                meta: Meta::default(),
            }),
        }
    }

    /// Runs against the default memory, with the sizes `ai.toml` sets here, printing to stdout.
    pub fn exec(self) -> Result<ExitCode> {
        let cwd = std::env::current_dir().context("Cannot read the current directory.")?;
        let settings = Settings::find(&cwd)?;
        let rt = Runtime {
            backend: settings.summariser.clone().maker(),
            terminal: io::stdout().is_terminal(),
            bars: progress::terminal(),
            config: settings.memory,
            opts: Options {
                jobs: settings.summariser.jobs(),
                ..Options::default()
            },
            summariser: settings.summariser,
            ..Runtime::default()
        };
        match self.run_with(&default_dir()?, &mut io::stdout().lock(), &rt) {
            Err(e) if broken_pipe(&e) => Ok(ExitCode::SUCCESS),
            r => r,
        }
    }

    pub fn run_with(self, dir: &Path, out: &mut dyn Write, rt: &Runtime) -> Result<ExitCode> {
        let command = self.command.unwrap_or(Command::View {
            whole: false,
            tree: None,
            meta: Meta::default(),
        });
        if let Command::Init = command {
            return init(dir, out, &rt.config);
        }
        let s = Store::open(dir)?;
        let cwd = std::env::current_dir().ok();
        let mut place = cwd.as_deref().map(prov::place).unwrap_or_default();
        if let (Command::Note { .. }, Some(cwd)) = (&command, &cwd) {
            place.project = Project::find(cwd)?.map(|p| p.id).unwrap_or_default();
        }
        let mut who = prov::env_who();
        let (cmd, mut args) = command.typed();
        let mut out = Tally::new(out);
        let quiet = matches!(command, Command::PromptHook);
        let r = match command {
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
            Command::View { whole, tree, meta } => {
                view(s, out, rt, whole || rt.terminal, tree, &meta)
            }
            Command::Note { text } => note(s, out, &text, rt, place, who),
            Command::Build { dry_run } => match dry_run {
                true => plan(s, out, rt),
                false => build(s, out, rt),
            },
            Command::Tree { cmd } => tree(s, out, cmd),
            Command::Grep(g) => grep(s, out, &g, rt.terminal, &(rt.views)()?),
            Command::Zoom { id, tree, meta } => zoom_cmd(&read_tree(s, tree)?, out, &id, rt, &meta),
            Command::Show { id } => show(&s.snapshot()?, out, &id),
            Command::Config => config(out, &rt.config),
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

fn check(text: &str, target: u64) -> Result<&str> {
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
    let (n, limit) = (text.chars().count() as u64, note_limit(target));
    if n > limit {
        bail!(
            "Too long: {n} characters, limit {limit}. Aim for {target}. Compress it, or split it into separate notes."
        );
    }
    Ok(text)
}

fn init(dir: &Path, out: &mut dyn Write, cfg: &Config) -> Result<ExitCode> {
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
        "Sizes live under [memory] in ~/.ai/{}; the defaults are fine.\n",
        settings::FILE
    )?;
    writeln!(
        out,
        "Paste this at the top of your agent's AGENTS.md (or CLAUDE.md), done:\n"
    )?;
    let block = TEMPLATE
        .replace("{memo}", ME)
        .replace("{chars}", &cfg.get(Knob::EntryChars).to_string());
    writeln!(out, "{block}")?;
    Ok(ExitCode::SUCCESS)
}

/// The memory's view: whole when `whole`, else in parts read on with `ai page`.
fn view(
    s: &Store,
    out: &mut dyn Write,
    rt: &Runtime,
    whole: bool,
    tree: Option<u64>,
    meta: &Meta,
) -> Result<ExitCode> {
    let text = view_text(&read_tree(s, tree)?, rt.config.get(Knob::ViewLines), meta)?;
    let command = format!("{ME} view{}", meta.flags());
    page::print(out, &(rt.views)()?, &command, &text, whole)?;
    Ok(ExitCode::SUCCESS)
}

/// The memory as of the latest commit, reading tree `n` if given, else the current one.
fn read_tree(s: &Store, n: Option<u64>) -> Result<Snapshot<'_>> {
    let snap = s.snapshot()?;
    match n {
        Some(n) => snap.tree(n),
        None => Ok(snap),
    }
}

/// The view's opening, its `n` lines under their date headers, then [`AWAKE`].
fn view_text(snap: &Snapshot, n: u64, meta: &Meta) -> Result<String> {
    let t = snap.log_len()?;
    if t == 0 {
        return Ok(format!(
            "No memories yet. Record the first with: {ME} note \"<one line>\"\n{AWAKE}"
        ));
    }
    let mut lines = Vec::new();
    for c in cover(t, n) {
        view_lines(snap, c, meta, &mut lines)?;
    }
    let mut text = HEADER.replace("{memo}", ME) + "\n";
    text.extend(dated(&lines).into_iter().map(|l| l + "\n"));
    text += AWAKE;
    Ok(text)
}

/// Line `c`, or its halves' lines while its summary is not built; tool calls have none.
fn view_lines(snap: &Snapshot, c: Coord, meta: &Meta, out: &mut Vec<Line>) -> Result<()> {
    let text = match snap.node(c)? {
        Some(n) if n.text.is_empty() => return Ok(()),
        Some(n) => n.text,
        None if c.l == 0 => PLACEHOLDER.into(),
        None => {
            for k in c.children() {
                view_lines(snap, k, meta, out)?;
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
    let text = check(text, rt.config.get(Knob::EntryChars))?;
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

/// Whether a node can be built now in the tree `build` writes.
fn pending(s: &Store) -> Result<bool> {
    let mem = Mem::load(&s.working()?, VIEW)?;
    Ok(!mem.due(|_| false, 1).is_empty())
}

/// What `build` shows its progress about: what it does, with what, and the past calls to
/// estimate from.
fn about(s: &Store, rt: &Runtime) -> Result<About> {
    let log = s.dir().join(log::SUMMARISE_LOG);
    let title = match s.working()?.state().building {
        Some(n) => format!("Rebuilding tree {n}"),
        None => "Building memory".into(),
    };
    Ok(About {
        title,
        unit: "summary",
        covers: |l| plural(1 << l, "message"),
        first_bar: 1,
        model: rt.summariser.model(),
        jobs: rt.opts.jobs,
        past: Past::read(&log, |id| Coord::parse(id).map(|(_, n)| n.trailing_zeros()))?,
        log,
    })
}

/// Each level of a tree over `t` messages, lowest first, with `built(l)` of its nodes built.
fn tree_levels(t: u64, built: impl Fn(u32) -> u64) -> Vec<Level> {
    (0..u64::BITS)
        .take_while(|&l| 1u64 << l <= t)
        .map(|l| {
            let (lines, written) = (t >> l, built(l));
            Level {
                lines,
                written,
                left: lines.saturating_sub(written),
            }
        })
        .collect()
}

/// The levels of `m`'s tree, with the summaries that are fresh written.
fn mem_levels(m: &Mem) -> Vec<Level> {
    tree_levels(m.t(), |l| m.fresh_at(l))
}

/// `build --dry-run`: the plan of `build`, and nothing else.
fn plan(s: &Store, out: &mut dyn Write, rt: &Runtime) -> Result<ExitCode> {
    let levels = mem_levels(&Mem::load(&s.working()?, VIEW)?);
    let progress = Progress::new(&mut *out, Mode::Plain, about(s, rt)?);
    if !progress.plan(&levels)? {
        drop(progress);
        writeln!(out, "Nothing to build.")?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Builds every node that is due in the tree being built, else the current one. A run that gives
/// up leaves what it has not built pending. Shows its plan, then its progress, then what it did.
fn build(s: &Store, out: &mut dyn Write, rt: &Runtime) -> Result<ExitCode> {
    let _log = log::init(&s.dir().join(log::SUMMARISE_LOG))?;
    let _span = info_span!(
        "summarise",
        pid = process::id(),
        model = %rt.summariser.model(),
        dir = ?pretty(s.dir())
    )
    .entered();
    let start = Instant::now();
    info!("start");
    let mode = match rt.bars {
        true => Mode::Bars,
        false => Mode::Plain,
    };
    let progress = Progress::new(&mut *out, mode, about(s, rt)?);
    let mut spent = Spend::default();
    let mut ran = Rounds::default();
    let r = rounds(s, &progress, rt, &mut spent, &mut ran);
    let Err(e) = r else {
        let planned = progress.planned();
        match planned {
            true => progress.end(None, spent)?,
            false => drop(progress),
        }
        if ran.busy {
            writeln!(out, "{BUSY}")?;
        } else if !planned {
            writeln!(out, "Nothing to build.")?;
        }
        if let Some(b) = s.working()?.state().building
            && !ran.busy
            && !pending(s)?
        {
            writeln!(
                out,
                "Tree {b} is built: `ai memory tree use {b}` makes it current."
            )?;
        }
        let (built, secs) = (ran.built, log::secs(start.elapsed()));
        log::spend!(info, spent, { built, secs }, "end");
        return Ok(ExitCode::SUCCESS);
    };
    let Some(g) = e.downcast_ref::<GaveUp>() else {
        log::spend!(error, spent, { error = format!("{e:#}") }, "failed");
        return Err(e);
    };
    log::spend!(error, spent, { node = %g.node, tries = g.tries, error = g.error }, "gave up");
    progress.gave_up(&g.node, g.tries, &g.error)?;
    progress.end(None, spent)?;
    writeln!(
        out,
        "{g}. It stays pending; the summariser after the next note tries again."
    )?;
    Ok(ExitCode::FAILURE)
}

/// What compactor rounds did: the nodes the model built, and whether they stopped at another
/// compactor's lock.
#[derive(Default)]
struct Rounds {
    built: u64,
    busy: bool,
}

/// Compactor rounds until nothing is due or another compactor holds the lock, shown on
/// `progress` and counted into `ran`. What their requests took goes into `spent`, failed or not.
fn rounds(
    s: &Store,
    progress: &Progress,
    rt: &Runtime,
    spent: &mut Spend,
    ran: &mut Rounds,
) -> Result<()> {
    while pending(s)? {
        let backend = (rt.backend)(&s.dir().join(CALLS))?;
        let Some(c) = Compactor::new(s, backend, rt.opts)? else {
            info!("busy: another compactor holds the lock; exiting");
            ran.busy = true;
            return Ok(());
        };
        round(c, progress, rt, spent, ran)?;
    }
    Ok(())
}

/// `tree`: lists the trees, or changes them.
fn tree(s: &Store, out: &mut dyn Write, cmd: Option<TreeCmd>) -> Result<ExitCode> {
    match cmd {
        None => list(s, out)?,
        Some(TreeCmd::Rebuild { range }) => rebuild(s, out, range.as_deref())?,
        Some(TreeCmd::Reroll { id }) => reroll(s, out, &id)?,
        Some(TreeCmd::Use { n }) => use_tree(s, out, n)?,
        Some(TreeCmd::Drop { n }) => drop_tree(s, out, n)?,
        Some(TreeCmd::Diff { a, b }) => diff(s, out, a, b)?,
    }
    Ok(ExitCode::SUCCESS)
}

/// Every tree the memory holds, the current one, the tree being built and the saved ones; for
/// the one being built, what it still needs.
fn list(s: &Store, out: &mut dyn Write) -> Result<()> {
    let st = s.snapshot()?.state();
    for n in 1..=st.last {
        let snap = s.snapshot()?;
        if n != st.current && !snap.has_tree(n)? {
            continue;
        }
        if n == st.current {
            writeln!(out, "tree {n} current")?;
        } else if st.building == Some(n) {
            let mem = Mem::load(&snap.tree(n)?, VIEW)?;
            let due = mem.due(|_| false, usize::MAX).len();
            writeln!(
                out,
                "tree {n} building: {} stale, {due} to build",
                mem.stale_count()
            )?;
        } else {
            writeln!(out, "tree {n} saved")?;
        }
    }
    Ok(())
}

/// The tree being built: the one started already, else a copy of the current tree.
fn building(s: &Store) -> Result<u64> {
    let st = s.snapshot()?.state();
    match st.building {
        Some(n) => Ok(n),
        None => s.start_building(st.current),
    }
}

/// Reports the tree being built after a change to it: what it still needs.
fn report_building(s: &Store, out: &mut dyn Write) -> Result<()> {
    let w = s.working()?;
    let n = w.number();
    let mem = Mem::load(&w, VIEW)?;
    writeln!(
        out,
        "Tree {n} is being built: {} stale, {} to build. Run `ai memory build`, then `ai memory tree use {n}`.",
        mem.stale_count(),
        mem.due(|_| false, usize::MAX).len()
    )?;
    Ok(())
}

/// The notes a `tree rebuild` argument names, `[a, b)`: `a..b`, or the notes under a line id.
fn span(arg: &str, t: u64) -> Result<(u64, u64)> {
    let Some((a, b)) = arg.split_once("..") else {
        let c = line_id(arg, t)?;
        return Ok((c.id(), c.end()));
    };
    let num = |x: &str| {
        x.parse::<u64>()
            .with_context(|| format!("'{arg}' is not a range: write a..b."))
    };
    let (a, b) = (num(a)?, num(b)?);
    if a >= b || b > t {
        bail!("No notes {a}..{b}: the memory holds {t} messages.");
    }
    Ok((a, b))
}

/// `tree rebuild`: marks stale, in the tree being built, the summaries above the notes it names.
fn rebuild(s: &Store, out: &mut dyn Write, arg: Option<&str>) -> Result<()> {
    let t = s.snapshot()?.log_len()?;
    let (a, b) = match arg {
        Some(arg) => span(arg, t)?,
        None => (0, t),
    };
    if t == 0 {
        bail!("No notes yet to rebuild.");
    }
    building(s)?;
    s.mark_stale(&overlapping(a, b, t))?;
    report_building(s, out)
}

/// `tree reroll`: marks one summary stale, and the summaries above it, keeping the ones below.
fn reroll(s: &Store, out: &mut dyn Write, id: &str) -> Result<()> {
    let t = s.snapshot()?.log_len()?;
    let c = line_id(id, t)?;
    if c.l == 0 {
        bail!("{id} is a note, which is never rebuilt. Name a summary, like 2184+8.");
    }
    building(s)?;
    if s.working()?.node(c)?.is_none() {
        bail!("Summary {id} is not built yet: run `ai memory build` first.");
    }
    let nodes: Vec<Coord> = std::iter::once(c).chain(ancestors(c, t)).collect();
    s.mark_stale(&nodes)?;
    report_building(s, out)
}

/// `tree use`: makes tree `n` current once the tree being built is `n` and fully built. An
/// older tree is first copied into a new tree, which is built and then made current.
fn use_tree(s: &Store, out: &mut dyn Write, n: u64) -> Result<()> {
    let st = s.snapshot()?.state();
    if n == st.current {
        writeln!(out, "Tree {n} is already current.")?;
        return Ok(());
    }
    s.snapshot()?.tree(n)?;
    let b = match st.building {
        Some(b) if b != n => bail!(
            "Tree {b} is being built. Build it and use it, or drop it: `ai memory tree drop {b}`."
        ),
        Some(b) => b,
        None => s.start_building(n)?,
    };
    let w = s.working()?;
    let mem = Mem::load(&w, VIEW)?;
    let (stale, due) = (mem.stale_count(), mem.due(|_| false, usize::MAX).len());
    if stale > 0 || due > 0 {
        bail!(
            "Tree {b} is not built: {stale} stale, {due} to build. Run `ai memory build`, then \
             `ai memory tree use {b}`."
        );
    }
    s.switch(b)?;
    match b == n {
        true => writeln!(out, "Tree {b} is current.")?,
        false => writeln!(out, "Tree {b} is current: a copy of tree {n}.")?,
    }
    Ok(())
}

/// `tree drop`: drops tree `n`; the tree being built is cancelled.
fn drop_tree(s: &Store, out: &mut dyn Write, n: u64) -> Result<()> {
    let snap = s.snapshot()?;
    let st = snap.state();
    if n == st.current {
        bail!("Tree {n} is current: make another tree current before dropping it.");
    }
    if !snap.has_tree(n)? {
        bail!("No tree {n}.");
    }
    s.drop_tree(n)?;
    match st.building == Some(n) {
        true => writeln!(out, "Cancelled the rebuild in tree {n}.")?,
        false => writeln!(out, "Dropped tree {n}.")?,
    }
    Ok(())
}

/// `tree diff`: the summaries whose text differs between trees `a` and `b`.
fn diff(s: &Store, out: &mut dyn Write, a: u64, b: u64) -> Result<()> {
    let (sa, sb) = (s.snapshot()?.tree(a)?, s.snapshot()?.tree(b)?);
    let t = sa.log_len()?;
    let text = |level: &[Option<Node>], i: usize| {
        level
            .get(i)
            .and_then(|n| n.as_ref())
            .map(|n| n.text.clone())
    };
    let mut any = false;
    for l in 0..LEVELS {
        if 1u64 << l > t {
            break;
        }
        let (la, lb) = (sa.level(l)?, sb.level(l)?);
        for i in 0..la.len().max(lb.len()) {
            let (ta, tb) = (text(&la, i), text(&lb, i));
            if ta == tb {
                continue;
            }
            any = true;
            writeln!(out, "{}", Coord::new(l, i as u64))?;
            writeln!(
                out,
                "  tree {a}: {}",
                ta.as_deref().unwrap_or("(not built)")
            )?;
            writeln!(
                out,
                "  tree {b}: {}",
                tb.as_deref().unwrap_or("(not built)")
            )?;
        }
    }
    if !any {
        writeln!(out, "No summaries differ.")?;
    }
    Ok(())
}

/// Runs `c` until nothing is due on the latest snapshot, showing on `progress` the plan, the
/// nodes it writes and its calls, then lets its lock go; counts what the model built into
/// `ran`, and what its requests took into `spent`.
fn round(
    mut c: Compactor,
    progress: &Progress,
    rt: &Runtime,
    spent: &mut Spend,
    ran: &mut Rounds,
) -> Result<()> {
    let tries = rt.opts.retries + 1;
    let r = report(&mut c, progress, tries).and_then(|()| c.run(|c| report(c, progress, tries)));
    let shown = report(&mut c, progress, tries);
    *spent += c.spent();
    r?;
    shown?;
    ran.built += c.built();
    (rt.before_release)();
    Ok(())
}

/// Shows on `progress` where `c` stands, its plan first, and what happened to nodes since last
/// asked; a node is tried `tries` times at most.
fn report(c: &mut Compactor, progress: &Progress, tries: usize) -> Result<()> {
    let levels = mem_levels(c.mem());
    progress.plan(&levels)?;
    for e in c.take_events() {
        match e {
            Event::Built {
                node,
                took,
                chars,
                cost,
            } => progress.built(node.l, &node, took, chars, cost)?,
            Event::Joined(node) => progress.joined(node.l, &node)?,
            Event::Failed {
                node,
                error,
                next,
                wait,
                cost,
            } => progress.retry(&node, &error, (next, tries), wait, cost)?,
        }
    }
    let (running, waiting) = c.calls();
    let at = |cs: Vec<Coord>| cs.into_iter().map(|c| c.l).collect::<Vec<u32>>();
    progress.update(&levels, &at(running), &at(waiting))
}

fn config(out: &mut dyn Write, cfg: &Config) -> Result<ExitCode> {
    for k in Knob::ALL {
        let note = match cfg.overridden(k) {
            Some(_) => format!(" (default {})", k.default()),
            None => String::new(),
        };
        writeln!(out, "{:<12} {:<7} {}{note}", k.key(), cfg.get(k), k.what())?;
    }
    Ok(ExitCode::SUCCESS)
}

/// What line `id` of `s` opens into: whole on a terminal, else in parts read on with `ai page`.
fn zoom_cmd(
    s: &Snapshot,
    out: &mut dyn Write,
    id: &str,
    rt: &Runtime,
    meta: &Meta,
) -> Result<ExitCode> {
    let c = line_id(id, s.log_len()?)?;
    let Some(text) = zoom(s, c.id(), c.n(), Some(meta))? else {
        bail!("No line {c}.");
    };
    let command = format!("{ME} zoom {c}{}", meta.flags());
    page::print(out, &(rt.views)()?, &command, &text, rt.terminal)?;
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
                ("project", p.project),
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
