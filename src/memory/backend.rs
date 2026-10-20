use std::{ops::AddAssign, path::Path, sync::Arc};

use anyhow::Result;

/// Makes the backend of a command, keeping its calls in the directory it is given.
pub type MakeBackend = dyn Fn(&Path) -> Result<Arc<dyn Backend>>;

/// One text block of a user message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub text: String,
}

impl Block {
    pub fn new(text: impl Into<String>) -> Block {
        Block { text: text.into() }
    }
}

/// What requests to a model took: tokens read, written to the cache, read from it and written
/// out, and what they cost in US dollars.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    pub input: u64,
    pub cache_write: u64,
    pub cache_read: u64,
    pub output: u64,
    pub cost: f64,
}

impl Usage {
    pub fn tokens(&self) -> u64 {
        self.input + self.cache_write + self.cache_read + self.output
    }
}

impl AddAssign for Usage {
    fn add_assign(&mut self, u: Usage) {
        self.input += u.input;
        self.cache_write += u.cache_write;
        self.cache_read += u.cache_read;
        self.output += u.output;
        self.cost += u.cost;
    }
}

/// A model, reached through some harness or API.
pub trait Backend: Send + Sync {
    /// The harness or API the model runs in, recorded as the agent of what it writes.
    fn agent(&self) -> &str;

    /// The model's name, recorded with what it writes.
    fn model(&self) -> &str;

    /// A new conversation under the system prompt `system`.
    fn open(&self, system: &str) -> Result<Box<dyn Conversation>>;

    /// Ends every conversation still open.
    fn stop(&self) {}
}

/// One conversation: each user message gets the model's reply, in the same context.
pub trait Conversation: Send {
    fn say(&mut self, message: &[Block]) -> Result<String>;

    /// The id the harness gave this conversation, once known; empty if none.
    fn session(&self) -> String;

    /// What the last reply took; zero if the backend reports nothing.
    fn used(&self) -> Usage {
        Usage::default()
    }
}
