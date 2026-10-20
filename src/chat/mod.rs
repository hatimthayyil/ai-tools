pub mod claude;
mod cli;
pub mod id;
pub mod model;
mod record;
pub mod render;
pub mod rollup;
mod source;
mod store;
mod summarise;
mod sync;
mod view;

pub use cli::{Cli, NOT_ATTACHED};
pub use id::Id;
pub use model::{Item, Kind, Marker, Place, SourceId, Tree, Turn, turns};
pub use record::{Agent, Binding, Head, Key, Line, LineKind};
pub use rollup::{Branch, Part, Rollup, Source};
pub use store::{
    Added, Chat, FORMAT, Mark, Snapshot, Thread, ThreadInfo, Transcript, base_dir, check_name,
};
pub use summarise::{
    Built, Config, Dry, DryLine, OnFail, Rewrote, build, dry_run, rebuild, rewrite, summarise,
};
pub use sync::{Attached, Synced, read, sync, sync_in};
pub use view::{FOLD_LINES, OPEN, RAIL};
