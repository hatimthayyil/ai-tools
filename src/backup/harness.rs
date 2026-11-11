//! The harnesses whose files `ai backup` backs up, and where each keeps them.

/// Whether a harness keeps a folder or a single file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Dir,
    /// A file its harness writes in place, such as a SQLite database: backed up as a copy made
    /// first ([`super::db`]), never as the file itself.
    File,
}

/// A harness and the folders and files of its that are backed up.
pub struct Harness {
    pub name: &'static str,
    /// The harness's folder under `$HOME`.
    pub default: &'static str,
    /// Its folders and files, under its folder.
    pub places: &'static [(Kind, &'static str)],
}

/// Every harness whose files are backed up.
pub const HARNESSES: &[Harness] = &[
    Harness {
        name: "claude",
        default: ".claude",
        places: &[(Kind::Dir, "projects")],
    },
    Harness {
        name: "codex",
        default: ".codex",
        places: &[(Kind::Dir, "sessions"), (Kind::Dir, "archived_sessions")],
    },
    Harness {
        name: "pi",
        default: ".pi/agent",
        places: &[(Kind::Dir, "sessions")],
    },
    Harness {
        name: "dsh",
        default: ".dsh",
        places: &[(Kind::Dir, "sessions"), (Kind::Dir, "attachments")],
    },
    Harness {
        name: "gemini",
        default: ".gemini",
        places: &[(Kind::Dir, "tmp")],
    },
    Harness {
        name: "opencode",
        default: ".local/share/opencode",
        places: &[(Kind::Dir, "storage"), (Kind::File, "opencode.db")],
    },
    Harness {
        name: "hermes",
        default: ".hermes",
        places: &[(Kind::Dir, "sessions"), (Kind::File, "state.db")],
    },
];
