pub mod claude;
mod cli;
pub mod id;
pub mod model;
pub mod record;
pub mod render;
pub mod rollup;
pub mod source;
mod store;
mod summarise;
mod sync;
mod view;

pub use cli::{AGENT_HELP, Cli, NOT_ATTACHED, Resume, Session, thread_view};
pub use id::Id;
pub use model::{Item, Kind, Marker, Place, SourceId, Tree, Turn, turns};
pub use record::{Agent, Binding, Head, Key, Line, LineKind};
pub use rollup::{Branch, Part, Rollup, Source};
pub use source::{Version, everywhere, restore};
pub use store::{
    AGENTS, ATTACHMENTS, Added, DIR, FORMAT, ITEMS, LINES, Mark, SESSIONS, SOURCES, Snapshot,
    Store, THREADS, Thread, ThreadInfo, Transcript, check_name,
};
pub use summarise::{
    Built, Config, OnFail, Preview, PreviewLine, Rewrote, build, plan, preview, rebuild, rewrite,
    summarise,
};
pub use sync::{Attached, Synced, read, sync};
pub use view::{Counts, Table, View};
