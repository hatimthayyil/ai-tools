# main.rs

Top-level clap dispatcher for memory, notes, agent, resume, view and init. Each module owns its CLI and returns an exit code or error; `view` composes memory and agent (view.rs.md). `ai resume` is `ai agent resume` under a shorter name: both parse `agent::Resume` and run the same `resume`; only its one-line about differs, in the top-level style.
