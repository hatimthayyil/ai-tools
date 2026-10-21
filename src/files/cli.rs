use std::{
    collections::{HashMap, HashSet},
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    thread,
};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};

use crate::{
    file::{AtPath, pretty},
    files::{
        Node, Store,
        summarise::{self, Child, Summary},
        walk,
    },
    git::hash,
    memory::{Backend, CALLS, ClaudeCode, JOBS, Spend, broken_pipe, log::usd, plural, store_dir},
    progress,
    project::Project,
};

#[derive(Parser, Debug)]
#[command(
    name = "files",
    about = "Summaries of a project's files, rolled up its directory tree.",
    after_help = "A directory is summarised from its children's summaries. What git ignores and \
                  files that are not text are left out. Summaries live in \
                  ~/.ai/files/<root commit>, or under $AI_FILES_DIR if set."
)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    #[command(
        about = "summarise what has changed since the last build, each directory after its children, stopping at the first that fails."
    )]
    Build,
    #[command(about = "print the summary of a file or directory, by default the project's.")]
    View {
        #[arg(help = "a file or directory of the project")]
        path: Option<PathBuf>,
    },
}

/// `$AI_FILES_DIR`, else `~/.ai/files`: every project's summaries, named by its id.
pub fn base_dir() -> Result<PathBuf> {
    store_dir("AI_FILES_DIR", "files")
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

    /// Runs in `cwd`, printing to `out`.
    pub fn run(self, cwd: &Path, out: &mut dyn Write) -> Result<ExitCode> {
        let (root, store) = open(cwd)?;
        match self.command {
            Command::Build => build(&root, &store, out),
            Command::View { path } => view(cwd, &root, &store, path.as_deref(), out),
        }
    }
}

/// The work tree of the project at or above `cwd`, and its summaries.
fn open(cwd: &Path) -> Result<(PathBuf, Store)> {
    let p = Project::find(cwd).with_context(|| match gix::discover(cwd) {
        Ok(_) => {
            "This repository has no commits yet. Its summaries are kept under its first \
             commit; commit once, then retry."
        }
        Err(_) => "Not in a git repository. Run this inside the project to summarise.",
    })?;
    let root = p
        .repo
        .workdir()
        .context("This repository has no work tree to summarise.")?
        .to_path_buf();
    Ok((root, Store::open(&base_dir()?.join(&p.id))?))
}

/// Summarises every node of the project at `root` whose hash has no summary, deepest first, up
/// to [`JOBS`] calls at once. The first failure stops it, once the calls beside it end; what
/// they wrote is kept.
fn build(root: &Path, store: &Store, out: &mut dyn Write) -> Result<ExitCode> {
    let Some(_lock) = store.build_lock()? else {
        bail!("Another ai files build is running on this project.");
    };
    let bar = progress::bar("summaries", "reading the files");
    let tree = Node::Dir(walk(root, &[])?);
    if matches!(&tree, Node::Dir(d) if d.kids.is_empty()) {
        bar.finish_and_clear();
        bail!("No text files to summarise in {}.", pretty(root));
    }
    let mut known: HashMap<String, String> = store
        .load()?
        .into_iter()
        .map(|(k, s)| (k, s.text))
        .collect();
    bar.set_length(missing(&tree, &known, &mut HashSet::new()));
    let backend = ClaudeCode::compactor(&store.dir().join(CALLS))?;
    let (mut built, mut spent) = (0, Spend::default());
    loop {
        let mut now = Vec::new();
        ready(&tree, &known, &mut HashSet::new(), &mut now);
        if now.is_empty() {
            break;
        }
        for batch in now.chunks(JOBS) {
            bar.set_message(format!("{} running", plural(batch.len() as u64, "call")));
            let made = make(&backend, root, batch, &known);
            let mut failed = None;
            for (n, (made, s)) in batch.iter().zip(made) {
                spent += s;
                match made {
                    Ok(m) => {
                        store.put(n.hash(), &m.who, &m.text)?;
                        known.insert(n.hash().into(), m.text);
                        built += 1;
                        bar.inc(1);
                    }
                    Err(e) => {
                        failed.get_or_insert((shown(n.path()).to_string(), e));
                    }
                }
            }
            if let Some((path, e)) = failed {
                bar.finish_and_clear();
                return Err(anyhow!(
                    "Stopped at {path}: {e:#}\nThe {} written so far are kept; the next ai files \
                     build goes on from them.",
                    plural(built, "summary")
                ));
            }
        }
    }
    bar.finish_and_clear();
    match built {
        0 => writeln!(out, "Nothing to build.")?,
        n => writeln!(
            out,
            "Wrote {} in {} to the model, ${}.",
            plural(n, "summary"),
            plural(spent.requests, "request"),
            usd(spent.all.cost)
        )?,
    }
    Ok(ExitCode::SUCCESS)
}

