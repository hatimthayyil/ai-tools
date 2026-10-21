use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ai", version, about = "All-in-one AI tool.")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Permanent memory for AI agents.")]
    Memory(ai_tools::memory::Cli),
    #[command(about = "A global knowledge store for AI agents.")]
    Notes(ai_tools::notes::Cli),
    #[command(about = "Named conversations that span agent sessions, kept in this repository.")]
    Chat(ai_tools::chat::Cli),
    #[command(about = "Summaries of a project's files, rolled up its directory tree.")]
    Files(ai_tools::files::Cli),
    #[command(about = "Your whole memory, then a thread's view: one read to wake on.")]
    View(ai_tools::view::Cli),
}

fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Command::Memory(cli) => cli.exec(),
        Command::Notes(cli) => cli.exec(),
        Command::Chat(cli) => cli.exec(),
        Command::Files(cli) => cli.exec(),
        Command::View(cli) => cli.exec(),
    };
    result.unwrap_or_else(|e| {
        eprintln!("{e}");
        ExitCode::FAILURE
    })
}
