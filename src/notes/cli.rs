use std::{
    io::{self, Write},
    path::Path,
    process::{self, ExitCode},
    thread,
    time::Instant,
};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use tracing::{error, info, info_span, warn};

use crate::{
    config::Settings,
    file::pretty,
    git::hash,
    memory::{
        Backend, CALLS, MakeBackend, PLACEHOLDER, Pattern, broken_pipe, excerpt, log, plural,
        spawn_summariser, store_dir,
    },
    notes::{
        leaf::{Confidence, Front, Leaf, check_body, check_title, is_id, new_id},
        store::{ME, Node, SUMMARY, Store},
        summary::{Job, Summary, due, input, only_child, summarise},
        topic::Topic,
    },
    summariser,
};

const DIR_VAR: &str = "AI_NOTES_DIR";
/// The most `view` prints, in bytes.
pub const VIEW_MAX: usize = 8000;

#[derive(Parser, Debug)]
#[command(
    name = "notes",
    about = "A global knowledge store for AI agents: claims filed in a tree of summarised topics.",
    after_help = "The notes live in ~/.ai/notes, or in $AI_NOTES_DIR if set."
)]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    #[command(about = "print the top of the tree: each topic's scope (the default).")]
    View,
    #[command(about = "open a topic: its subtopics' scope and gist, and its notes.")]
    Zoom {
        #[arg(help = "a topic, as printed: linguistics.syntax")]
        topic: String,
    },
    #[command(about = "print one note whole, wherever it sits.")]
    Show {
        #[arg(help = "a note's id, as printed: 20261013T221500")]
        id: String,
    },
    #[command(about = "search every note and summary.")]
    Grep(Pattern),
    #[command(about = "file one claim under a topic.")]
    Add(Add),
    #[command(about = "move a note (by id) or a topic (by name) to another topic.")]
    Mv { from: String, to: String },
    #[command(about = "link a note to another: a weak cross-topic reference.")]
    Link { from: String, to: String },
    #[command(about = "rebuild every summary out of date, with Claude Code.")]
    Build,
}

#[derive(Args, Debug)]
struct Add {
    #[arg(help = "the topic to file it under: projects.ai-tools, physics.qm")]
    topic: String,
    #[arg(
        allow_hyphen_values = true,
        help = "the claim, at most 1024 characters; read from stdin if absent or -"
    )]
    text: Option<String>,
    #[arg(
        long,
        help = "the claim in a few words, at most 80 characters: \"Merge is binary\""
    )]
    title: String,
    #[arg(long, value_enum, help = "how far it can be trusted")]
    confidence: Confidence,
    #[arg(long, help = "where it comes from: a URL, file or session")]
    source: Option<String>,
    #[arg(long, value_name = "YYYY-MM-DD", help = "when it was last verified")]
    checked: Option<jiff::civil::Date>,
    #[arg(long, help = "how fast its subject goes stale")]
    decay: Option<String>,
    #[arg(
        long = "link",
        value_name = "ID",
        help = "a note it points to; repeatable"
    )]
    links: Vec<String>,
}

/// How commands reach the summariser's model and stdin; tests swap in their own.
pub struct Runtime {
    pub backend: Box<MakeBackend>,
    /// What `ai.toml` sets for the summariser, which `backend` makes.
    pub summariser: summariser::Config,
    /// Whether changes start a background `build`.
    pub summarise_on_change: bool,
    pub stdin: Box<dyn Fn() -> io::Result<String>>,
}

impl Default for Runtime {
    fn default() -> Runtime {
        Runtime {
            backend: summariser::Config::default().maker(),
            summariser: summariser::Config::default(),
            summarise_on_change: std::env::var_os("AI_NOTES_SUMMARISE").is_none_or(|v| v != "0"),
            stdin: Box::new(|| io::read_to_string(io::stdin())),
        }
    }
}

impl Cli {
    /// Runs against the default store, with the summariser `ai.toml` sets here, printing to
    /// stdout.
    pub fn exec(self) -> Result<ExitCode> {
        let cwd = std::env::current_dir().context("Cannot read the current directory.")?;
        let summariser = Settings::find(&cwd)?.summariser;
        let rt = Runtime {
            backend: summariser.clone().maker(),
            summariser,
            ..Runtime::default()
        };
        match self.run_with(&store_dir(DIR_VAR, "notes")?, &mut io::stdout().lock(), &rt) {
            Err(e) if broken_pipe(&e) => Ok(ExitCode::SUCCESS),
            r => r,
        }
    }

    pub fn run(self, dir: &Path, out: &mut dyn Write) -> Result<ExitCode> {
        self.run_with(dir, out, &Runtime::default())
    }

