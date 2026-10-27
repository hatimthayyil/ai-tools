use anyhow::{Context, Result, bail};

use crate::{
    git::hash,
    memory::{Backend, Block, Conversation, NODE, Spend, TRIES, flat, settle},
    notes::{leaf::Front, store::Node, topic::Topic},
};

pub const PROMPT: &str = include_str!("prompts/summary.txt");
/// The longest scope line, in characters.
pub const SCOPE_MAX: usize = 160;
/// The longest gist, in characters.
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
        "Summarise this topic: a scope of at most {SCOPE_MAX} characters, then a gist of at most {GIST_MAX} characters.\n{input}"
    )
}

/// The reply's first line and the rest; a problem to send back if it does not fit.
fn split(reply: &str) -> ((String, String), Option<String>) {
    let reply = reply.trim();
    let (scope, gist) = reply.split_once('\n').unwrap_or((reply, ""));
    let (scope, gist) = (scope.trim().to_string(), gist.trim().to_string());
    let (s, g) = (scope.chars().count(), gist.chars().count());
    let problem = if scope.is_empty() || gist.is_empty() {
        Some(
            "Reply in the format asked: the scope on the first line, a blank line, then the gist."
                .to_string(),
        )
    } else if s > SCOPE_MAX || g > GIST_MAX {
        Some(format!(
            "The scope is {s} characters (limit {SCOPE_MAX}) and the gist {g} characters (limit {GIST_MAX}). \
             Rewrite both whole within their limits: shorten the minor items, never cut them off."
        ))
    } else {
        None
    };
    ((scope, gist), problem)
}

/// The scope and gist the model writes for `input`, held to their form and limits by [`fit`],
/// then, when `backend` has a witness, checked by it until it finds nothing wrong ([`settle`]);
/// `topic` names them in the log.
pub fn summarise(backend: &dyn Backend, input: &str, topic: &Topic) -> Result<(String, String)> {
    let mut conv = backend.open(PROMPT)?;
    let request = ask(input);
    let reply = conv.say(&[Block::new(request.as_str())])?;
    let sg = fit(&mut *conv, &reply)?;
    let Some(witness) = backend.witness() else {
        return Ok(sg);
    };
    settle(
        witness,
        &mut *conv,
        &request,
        sg,
        |(s, g)| format!("{s}\n\n{g}"),
        |conv, reply, _| fit(conv, reply),
        topic,
        &mut Spend::default(),
    )
}

/// The scope and gist in `reply`, retried in `conv` while they do not fit, up to [`TRIES`]; past
/// that, the shortest well-formed try.
fn fit(conv: &mut dyn Conversation, reply: &str) -> Result<(String, String)> {
    let mut reply = reply.to_string();
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
    match tries
        .into_iter()
        .min_by_key(|(s, g)| s.chars().count() + g.chars().count())
    {
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
                .starts_with("The scope is 161 characters")
        );
    }

    #[test]
    fn a_witness_checks_the_scope_and_gist_until_it_finds_nothing() {
        use std::sync::Arc;

        use serde_json::json;
        use tempfile::TempDir;

        use crate::memory::{
            Witnessed,
            fake_api::{completion, serve, text},
        };

        let reply = |t: &str| (200, completion(t, json!({"prompt_tokens": 1})));
        let writer = serve(vec![
            reply("Syntax.\n\nMerge is unary."),
            reply("Syntax.\n\nMerge is binary."),
        ]);
        let witness = serve(vec![
            reply("- contradiction: \"unary\", where the notes say binary"),
            reply("OK"),
        ]);
        let tmp = TempDir::new().unwrap();
        let backend = Witnessed::new(
            Arc::new(writer.api(tmp.path())),
            Arc::new(witness.api(tmp.path())),
        );
        let topic = Topic::parse("syntax").unwrap();
        let sg = summarise(&backend, "- merge.md: Merge is binary.", &topic).unwrap();
        assert_eq!(sg, ("Syntax.".into(), "Merge is binary.".into()));
        let first = witness.sent.recv().unwrap();
        assert_eq!(
            text(&first["messages"][1]["content"]),
            format!(
                "<request>\n{}\n</request>\n<summary>\nSyntax.\n\nMerge is unary.\n</summary>",
                ask("- merge.md: Merge is binary.")
            )
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
