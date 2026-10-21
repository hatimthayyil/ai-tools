use anyhow::{Result, bail};
use tracing::info;

use crate::memory::{Backend, Block, Conversation, Spend, TRIES, Who, fit};

/// The system prompt of every call.
pub const PROMPT: &str = include_str!("prompts/summary.txt");
/// The fewest bytes a summary is asked for, however little it covers.
pub const FLOOR: usize = 512;
/// A file's bytes per byte its summary is asked for.
pub const RATIO: usize = 10;

/// The bytes a summary asked for in `ask` is accepted at: a quarter more.
pub fn accept(ask: usize) -> usize {
    ask + ask / 4
}

/// The bytes the summary of a file of `bytes` is asked for: one in [`RATIO`], at least
/// [`FLOOR`].
pub fn file_ask(bytes: usize) -> usize {
    (bytes / RATIO).max(FLOOR)
}

/// The bytes a directory's last pass asks for, `sizes` being its children's summaries' bytes:
/// their mean, at least [`FLOOR`].
pub fn dir_target(sizes: &[usize]) -> usize {
    (sizes.iter().sum::<usize>() / sizes.len().max(1)).max(FLOOR)
}

/// The bytes the pass after a text of `prev` bytes asks for: half of it, at least `target`.
pub fn next_ask(prev: usize, target: usize) -> usize {
    (prev / 2).max(target)
}

/// A summary, and who wrote it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    pub text: String,
    pub who: Who,
}

/// A child of a directory: its path and its summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Child {
    pub path: String,
    pub summary: String,
}

/// The summary of the file at `path` holding `text`, in one call: asked for [`file_ask`] of its
/// bytes, accepted at [`accept`] of that. What every request took goes into `spent`.
pub fn file(backend: &dyn Backend, path: &str, text: &str, spent: &mut Spend) -> Result<Summary> {
    let ask = file_ask(text.len());
    let message = format!(
        "{}Summarise the file {path} above, {} bytes, in at most {ask} bytes.",
        tagged("file", path, &numbered(text)),
        text.len()
    );
    let mut conv = backend.open(PROMPT)?;
    let text = within(&mut *conv, &message, ask, path, spent)?;
    Ok(Summary {
        text,
        who: who(backend, &*conv),
    })
}

/// The summary of the directory at `path` from its `children`'s summaries, in passes of one
/// conversation, so each reads the ones before from the cache. The first pass takes the
/// children's summaries, each next one only the summary before it; each asks for
/// [`next_ask`] of what it takes, accepted at [`accept`] of that. The passes end once a summary
/// is accepted at [`dir_target`], or a pass asked for it. What every request took goes into
/// `spent`.
pub fn dir(
    backend: &dyn Backend,
    path: &str,
    children: &[Child],
    spent: &mut Spend,
) -> Result<Summary> {
    if children.is_empty() {
        bail!("{path} has no summarised children.");
    }
    let sizes: Vec<usize> = children.iter().map(|c| c.summary.len()).collect();
    let target = dir_target(&sizes);
    let total: usize = sizes.iter().sum();
    let mut conv = backend.open(PROMPT)?;
    let mut ask = next_ask(total, target);
    let input: String = children
        .iter()
        .map(|c| tagged("summary", &c.path, &c.summary))
        .collect();
    let mut message = format!(
        "{input}Summarise the directory {path} from the summaries of its {} children above, \
         {total} bytes in all, in at most {ask} bytes.",
        children.len()
    );
    let mut pass = 1;
    loop {
        let text = within(&mut *conv, &message, ask, path, spent)?;
        info!(dir = path, pass, ask, bytes = text.len(), "pass");
        if ask == target || text.len() <= accept(target) {
            return Ok(Summary {
                who: who(backend, &*conv),
                text,
            });
        }
        let prev = text.len();
        ask = next_ask(prev, target);
        message = format!(
            "{}Condense this summary of the directory {path}, {prev} bytes, to at most {ask} \
             bytes.",
            tagged("summary", path, &text)
        );
        pass += 1;
    }
}

/// `conv`'s reply to `message`, retried while over [`accept`] of `ask`; over it after [`TRIES`],
/// an error.
fn within(
    conv: &mut dyn Conversation,
    message: &str,
    ask: usize,
    path: &str,
    spent: &mut Spend,
) -> Result<String> {
    let max = accept(ask);
    let text = fit(
        conv,
        &[Block::new(message)],
        max,
        |r| r.trim().to_string(),
        |t| retry(t, ask),
        &path,
        spent,
    )?;
    if text.len() > max {
        bail!(
            "{path}: {} bytes after {TRIES} tries; this summary holds at most {max}",
            text.len()
        );
    }
    Ok(text)
}

/// Asks for `ask` bytes scaled down by how far `text` overran it, so the next try lands near it.
fn retry(text: &str, ask: usize) -> String {
    let want = (ask * ask / text.len().max(1)).clamp(1, ask);
    format!(
        "That summary is {} bytes, too long. Rewrite it whole in at most {want} bytes: shorten the minor points, never cut it off.",
        text.len()
    )
}

fn who(backend: &dyn Backend, conv: &dyn Conversation) -> Who {
    Who {
        agent: backend.agent().into(),
        model: backend.model().into(),
        session: conv.session(),
    }
}

/// `text` in `<tag path="path">`, on lines of its own.
fn tagged(tag: &str, path: &str, text: &str) -> String {
    format!("<{tag} path=\"{path}\">\n{}\n</{tag}>\n", text.trim_end())
}

/// Each line of `text` after its number, from 1, and a tab.
fn numbered(text: &str) -> String {
    text.lines()
        .enumerate()
        .map(|(i, l)| format!("{}\t{l}\n", i + 1))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_scale_with_the_input_above_the_floor() {
        assert_eq!(file_ask(0), FLOOR);
        assert_eq!(file_ask(5_119), FLOOR);
        assert_eq!(file_ask(52_310), 5_231);
        assert_eq!(accept(1_000), 1_250);
        assert_eq!(dir_target(&[2_000, 4_000, 3_000]), 3_000);
        assert_eq!(dir_target(&[100, 200]), FLOOR);
        assert_eq!(next_ask(9_000, 3_000), 4_500);
        assert_eq!(next_ask(5_000, 3_000), 3_000);
    }

    #[test]
    fn a_retry_asks_less_the_further_a_try_overran() {
        let r = retry(&"x".repeat(2_000), 1_000);
        assert!(r.starts_with("That summary is 2000 bytes"), "{r}");
        assert!(r.contains("at most 500 bytes"), "{r}");
        assert!(retry(&"x".repeat(1_100), 1_000).contains("at most 909 bytes"));
    }

    #[test]
    fn files_are_read_with_line_numbers() {
        assert_eq!(numbered("a\n\nb\n"), "1\ta\n2\t\n3\tb\n");
        assert_eq!(
            tagged("file", "src/a.rs", "1\ta\n"),
            "<file path=\"src/a.rs\">\n1\ta\n</file>\n"
        );
    }
}