    pub fn run_with(self, dir: &Path, out: &mut dyn Write, rt: &Runtime) -> Result<ExitCode> {
        let s = Store::new(dir);
        match self.command.unwrap_or(Command::View) {
            Command::View => view(&s.load()?, out),
            Command::Zoom { topic } => zoom(&s.load()?, out, &Topic::parse(&topic)?),
            Command::Show { id } => show(&s.load()?, out, &id),
            Command::Grep(p) => grep(&s.load()?, out, &p),
            Command::Add(a) => add(&s, out, rt, a),
            Command::Mv { from, to } => mv(&s, out, rt, &from, &to),
            Command::Link { from, to } => link(&s, out, &from, &to),
            Command::Build => build_cmd(&s, out, rt),
        }
    }
}

fn summarise_after(s: &Store, rt: &Runtime) -> Result<()> {
    if rt.summarise_on_change && !due(&s.load()?)?.is_empty() {
        spawn_summariser("notes", DIR_VAR, s.dir(), &s.git_dir())?;
    }
    Ok(())
}

/// `topic (n): scope`, the line of a topic in `view` and `zoom`.
fn line(n: &Node) -> String {
    let scope = n.summary.as_ref().map_or(PLACEHOLDER, |s| s.scope.as_str());
    format!("{} ({}): {scope}", n.topic, n.count())
}

/// `n`'s one subtopic, when `n`'s summary is a copy of it.
fn collapsed(n: &Node) -> Option<&Node> {
    let [k] = n.kids.as_slice() else {
        return None;
    };
    let (Some(s), Some(c)) = (&n.summary, only_child(n)) else {
        return None;
    };
    (s.scope == c.scope && s.gist == c.gist).then_some(k)
}

/// The view's lines under `n`, with their depth: a chain of copied summaries is one line,
/// named by its deepest topic.
fn rows(n: &Node, depth: usize, out: &mut Vec<(usize, String)>) {
    let mut shown = n;
    while let Some(k) = collapsed(shown) {
        shown = k;
    }
    out.push((depth, line(shown)));
    for k in &shown.kids {
        rows(k, depth + 1, out);
    }
}

fn view(root: &Node, out: &mut dyn Write) -> Result<ExitCode> {
    if root.kids.is_empty() {
        writeln!(
            out,
            "No notes yet. File the first with: {ME} add <topic> \"<claim>\" --title <title> --confidence <level>"
        )?;
        return Ok(ExitCode::SUCCESS);
    }
    let mut nodes = Vec::new();
    for k in &root.kids {
        rows(k, 1, &mut nodes);
    }
    let deepest = nodes.iter().map(|(d, _)| *d).max().unwrap_or(1);
    let size = |d: usize| -> usize {
        nodes
            .iter()
            .filter(|(k, _)| *k <= d)
            .map(|(k, l)| 2 * (k - 1) + l.len() + 1)
            .sum()
    };
    let depth = (2..=deepest)
        .take_while(|&d| size(d) <= VIEW_MAX)
        .last()
        .unwrap_or(1);
    let (mut used, mut hidden) = (0, 0);
    for (d, l) in nodes.iter().filter(|(d, _)| *d <= depth) {
        let text = format!("{}{l}", "  ".repeat(d - 1));
        if used + text.len() + 1 > VIEW_MAX {
            hidden += 1;
            continue;
        }
        used += text.len() + 1;
        writeln!(out, "{text}")?;
    }
    if hidden > 0 {
        writeln!(
            out,
            "… {} more. Search them: {ME} grep <regex>",
            plural(hidden, "topic")
        )?;
    } else if depth < deepest {
        writeln!(out, "Deeper topics below. Open one: {ME} zoom <topic>")?;
    }
    Ok(ExitCode::SUCCESS)
}

fn zoom(root: &Node, out: &mut dyn Write, t: &Topic) -> Result<ExitCode> {
    let Some(n) = root.find(t) else {
        bail!("No topic {t}. Run: {ME}");
    };
    let block = |n: &Node| match &n.summary {
        Some(s) => format!("{}\n{}", line(n), s.gist),
        None => line(n),
    };
    let mut parts = vec![block(n)];
    parts.extend(n.kids.iter().map(block));
    let notes: Vec<String> = n
        .leaves
        .iter()
        .map(|l| {
            let c = l.confidence().unwrap_or("unrated");
            format!("{} [{c}] {}", l.id, excerpt(l.label(), None))
        })
        .collect();
    if !notes.is_empty() {
        parts.push(notes.join("\n"));
    }
    writeln!(out, "{}", parts.join("\n\n"))?;
    Ok(ExitCode::SUCCESS)
}

