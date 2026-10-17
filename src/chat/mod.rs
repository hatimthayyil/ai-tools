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
mod v1;
mod view;

pub use cli::{Cli, NOT_ATTACHED};
pub use id::Id;
pub use model::{Item, Kind, Marker, Place, SourceId, Tree, Turn, turns};
pub use record::{Agent, Binding, Head, Key, Line, LineKind};
pub use rollup::{Branch, Part, Rollup, Source};
pub use store::{Added, Chat, FORMAT, REFS, Snapshot, Thread, ThreadInfo, Transcript, check_name};
pub use summarise::{
    Built, Config, Dry, DryLine, Rewrote, build, dry_run, rebuild, rewrite, summarise,
};
pub use sync::{Attached, Synced, read, sync};
pub use view::{FOLD_LINES, OPEN, RAIL};
