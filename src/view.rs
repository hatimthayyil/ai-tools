use std::{
    env,
    io::{self, IsTerminal, Write},
    path::Path,
    process::ExitCode,
};

use anyhow::{Context, Result};
use clap::Args;

use crate::{agent, config::Settings, memory, page};

/// `ai view`: the memory's whole view, read as `ai memory view`, then a thread's view
/// (`agent::thread_view`); whole on a terminal, else in parts read on with `ai page`, cut
/// between memory lines and thread entries.
#[derive(Args, Debug)]
pub struct Cli {
    #[arg(help = agent::AGENT_HELP)]
    thread: Option<String>,
    #[arg(
        long,
        hide = true,
        help = "plumbing: the whole view at once, not in parts"
    )]
    whole: bool,
    #[command(flatten)]
    session: agent::Session,
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
        let thread = agent::thread_view(cwd, self.thread, self.session)?;
        let rt = memory::Runtime {
            config: Settings::find(cwd)?.memory,
            ..memory::Runtime::default()
        };
        let mut text = b"# Memory\n\n".to_vec();
        let woke = memory::Cli::view_whole().run_with(memory, &mut text, &rt)?;
        if woke != ExitCode::SUCCESS {
            out.write_all(&text)?;
            return Ok(woke);
        }
        let text = String::from_utf8(text).context("The memory's view is not UTF-8.")?;
        let mut entries: Vec<String> = text.split_inclusive('\n').map(String::from).collect();
        let mut command = "ai view".to_string();
        if let Some((view, name)) = thread {
            entries.extend(view.into_iter().enumerate().map(|(k, e)| match k {
                0 => format!("\n{e}"),
                _ => e,
            }));
            command += &format!(" {name}");
        }
        page::print_entries(
            out,
            &page::cache()?,
            &command,
            &entries,
            self.whole || terminal,
        )?;
        Ok(ExitCode::SUCCESS)
    }
}
