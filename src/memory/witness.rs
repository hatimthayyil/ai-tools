use std::{fmt, sync::Arc};

use anyhow::{Result, bail};
use tracing::info;

use crate::memory::{
    backend::{Backend, Block, Conversation},
    compact::{Spend, TRIES, ask},
    log,
};

pub const WITNESS: &str = include_str!("prompts/witness.txt");

/// What the witness is told when its reply is neither `OK` nor findings.
const FORMAT: &str =
    "Reply in the format asked: OK alone, or one finding per line, each starting with \"- \".";

/// A summariser with the witness that checks every summary it writes: `[summariser.witness]`.
/// It writes as the summariser alone; [`settle`] brings in the witness.
pub struct Witnessed {
    writer: Arc<dyn Backend>,
    witness: Arc<dyn Backend>,
}

impl Witnessed {
    pub fn new(writer: Arc<dyn Backend>, witness: Arc<dyn Backend>) -> Witnessed {
        Witnessed { writer, witness }
    }
}

impl Backend for Witnessed {
    fn agent(&self) -> &str {
        self.writer.agent()
    }

    fn model(&self) -> &str {
        self.writer.model()
    }

    fn open(&self, system: &str) -> Result<Box<dyn Conversation>> {
        self.writer.open(system)
    }

    fn jobs(&self) -> usize {
        self.writer.jobs()
    }

    fn witness(&self) -> Option<&dyn Backend> {
        Some(&*self.witness)
    }

    fn stop(&self) {
        self.writer.stop();
        self.witness.stop();
    }
}

/// What the witness made of a summary.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Clean,
    /// One finding per line, each starting with `- `.
    Findings(Vec<String>),
}

/// The witness's `reply` read as `OK` or findings; `None` when it is neither.
fn verdict(reply: &str) -> Option<Verdict> {
    let reply = reply.trim();
    if reply.trim_end_matches('.').eq_ignore_ascii_case("ok") {
        return Some(Verdict::Clean);
    }
    let findings: Vec<String> = reply
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("- "))
        .map(String::from)
        .collect();
    (!findings.is_empty()).then_some(Verdict::Findings(findings))
}

/// The summariser's `reply` to findings: its summary, then its reasons for the findings it
/// contests, from the first line starting `Contested:`.
fn answer(reply: &str) -> (&str, Option<&str>) {
    let mut at = 0;
    for line in reply.split_inclusive('\n') {
        let head = line.trim_start().get(..10);
        if head.is_some_and(|h| h.eq_ignore_ascii_case("contested:")) {
            return (&reply[..at], Some(reply[at..].trim()));
        }
        at += line.len();
    }
    (reply, None)
}

/// The summariser's message for `findings`.
fn found(findings: &[String]) -> String {
    format!(
        "A witness checked your summary against the sources and found:\n{}\n\n\
         Answer every finding. Rewrite the summary whole, in the same form and within the same \
         limit, to fix each finding that is right. For a finding that is wrong, keep your words, \
         and after the summary write a line starting \"Contested:\" that gives your reason.",
        findings.join("\n")
    )
}

/// The witness's verdict on `message`, asked again in the same conversation while it replies in
/// no format asked, up to [`TRIES`].
fn check(checks: &mut dyn Conversation, message: &str, spent: &mut Spend) -> Result<Verdict> {
    let mut reply = ask(checks, &[Block::new(message)], false, spent)?;
    for k in 1..=TRIES {
        if let Some(v) = verdict(&reply) {
            return Ok(v);
        }
        if k < TRIES {
            reply = ask(checks, &[Block::new(FORMAT)], false, spent)?;
        }
    }
    bail!("the witness replied in no format asked after {TRIES} tries")
}

