use anyhow::{Context, Result, bail};

use crate::{
    git::hash,
    memory::{Backend, Block, NODE, TRIES, flat},
    notes::{leaf::Front, store::Node, topic::Topic},
};

pub const PROMPT: &str = include_str!("prompts/summary.txt");
/// The longest scope line, in bytes.
pub const SCOPE_MAX: usize = 160;
/// The longest gist, in bytes.
pub const GIST_MAX: usize = NODE;

/// A topic's `_summary.md`: what is below it (`scope`), its key claims (`gist`), and the hash of
/// the input it was made from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    pub hash: String,
    pub scope: String,
    pub gist: String,
}

impl Summary {
    pub fn parse(text: &str) -> Result<Summary> {
        let (front, body) = Front::parse(text)?;
        let hash = front.get("hash").context("no hash")?.to_string();
        let (scope, gist) = body.split_once('\n').unwrap_or((body, ""));
        Ok(Summary {
            hash,
            scope: scope.trim().into(),
            gist: gist.trim().into(),
        })
    }

    pub fn render(&self) -> String {
        let mut f = Front::default();
        f.set("hash", self.hash.clone());
        format!("{}{}\n\n{}\n", f.render(), self.scope, self.gist)
    }
}

/// What a topic's summary is made from: its subtopics' summaries and its valid notes.
/// `None` when it has neither.
pub fn input(n: &Node) -> Option<String> {
    let mut lines = Vec::new();
    for k in &n.kids {
        if let Some(s) = &k.summary {
            lines.push(format!(
                "- subtopic {}: {} | {}",
                k.topic.name(),
                s.scope,
                flat(&s.gist)
            ));
        }
    }
    for l in n.leaves.iter().filter(|l| l.is_valid()) {
        let c = l.confidence().unwrap_or("unrated");
        lines.push(format!("- note [{c}] {}: {}", l.title, flat(&l.body)));
    }
    if lines.is_empty() {
        return None;
    }
    Some(format!("Topic: {}\n{}", n.topic.name(), lines.join("\n")))
}

/// The summary of `n`'s one subtopic, when that is all `n` holds: `n` takes it as its own.
pub fn only_child(n: &Node) -> Option<&Summary> {
    if n.leaves.iter().any(|l| l.is_valid()) {
        return None;
    }
    let mut sums = n.kids.iter().filter_map(|k| k.summary.as_ref());
    match (sums.next(), sums.next()) {
        (Some(s), None) => Some(s),
        _ => None,
    }
}

/// One summary to make (or, with no input, to remove); `copy` when it needs no model call.
#[derive(Clone, Debug)]
pub struct Job {
    pub topic: Topic,
    pub input: Option<String>,
    pub copy: Option<(String, String)>,
}

/// Whether `n`'s summary does not match its input.
pub fn is_dirty(n: &Node) -> Result<bool> {
    let want = input(n).map(|i| hash(&i)).transpose()?;
    Ok(want.as_deref() != n.summary.as_ref().map(|s| s.hash.as_str()))
}

/// Every dirty topic with no dirty topic below it: the ones that can be summarised now.
pub fn due(root: &Node) -> Result<Vec<Job>> {
    fn go(n: &Node, out: &mut Vec<Job>) -> Result<bool> {
        let mut below = false;
        for k in &n.kids {
            below |= go(k, out)?;
        }
        if n.topic.is_root() {
            return Ok(below);
        }
        let dirty = is_dirty(n)?;
        if dirty && !below {
            out.push(Job {
                topic: n.topic.clone(),
                input: input(n),
                copy: only_child(n).map(|s| (s.scope.clone(), s.gist.clone())),
            });
        }
        Ok(below || dirty)
    }
    let mut out = Vec::new();
    go(root, &mut out)?;
    Ok(out)
}

fn ask(input: &str) -> String {
    format!(
        "Summarise this topic: a scope of at most {SCOPE_MAX} bytes, then a gist of at most {GIST_MAX} bytes.\n{input}"
    )
}

/// The reply's first line and the rest; a problem to send back if it does not fit.
fn split(reply: &str) -> ((String, String), Option<String>) {
    let reply = reply.trim();
    let (scope, gist) = reply.split_once('\n').unwrap_or((reply, ""));
    let (scope, gist) = (scope.trim().to_string(), gist.trim().to_string());
    let problem = if scope.is_empty() || gist.is_empty() {
        Some(
            "Reply in the format asked: the scope on the first line, a blank line, then the gist."
                .to_string(),
        )
    } else if scope.len() > SCOPE_MAX || gist.len() > GIST_MAX {
        Some(format!(
            "The scope is {} bytes (limit {SCOPE_MAX}) and the gist {} bytes (limit {GIST_MAX}). \
             Rewrite both whole within their limits: shorten the minor items, never cut them off.",
            scope.len(),
            gist.len()
        ))
    } else {
        None
    };
    ((scope, gist), problem)
}

/// The scope and gist the model writes for `input`, retried in the same conversation while they
/// do not fit, up to [`TRIES`]; past that, the shortest well-formed try.
pub fn summarise(backend: &dyn Backend, input: &str) -> Result<(String, String)> {
    let mut conv = backend.open(PROMPT)?;
    let mut reply = conv.say(&[Block::new(ask(input))])?;
    let mut tries = Vec::new();
    for k in 1..=TRIES {
        let (sg, problem) = split(&reply);
        let Some(problem) = problem else {
            return Ok(sg);
        };
        if !sg.0.is_empty() && !sg.1.is_empty() {
            tries.push(sg);
        }
        if k < TRIES {
            reply = conv.say(&[Block::new(problem)])?;
        }
    }
    match tries.into_iter().min_by_key(|(s, g)| s.len() + g.len()) {
        Some(sg) => Ok(sg),
        None => bail!("no summary in the format asked after {TRIES} tries"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_round_trip() {
        let s = Summary {
            hash: "ab12".into(),
            scope: "Syntax: how words combine.".into(),
            gist: "Merge is binary.\nMove is internal merge.".into(),
        };
        let text = s.render();
        assert_eq!(
            text,
            "---\nhash: ab12\n---\nSyntax: how words combine.\n\nMerge is binary.\nMove is internal merge.\n"
        );
        assert_eq!(Summary::parse(&text).unwrap(), s);
        assert!(Summary::parse("scope\n\ngist").is_err());
    }

    #[test]
    fn replies_split_into_scope_and_gist() {
        let ((s, g), p) = split(" scope line \n\n gist a\ngist b \n");
        assert_eq!(
            (s.as_str(), g.as_str(), p),
            ("scope line", "gist a\ngist b", None)
        );
        assert!(
            split("only a scope")
                .1
                .unwrap()
                .starts_with("Reply in the format")
        );
        let long = format!("{}\n\ngist", "s".repeat(SCOPE_MAX + 1));
        assert!(
            split(&long)
                .1
                .unwrap()
                .starts_with("The scope is 161 bytes")
        );
    }

    #[test]
    fn prompt_is_the_spec_s() {
        assert!(PROMPT.starts_with("You write the summaries of ai notes"));
        assert!(PROMPT.contains("Write about the subject, never about the notes."));
        assert!(PROMPT.contains("A verified\nclaim is untagged."));
        assert!(hash("x").unwrap().len() == 40);
    }
}