fn find<'a>(root: &'a Node, id: &str) -> Result<&'a Leaf> {
    if !is_id(id) {
        bail!("'{id}' is not a note id. Copy one as printed, like 20261013T221500.");
    }
    match root.leaf(id) {
        Some(l) => Ok(l),
        None => bail!("No note {id}."),
    }
}

fn show(root: &Node, out: &mut dyn Write, id: &str) -> Result<ExitCode> {
    let l = find(root, id)?;
    writeln!(out, "{}", l.rel().display())?;
    write!(out, "{}", l.render())?;
    let links = l.links();
    if !links.is_empty() {
        writeln!(out, "\nLinks:")?;
    }
    for id in links {
        match root.leaf(&id) {
            Some(to) => writeln!(
                out,
                "{} › {id}: {}",
                to.topic.crumbs(),
                excerpt(to.label(), None)
            )?,
            None => writeln!(out, "{id}: no such note")?,
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn grep(root: &Node, out: &mut dyn Write, p: &Pattern) -> Result<ExitCode> {
    let re = p.regex()?;
    let mut found: Vec<(&Topic, bool, String)> = Vec::new();
    root.walk(&mut |n| {
        if let Some(s) = &n.summary {
            let text = format!("{}\n{}", s.scope, s.gist);
            if re.is_match(&text) {
                let line = format!("{}: {}", n.topic.crumbs(), excerpt(&text, Some(&re)));
                found.push((&n.topic, false, line));
            }
        }
        let hit = |l: &&Leaf| re.is_match(&l.title) || re.is_match(&l.body);
        for l in n.leaves.iter().filter(hit) {
            let at = n.topic.crumbs();
            let line = format!("{at} › {}: {}", l.id, excerpt(l.label(), None));
            found.push((&n.topic, true, line));
        }
    });
    let deeper = |t: &Topic| {
        found
            .iter()
            .any(|(k, leaf, _)| k.within(t) && (*leaf || *k != t))
    };
    let hits: Vec<&str> = found
        .iter()
        .filter(|(t, leaf, _)| *leaf || !deeper(t))
        .map(|(_, _, l)| l.as_str())
        .collect();
    if hits.is_empty() {
        writeln!(out, "No match.")?;
    } else {
        writeln!(out, "{}", hits.join("\n"))?;
    }
    Ok(ExitCode::SUCCESS)
}

/// A frontmatter value: one line.
fn one_line<'a>(flag: &str, v: &'a str) -> Result<&'a str> {
    let v = v.trim();
    if v.is_empty() || v.contains(['\n', '\r']) {
        bail!("--{flag} is one line of text, not '{v}'.");
    }
    Ok(v)
}

fn add(s: &Store, out: &mut dyn Write, rt: &Runtime, a: Add) -> Result<ExitCode> {
    let topic = Topic::parse(&a.topic)?;
    let text = match a.text.as_deref() {
        None | Some("-") => (rt.stdin)()?,
        Some(t) => t.to_string(),
    };
    let body = check_body(&text)?;
    let title = check_title(&a.title)?;
    let mut front = Front::default();
    if let Some(v) = &a.source {
        front.set("source", one_line("source", v)?.into());
    }
    front.set("confidence", a.confidence.name().into());
    if let Some(d) = a.checked {
        front.set("checked", d.to_string());
    }
    if let Some(v) = &a.decay {
        front.set("decay", one_line("decay", v)?.into());
    }
    let id = s.mutate(&format!("add {topic}"), |root| {
        for l in &a.links {
            find(root, l)?;
        }
        let mut links = a.links.clone();
        links.dedup();
        let mut front = front.clone();
        if !links.is_empty() {
            front.set_list("links", &links);
        }
        let leaf = Leaf::new(new_id(&root.ids()), topic.clone(), front, title, body);
        s.put_leaf(&leaf)?;
        Ok(leaf.id)
    })?;
    writeln!(out, "Saved as {id} in {topic}.")?;
    summarise_after(s, rt)?;
    Ok(ExitCode::SUCCESS)
}

fn mv(s: &Store, out: &mut dyn Write, rt: &Runtime, from: &str, to: &str) -> Result<ExitCode> {
    let dest = Topic::parse(to)?;
    s.mutate(&format!("mv {from} {dest}"), |root| {
        if is_id(from) {
            let l = find(root, from)?;
            if l.topic == dest {
                bail!("{from} is in {dest} already.");
            }
            s.rename(&l.rel(), &dest.rel().join(l.file_name()))?;
            return s.prune(&l.topic);
        }
        let t = Topic::parse(from)?;
        if root.find(&t).is_none() {
            bail!("No topic {t}. Run: {ME}");
        }
        if dest.within(&t) {
            bail!("Cannot move {t} into itself.");
        }
        if s.path(&dest.rel()).exists() {
            bail!(
                "{dest} exists. Move {t} to a new topic, like {dest}.{}",
                t.name()
            );
        }
        s.rename(&t.rel(), &dest.rel())?;
        s.prune(&t.parent().expect("not the root"))
    })?;
    writeln!(out, "Moved {from} to {dest}.")?;
    summarise_after(s, rt)?;
    Ok(ExitCode::SUCCESS)
}

fn link(s: &Store, out: &mut dyn Write, from: &str, to: &str) -> Result<ExitCode> {
    let added = s.mutate(&format!("link {from} {to}"), |root| {
        let mut l = find(root, from)?.clone();
        find(root, to)?;
        if from == to {
            bail!("A note cannot link to itself.");
        }
        let mut links = l.front.list("links");
        if links.iter().any(|k| k == to) {
            return Ok(false);
        }
        links.push(to.into());
        l.front.set_list("links", &links);
        s.put_leaf(&l)?;
        Ok(true)
    })?;
    match added {
        true => writeln!(out, "Linked {from} to {to}.")?,
        false => writeln!(out, "{from} links to {to} already.")?,
    }
    Ok(ExitCode::SUCCESS)
}

/// Another summariser holds the lock; it will see these changes too.
const BUSY: &str = "A summariser is already running; it will build these too.";

fn build_cmd(s: &Store, out: &mut dyn Write, rt: &Runtime) -> Result<ExitCode> {
    if !s.exists() {
        writeln!(out, "Nothing to build.")?;
        return Ok(ExitCode::SUCCESS);
    }
    let _log = log::init(&s.git_dir().join(log::SUMMARISE_LOG))?;
    let _span = info_span!(
        "summarise",
        pid = process::id(),
        model = %rt.summariser.model(),
        dir = ?pretty(s.dir())
    )
    .entered();
    let start = Instant::now();
    info!("start");
    match rounds(s, out, rt) {
        Ok(built) => {
            info!(built, secs = log::secs(start.elapsed()), "end");
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            error!(error = format!("{e:#}"), "failed");
            Err(e.context(
                "Summaries stay out of date; the summariser after the next change tries again",
            ))
        }
    }
}

/// Rounds of summaries, deepest first, until none is out of date or another summariser holds the lock.
fn rounds(s: &Store, out: &mut dyn Write, rt: &Runtime) -> Result<u64> {
    let mut built = 0;
    while !due(&s.load()?)?.is_empty() {
        let Some(lock) = s.summarise_lock()? else {
            writeln!(out, "{BUSY}")?;
            info!("busy: another summariser holds the lock; exiting");
            return Ok(built);
        };
        let backend = (rt.backend)(&s.git_dir().join(CALLS))?;
        loop {
            let jobs = due(&s.load()?)?;
            if jobs.is_empty() {
                break;
            }
            for batch in jobs.chunks(backend.jobs()) {
                built += round(s, &*backend, batch)?;
            }
        }
        backend.stop();
        drop(lock);
    }
    match built {
        0 => writeln!(out, "Nothing to build.")?,
        n => writeln!(out, "Built {}.", plural(n, "summary"))?,
    }
    Ok(built)
}

/// Makes the summaries of `jobs` at once, then writes each whose input is unchanged.
fn round(s: &Store, backend: &dyn Backend, jobs: &[Job]) -> Result<u64> {
    let made: Vec<Result<Option<(String, String)>>> = thread::scope(|sc| {
        let calls: Vec<_> = jobs
            .iter()
            .map(|j| {
                sc.spawn(|| match (&j.copy, &j.input) {
                    (Some(c), _) => Ok(Some(c.clone())),
                    (None, Some(i)) => summarise(backend, i).map(Some),
                    (None, None) => Ok(None),
                })
            })
            .collect();
        calls
            .into_iter()
            .map(|c| c.join().expect("a summary call panicked"))
            .collect()
    });
    let mut built = 0;
    for (job, made) in jobs.iter().zip(made) {
        let made = made?;
        let t = &job.topic;
        let wrote = s.mutate(&format!("summary {t}"), |root| {
            let Some(n) = root.find(t) else {
                return Ok(false);
            };
            let now = input(n);
            if now != job.input {
                return Ok(false);
            }
            let rel = t.rel().join(SUMMARY);
            match (now, &made) {
                (Some(i), Some((scope, gist))) => {
                    let sum = Summary {
                        hash: hash(&i)?,
                        scope: scope.clone(),
                        gist: gist.clone(),
                    };
                    s.write(&rel, &sum.render())?;
                }
                _ => s.remove(&rel)?,
            }
            Ok(true)
        })?;
        match wrote {
            true => {
                info!(topic = %t, "built");
                built += u64::from(made.is_some());
            }
            false => warn!(topic = %t, "changed while summarising; redone next round"),
        }
    }
    Ok(built)
}