/// Has `witness` check `summary`, which the summariser wrote in `conv` for `request`, and goes
/// back and forth until the witness finds nothing wrong. Each round the summariser answers the
/// findings in `conv`: it rewrites the summary, which `fit` holds to its form and limit as it held
/// the first, or contests findings with its reasons. The witness then checks the summary again,
/// in its own conversation with everything said so far. `show` renders a summary for the
/// witness. There is no limit on rounds: they end when the witness finds nothing wrong, or with
/// the first call that fails. What every request took goes into `spent`; `node` names the
/// summary in the log.
#[allow(clippy::too_many_arguments)]
pub fn settle<T>(
    witness: &dyn Backend,
    conv: &mut dyn Conversation,
    request: &str,
    mut summary: T,
    show: impl Fn(&T) -> String,
    mut fit: impl FnMut(&mut dyn Conversation, &str, &mut Spend) -> Result<T>,
    node: &dyn fmt::Display,
    spent: &mut Spend,
) -> Result<T> {
    let mut checks = witness.open(WITNESS)?;
    let mut message = format!(
        "<request>\n{request}\n</request>\n<summary>\n{}\n</summary>",
        show(&summary)
    );
    let mut round = 0u64;
    loop {
        let findings = match check(&mut *checks, &message, spent)? {
            Verdict::Clean => {
                info!(node = %node, rounds = round, witness = %witness.model(), "witnessed");
                return Ok(summary);
            }
            Verdict::Findings(f) => f,
        };
        round += 1;
        info!(
            node = %node,
            round,
            findings = findings.len(),
            cost = log::usd(checks.used().cost),
            "witness found faults: {}",
            findings.join(" ")
        );
        let reply = ask(conv, &[Block::new(found(&findings))], true, spent)?;
        let (text, reasons) = answer(&reply);
        if !text.trim().is_empty() || reasons.is_none() {
            summary = fit(conv, text, spent)?;
        }
        let now = show(&summary);
        info!(
            node = %node,
            round,
            chars = now.chars().count(),
            contested = reasons.is_some(),
            "summariser answered"
        );
        message = format!("The summariser answered.\n<summary>\n{now}\n</summary>");
        if let Some(r) = reasons {
            message = format!("{message}\n{r}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_verdict_is_ok_or_findings_else_nothing() {
        assert_eq!(verdict(" OK. \n"), Some(Verdict::Clean));
        assert_eq!(verdict("ok"), Some(Verdict::Clean));
        assert_eq!(
            verdict(
                "Two faults:\n- unsupported: \"x\" is not in the sources\n  - contradiction: \"y\"\n"
            ),
            Some(Verdict::Findings(vec![
                "- unsupported: \"x\" is not in the sources".into(),
                "- contradiction: \"y\"".into(),
            ]))
        );
        assert_eq!(
            verdict("OK\n- meaning: \"z\""),
            Some(Verdict::Findings(vec!["- meaning: \"z\"".into()]))
        );
        assert_eq!(verdict("Nothing wrong here."), None);
        assert_eq!(verdict(""), None);
    }

    #[test]
    fn an_answer_splits_at_its_first_contested_line() {
        assert_eq!(answer("new line\n"), ("new line\n", None));
        assert_eq!(
            answer("the line\ncontested: the sources say \"never\"\nContested: more"),
            (
                "the line\n",
                Some("contested: the sources say \"never\"\nContested: more")
            )
        );
        assert_eq!(
            answer("  Contested: all of it"),
            ("", Some("Contested: all of it"))
        );
    }

    use std::path::Path;

    use genai::chat::ReasoningEffort;
    use serde_json::{Value, json};
    use tempfile::TempDir;

    use crate::memory::{
        api::tests::{Server, completion, serve, text},
        compact::{retry_text, ruler, write_line},
        tree::limit,
    };

    fn reply(text: &str) -> (u16, Value) {
        let usage = json!({"prompt_tokens": 10, "completion_tokens": 2});
        (200, completion(text, usage))
    }

    /// A summariser and a witness thinking hard, each behind its own fake DeepSeek.
    fn pair(writer: &Server, witness: &Server, calls: &Path) -> Witnessed {
        Witnessed::new(
            Arc::new(writer.api(calls)),
            Arc::new(witness.api_at(Some(ReasoningEffort::High), calls)),
        )
    }

    /// Each message of the request a server received next: role, then text.
    fn said(server: &Server) -> Vec<(String, String)> {
        let body = server.sent.recv().unwrap();
        body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| {
                (
                    m["role"].as_str().unwrap().into(),
                    text(&m["content"]).into(),
                )
            })
            .collect()
    }

    fn last(server: &Server) -> String {
        said(server).pop().unwrap().1
    }

    /// The request of a merge of `a` and `b` into a line of `words` words, after its ruler.
    fn merge(words: usize) -> String {
        format!("Merge these two lines into one, in at most {words} words:\na\nb")
    }

    fn write(backend: &Witnessed, words: usize, spent: &mut Spend) -> Result<String> {
        let message = [Block::new(format!("{}{}", ruler(words), merge(words)))];
        write_line(backend, "Write a line.", &message, words, &"0+2", spent).map(|(_, l)| l)
    }

    #[test]
    fn the_witness_and_the_summariser_go_back_and_forth_until_the_witness_finds_nothing() {
        let writer = serve(vec![
            reply("first line"),
            reply("second line"),
            reply("third line"),
        ]);
        let witness = serve(vec![
            reply("- unsupported: \"first\" is not in the sources"),
            reply("Still:\n- contradiction: \"second\" says the opposite"),
            reply("OK"),
        ]);
        let tmp = TempDir::new().unwrap();
        let backend = pair(&writer, &witness, tmp.path());
        let mut spent = Spend::default();
        assert_eq!(write(&backend, 50, &mut spent).unwrap(), "third line");
        assert_eq!((spent.requests, spent.retries), (6, 2));

        let request = format!("<request>\n{}\n</request>", merge(50));
        let first = said(&witness);
        assert_eq!(first[0], ("system".into(), WITNESS.into()));
        assert_eq!(
            first[1].1,
            format!("{request}\n<summary>\nfirst line\n</summary>")
        );
        assert_eq!(
            last(&witness),
            "The summariser answered.\n<summary>\nsecond line\n</summary>"
        );
        let third = said(&witness);
        assert_eq!(
            third.len(),
            6,
            "the witness sees the whole exchange: {third:?}"
        );
        assert_eq!(
            third[5].1,
            "The summariser answered.\n<summary>\nthird line\n</summary>"
        );

        assert_eq!(last(&writer), format!("{}{}", ruler(50), merge(50)));
        let answered = said(&writer);
        assert!(
            answered[3].1.starts_with(
                "A witness checked your summary against the sources and found:\n\
                 - unsupported: \"first\" is not in the sources\n\n"
            ),
            "{answered:?}"
        );
        assert!(
            last(&writer).contains("- contradiction: \"second\" says the opposite"),
            "only the findings go to the summariser"
        );
    }

    #[test]
    fn a_contested_finding_goes_to_the_witness_with_its_reason() {
        let writer = serve(vec![
            reply("the line"),
            reply("the line\nContested: the sources say \"never\" in a"),
        ]);
        let witness = serve(vec![
            reply("- negation: \"the line\" drops \"never\""),
            reply("OK."),
        ]);
        let tmp = TempDir::new().unwrap();
        let mut spent = Spend::default();
        let line = write(&pair(&writer, &witness, tmp.path()), 50, &mut spent).unwrap();
        assert_eq!(line, "the line");
        said(&witness);
        assert_eq!(
            last(&witness),
            "The summariser answered.\n<summary>\nthe line\n</summary>\n\
             Contested: the sources say \"never\" in a"
        );
    }

    #[test]
    fn a_rewrite_is_held_to_the_limit_before_the_witness_sees_it() {
        let words = 5;
        let long = "a rewrite far longer than any line of five words may be";
        assert!(long.chars().count() > limit(words));
        let writer = serve(vec![reply("short"), reply(long), reply("fixed")]);
        let witness = serve(vec![reply("- unsupported: \"short\""), reply("OK")]);
        let tmp = TempDir::new().unwrap();
        let mut spent = Spend::default();
        let line = write(&pair(&writer, &witness, tmp.path()), words, &mut spent).unwrap();
        assert_eq!(line, "fixed");
        said(&writer);
        said(&writer);
        assert_eq!(last(&writer), retry_text(long, words));
        said(&witness);
        assert_eq!(
            last(&witness),
            "The summariser answered.\n<summary>\nfixed\n</summary>"
        );
    }

    #[test]
    fn a_failing_witness_fails_the_line() {
        let writer = serve(vec![reply("the line")]);
        let witness = serve(vec![(
            429,
            json!({"error": {"message": "Rate limit reached", "type": "rate_limit"}}),
        )]);
        let tmp = TempDir::new().unwrap();
        let err = write(
            &pair(&writer, &witness, tmp.path()),
            50,
            &mut Spend::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.starts_with("deepseek: "), "{err}");
        assert!(err.contains("Rate limit reached"), "{err}");

        let writer = serve(vec![reply("the line")]);
        let witness = serve(vec![reply("Looks fine to me.")]);
        let mut spent = Spend::default();
        let err = write(&pair(&writer, &witness, tmp.path()), 50, &mut spent)
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            format!("the witness replied in no format asked after {TRIES} tries")
        );
        assert_eq!(spent.requests, 1 + TRIES as u64);
        said(&witness);
        assert_eq!(last(&witness), FORMAT);
    }
}
