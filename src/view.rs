use std::{
    env, fmt,
    io::{self, IsTerminal, Write},
    path::Path,
    process::ExitCode,
    str::FromStr,
};

use anyhow::{Context, Result, anyhow};
use clap::Args;

use crate::{chat, memory, page};

/// `ai view`: the memory's whole view, read as `ai memory view`, then a thread's view
/// (`chat::thread_view`); whole on a terminal, else in parts from one state of both.
#[derive(Args, Debug)]
pub struct Cli {
    #[arg(help = chat::THREAD_HELP)]
    thread: Option<String>,
    #[arg(long, hide = true, help = "the part of the view to print, as printed")]
    part: Option<u64>,
    #[arg(long, hide = true, help = "the state the parts come from, as printed")]
    at: Option<String>,
    #[arg(
        long,
        hide = true,
        help = "plumbing: the whole view at once, not in parts"
    )]
    whole: bool,
    #[command(flatten)]
    session: chat::Session,
}

/// A state of `ai view`, printed for `--at`: the memory's, then the name and state of the thread
/// it shows, if any, `:`-separated.
struct Mark {
    memory: memory::Mark,
    thread: Option<(String, chat::Mark)>,
}

impl fmt::Display for Mark {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.memory)?;
        match &self.thread {
            Some((name, mark)) => write!(f, ":{name}:{mark}"),
            None => Ok(()),
        }
    }
}

impl FromStr for Mark {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Mark> {
        let bad = || anyhow!("'{s}' is not a view state. Copy it as printed.");
        let parts: Vec<&str> = s.split(':').collect();
        let memory = parts[0].parse().map_err(|_| bad())?;
        let thread = match parts[1..] {
            [] => None,
            [name, mark] => Some((name.to_string(), mark.parse().map_err(|_| bad())?)),
            _ => return Err(bad()),
        };
        Ok(Mark { memory, thread })
    }
}

impl Cli {
    /// Runs against the default memory, in the current directory, printing to stdout.
    pub fn exec(self) -> Result<ExitCode> {
        let cwd = env::current_dir().context("Cannot read the current directory.")?;
        let terminal = io::stdout().is_terminal();
        match self.run(
            &memory::default_dir()?,
            &cwd,
            &mut io::stdout().lock(),
            terminal,
        ) {
            Err(e) if memory::broken_pipe(&e) => Ok(ExitCode::SUCCESS),
            r => r,
        }
    }

    /// Runs against the memory at `memory`, in `cwd`, printing to `out`; whole on a `terminal`.
    pub fn run(
        self,
        memory: &Path,
        cwd: &Path,
        out: &mut dyn Write,
        terminal: bool,
    ) -> Result<ExitCode> {
        let at: Option<Mark> = self.at.as_deref().map(str::parse).transpose()?;
        let thread = match at.as_ref().map(|at| at.thread.clone()) {
            Some(None) => None,
            Some(Some((name, mark))) => {
                chat::thread_view(cwd, Some(name), self.session, Some(mark))?
            }
            None => chat::thread_view(cwd, self.thread, self.session, None)?,
        };
        let mem = match &at {
            Some(at) => at.memory,
            None => memory::Store::open(memory)?.snapshot()?.mark(),
        };
        let mut text = b"# Memory\n\n".to_vec();
        let woke = memory::Cli::view_whole(mem).run(memory, &mut text)?;
        if woke != ExitCode::SUCCESS {
            out.write_all(&text)?;
            return Ok(woke);
        }
        let mut text = String::from_utf8(text).context("The memory's view is not UTF-8.")?;
        let thread = thread.map(|(view, name, mark)| {
            text += &format!("\n{view}");
            (name, mark)
        });
        let mark = Mark {
            memory: mem,
            thread,
        };
        page::print(
            out,
            &text,
            page::pick(self.part, self.whole || terminal),
            |k| format!("ai view --part {k} --at {mark}"),
        )?;
        Ok(ExitCode::SUCCESS)
    }
}