/// The summaries of `batch`, made at once, each with what its requests took.
fn make(
    backend: &dyn Backend,
    root: &Path,
    batch: &[&Node],
    known: &HashMap<String, String>,
) -> Vec<(Result<Summary>, Spend)> {
    thread::scope(|sc| {
        let calls: Vec<_> = batch
            .iter()
            .map(|n| {
                sc.spawn(move || {
                    let mut s = Spend::default();
                    (summary(backend, root, n, known, &mut s), s)
                })
            })
            .collect();
        calls
            .into_iter()
            .map(|c| c.join().expect("a summary call panicked"))
            .collect()
    })
}

/// The summary of `n`: of a file's text, or of a directory's children's summaries in `known`.
fn summary(
    backend: &dyn Backend,
    root: &Path,
    n: &Node,
    known: &HashMap<String, String>,
    spent: &mut Spend,
) -> Result<Summary> {
    match n {
        Node::File(f) => {
            let p = root.join(&f.path);
            let text = fs::read_to_string(&p).at(&p)?;
            if hash(&text)? != f.hash {
                bail!("it changed during the build; run ai files build again.");
            }
            summarise::file(backend, &f.path, &text, spent)
        }
        Node::Dir(d) => {
            let kids: Vec<Child> = d
                .kids
                .iter()
                .map(|k| Child {
                    path: k.path().into(),
                    summary: known[k.hash()].clone(),
                })
                .collect();
            summarise::dir(backend, shown(&d.path), &kids, spent)
        }
    }
}

/// How many of the nodes from `n` down have no summary in `known`, counting each hash once.
fn missing<'a>(n: &'a Node, known: &HashMap<String, String>, seen: &mut HashSet<&'a str>) -> u64 {
    if known.contains_key(n.hash()) || !seen.insert(n.hash()) {
        return 0;
    }
    match n {
        Node::File(_) => 1,
        Node::Dir(d) => 1 + d.kids.iter().map(|k| missing(k, known, seen)).sum::<u64>(),
    }
}

/// The nodes from `n` down with no summary in `known` that can be summarised now: files, and
/// directories whose children all have one; each hash once.
fn ready<'a>(
    n: &'a Node,
    known: &HashMap<String, String>,
    seen: &mut HashSet<&'a str>,
    now: &mut Vec<&'a Node>,
) {
    if known.contains_key(n.hash()) {
        return;
    }
    match n {
        Node::Dir(d) if !d.kids.iter().all(|k| known.contains_key(k.hash())) => {
            for k in &d.kids {
                ready(k, known, seen, now);
            }
        }
        _ if seen.insert(n.hash()) => now.push(n),
        _ => {}
    }
}

/// Prints the summary of `path`, given from `cwd`, else of the project at `root`, as the files
/// are now.
fn view(
    cwd: &Path,
    root: &Path,
    store: &Store,
    path: Option<&Path>,
    out: &mut dyn Write,
) -> Result<ExitCode> {
    let rel = match path {
        Some(p) => relative(cwd, root, p)?,
        None => String::new(),
    };
    let tree = Node::Dir(walk(root, &[])?);
    let n = find(&tree, &rel).with_context(|| {
        format!(
            "ai files leaves {} out: git ignores it, or it holds no text.",
            shown(&rel)
        )
    })?;
    let all = store.load()?;
    let s = all.get(n.hash()).with_context(|| {
        format!(
            "{} has no summary as it is now. Run ai files build.",
            shown(&rel)
        )
    })?;
    writeln!(out, "{}", s.text.trim_end())?;
    Ok(ExitCode::SUCCESS)
}

/// `p`, given from `cwd`, as a path of the project at `root`: `/`-separated, `""` for the root.
fn relative(cwd: &Path, root: &Path, p: &Path) -> Result<String> {
    let abs = cwd.join(p);
    let abs = abs.canonicalize().at(&abs)?;
    let top = root.canonicalize().at(root)?;
    let rel = abs.strip_prefix(&top).map_err(|_| {
        anyhow!(
            "{} is outside the project at {}.",
            p.display(),
            pretty(&top)
        )
    })?;
    let names = rel
        .iter()
        .map(|n| n.to_str())
        .collect::<Option<Vec<_>>>()
        .with_context(|| format!("{} is not a UTF-8 path.", p.display()))?;
    Ok(names.join("/"))
}

/// The node at `path` from `n` down.
fn find<'a>(n: &'a Node, path: &str) -> Option<&'a Node> {
    if n.path() == path {
        return Some(n);
    }
    let Node::Dir(d) = n else {
        return None;
    };
    d.kids
        .iter()
        .find(|k| {
            path.strip_prefix(k.path())
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        })
        .and_then(|k| find(k, path))
}

/// `path` as the user names it: `.` for the root.
fn shown(path: &str) -> &str {
    match path {
        "" => ".",
        p => p,
    }
}
