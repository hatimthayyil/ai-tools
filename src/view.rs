use std::{
    env,
    io::{self, Write},
    path::Path,
    process::ExitCode,
};

use anyhow::{Context, Result};
use clap::Args;

use crate::{chat, memory};

/// `ai view`: the memory's whole view, read as `ai memory view`, then a thread's view
/// (`chat::thread_view`).
#[derive(Args, Debug)]
pub struct Cli {
    #[arg(help = chat::THREAD_HELP)]
    thread: Option<String>,
    #[command(flatten)]
    session: chat::Session,
}

impl Cli {
    /// Runs against the default memory, in the current directory, printing to stdout.
    pub fn exec(self) -> Result<ExitCode> {
        let cwd = env::current_dir().context("Cannot read the current directory.")?;
        match self.run(&memory::default_dir()?, &cwd, &mut io::stdout().lock()) {
            Err(e) if memory::broken_pipe(&e) => Ok(ExitCode::SUCCESS),
            r => r,
        }
    }

    pub fn run(self, memory: &Path, cwd: &Path, out: &mut dyn Write) -> Result<ExitCode> {
        let thread = chat::thread_view(cwd, self.thread, self.session)?;
        writeln!(out, "# Memory\n")?;
        let woke = memory::Cli::view_whole().run(memory, out)?;
        if woke != ExitCode::SUCCESS {
            return Ok(woke);
        }
        if let Some(view) = thread {
            write!(out, "\n{view}")?;
        }
        Ok(ExitCode::SUCCESS)
    }
}
