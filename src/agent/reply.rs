//! What `ai reply` quotes: the assistant's replies to the user, in answers.
//!
//! An answer runs from one of the user's messages to their next, to a compaction between turns,
//! or to where another session takes the thread up, as after `/fold`; a compaction inside a
//! turn, before its final reply, leaves the turn in its answer. Messages from other agents open
//! turns inside it, so an answer can span several turns; text the assistant writes between tool
//! calls is left out.

use std::{collections::HashSet, str::FromStr};

use anyhow::{Result, anyhow, bail};

use crate::{
    agent::{
        id::Id,
        model::{Item, Kind, Marker, Turn, turns},
    },
    memory::plural,
};

/// Which answer a reference names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    /// Counted back from the latest: 1 for the latest.
    Back(usize),
    /// The answer to the user's message at this turn of the path.
    Turn(usize),
}

/// Whose items a path holds, as errors name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Whose {
    /// The session's own transcript.
    Session,
    /// The session's branch of the thread it is attached to.
    Branch,
}

impl Whose {
    fn noun(self) -> &'static str {
        match self {
            Whose::Session => "session",
            Whose::Branch => "branch",
        }
    }
}

/// A reference: which answer, and which reply in it, else all of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ref {
    pub which: Which,
    /// 1 for the answer's first reply; none for every reply.
    pub pick: Option<usize>,
}

impl FromStr for Ref {
    type Err = anyhow::Error;

    /// `-N`, `N`, `#k`, `-N#k` or `N#k`; empty for the latest answer.
    fn from_str(s: &str) -> Result<Ref> {
        let (answer, pick) = match s.split_once('#') {
            Some((a, k)) => (a, Some(k)),
            None => (s, None),
        };
        let malformed = || {
            anyhow!(
                "Not a reply reference: {s}. Use -N, N, #k, -N#k or N#k; ai reply --help shows each."
            )
        };
        let which = match answer.strip_prefix('-') {
            None if answer.is_empty() => Which::Back(1),
            None => match count(answer) {
                Some(n) => Which::Turn(n),
                None if pick.is_none() && Id::parse(s).is_some() => bail!(
                    "{s} names items or turns as ai agent zoom does; ai reply takes a turn's number, N, or N#k for one reply in its answer."
                ),
                None => return Err(malformed()),
            },
            Some(n) => match count(n).ok_or_else(malformed)? {
                0 => bail!("-0 counts no answer back: -1 is the latest."),
                n => Which::Back(n),
            },
        };
        let pick = match pick {
            None => None,
            Some(k) => match count(k).ok_or_else(malformed)? {
                0 => bail!("{s} picks no reply: #1 is the first in an answer."),
                k => Some(k),
            },
        };
        Ok(Ref { which, pick })
    }
}

/// What the error of a reference that fails ends with: how to quote less than a reply.
pub const PART: &str = "To quote part of an answer, select it and run /reply.";

/// The references `args` hold: split at commas and white space, each without the quotes around
/// it, which `/reply` passes on as typed. None when they hold only separators and quotes.
pub fn split(args: &[String]) -> Vec<&str> {
    args.iter()
        .flat_map(|a| a.split(|c: char| c == ',' || c.is_whitespace()))
        .map(|r| r.trim_matches(['\'', '"']))
        .filter(|r| !r.is_empty())
        .collect()
}

/// `s` as a number, when it is only ASCII digits.
fn count(s: &str) -> Option<usize> {
    s.bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| s.parse().ok())
        .flatten()
}

/// One answer on a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    /// The position of the item that opened it: the user's message, or a compaction; none at
    /// the path's start, and where another session takes the thread up with no message of the
    /// user's.
    pub opens: Option<usize>,
    /// The positions of its replies, oldest first; never empty.
    pub replies: Vec<usize>,
}

/// The answers along `path`, oldest first. An answer without a reply is left out.
///
/// Each session's stretch of the path is read by itself, so a reply stays its turn's final one
/// when the session that takes the thread up writes on with no message before it, as it does
/// to a background command that finishes after `/fold`.
pub fn answers(path: &[&Item]) -> Vec<Answer> {
    let session = |i: usize| (&path[i].id.harness, &path[i].id.session);
    let ends = (1..=path.len()).filter(|&i| i == path.len() || session(i) != session(i - 1));
    let mut out = Vec::new();
    let mut from = 0;
    for to in ends {
        out.extend(within(&path[from..to]).into_iter().map(|a| Answer {
            opens: a.opens.map(|i| from + i),
            replies: a.replies.iter().map(|i| from + i).collect(),
        }));
        from = to;
    }
    out
}

/// The answers along `path`, one session's stretch, by their positions in it.
fn within(path: &[&Item]) -> Vec<Answer> {
    let xs = turns(path);
    let replies: HashSet<usize> = xs.iter().filter_map(|t| t.reply).collect();
    // A turn's work before its final reply, where a compaction leaves the turn going on.
    let inside: HashSet<usize> = xs
        .iter()
        .filter_map(|t| Some(t.span.start + 1..t.reply?))
        .flatten()
        .collect();
    let mut out = vec![Answer {
        opens: None,
        replies: Vec::new(),
    }];
    for (i, item) in path.iter().enumerate() {
        let separates = match item.kind {
            Kind::User(_) => true,
            Kind::Marker(Marker::Compaction | Marker::Summary) => !inside.contains(&i),
            _ => false,
        };
        if separates {
            out.push(Answer {
                opens: Some(i),
                replies: Vec::new(),
            });
        } else if replies.contains(&i) {
            out.last_mut().expect("starts with one").replies.push(i);
        }
    }
    out.retain(|a| !a.replies.is_empty());
    out
}

/// The replies `r` picks along `path`, oldest first: every line quoted with `> `, a blank line
/// between replies, ending in a newline. `s` is the reference as written, and `whose` the path's,
/// for errors.
pub fn quote(path: &[&Item], s: &str, r: Ref, whose: Whose) -> Result<String> {
    let all = answers(path);
    let (answer, name) = match r.which {
        Which::Back(n) => back(&all, n, whose)?,
        Which::Turn(t) => opened(path, &all, t, whose)?,
    };
    let picked = match r.pick {
        None => &answer.replies[..],
        Some(k) => answer.replies.get(k - 1..k).ok_or_else(|| {
            anyhow!(
                "{name} has {}, so {s} picks none.",
                plural(answer.replies.len() as u64, "reply")
            )
        })?,
    };
    let quoted: Vec<String> = picked
        .iter()
        .map(|&i| match &path[i].kind {
            Kind::Text(t) => quoted(t),
            _ => unreachable!("a final reply is the assistant's words"),
        })
        .collect();
    Ok(quoted.join("\n\n") + "\n")
}

/// The answer `n` back from the latest of `all`, with what errors call it.
fn back(all: &[Answer], n: usize, whose: Whose) -> Result<(&Answer, String)> {
    let this = whose.noun();
    if all.is_empty() {
        bail!("This {this} has no replies yet, so -{n} has nothing to quote.");
    }
    let Some(at) = all.len().checked_sub(n) else {
        bail!(
            "-{n} is past the oldest answer: this {this} has {}.",
            plural(all.len() as u64, "answer")
        );
    };
    let name = match n {
        1 => "The latest answer".to_string(),
        n => format!("Answer -{n}"),
    };
    Ok((&all[at], name))
}

/// The answer the user's message at turn `t` of `path` opens, with what errors call it.
fn opened<'a>(
    path: &[&Item],
    all: &'a [Answer],
    t: usize,
    whose: Whose,
) -> Result<(&'a Answer, String)> {
    let xs = turns(path);
    let Some(x) = xs.get(t) else {
        let this = whose.noun();
        match xs.len() {
            0 => bail!("This {this} has no turns yet, so {t} has nothing to quote."),
            n => bail!(
                "This {this} has no turn {t}: its turns run from 0 to {}.",
                n - 1
            ),
        }
    };
    match &path[x.span.start].kind {
        Kind::User(_) => match all.iter().find(|a| a.opens == Some(x.span.start)) {
            Some(a) => Ok((a, format!("The answer to your message {t}"))),
            None => bail!("Your message {t} has no reply before your next message or a fold."),
        },
        Kind::Work { .. } => bail!(not_yours(path, &xs, all, t)),
        _ => bail!("Turn {t} opens with no message of yours."),
    }
}

/// The error for turn `t` of `xs`, another agent's report: where its reply is, if anywhere.
fn not_yours(path: &[&Item], xs: &[Turn], all: &[Answer], t: usize) -> String {
    let x = &xs[t];
    let Kind::Work { from, .. } = &path[x.span.start].kind else {
        unreachable!("the turn opens with a report")
    };
    let head = format!("Turn {t} is the report \"{from}\", not your message");
    let Some(reply) = x.reply else {
        return format!("{head}, and it has no reply.");
    };
    let a = all
        .iter()
        .find(|a| a.replies.contains(&reply))
        .expect("every final reply is in an answer");
    let k = 1 + a.replies.iter().position(|&r| r == reply).expect("found");
    let message = a
        .opens
        .filter(|&p| matches!(path[p].kind, Kind::User(_)))
        .and_then(|p| xs.iter().position(|x| x.span.start == p));
    match message {
        Some(m) => format!("{head}: its reply is {m}#{k}."),
        None => {
            format!("{head}, and its reply follows a fold, in no answer to a message of yours.")
        }
    }
}

/// `text` with `> ` before each line, `>` alone on an empty one, without the empty lines it
/// starts or ends with.
fn quoted(text: &str) -> String {
    text.trim_end()
        .trim_start_matches(['\n', '\r'])
        .lines()
        .map(|l| match l.trim_end() {
            "" => ">".to_string(),
            l => format!("> {l}"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::agent::{claude, model::Tree};

    fn fixture(name: &str) -> Vec<Item> {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/claude")
            .join(name);
        claude::read(&p, "s").unwrap()
    }

    /// The newest branch of the session transcript `name`, quoted as `r` picks.
    fn reply(name: &str, r: &str) -> Result<String> {
        let tree = Tree::new(fixture(name));
        let path = tree.path(*tree.tips().last().unwrap());
        quote(&path, r, r.parse()?, Whose::Session)
    }

    fn err(name: &str, r: &str) -> String {
        reply(name, r).unwrap_err().to_string()
    }

    #[test]
    fn references_parse() {
        let r = |which, pick| Ref { which, pick };
        let (b, t) = (Which::Back, Which::Turn);
        assert_eq!("".parse::<Ref>().unwrap(), r(b(1), None));
        assert_eq!("-1".parse::<Ref>().unwrap(), r(b(1), None));
        assert_eq!("-3".parse::<Ref>().unwrap(), r(b(3), None));
        assert_eq!("#2".parse::<Ref>().unwrap(), r(b(1), Some(2)));
        assert_eq!("-2#1".parse::<Ref>().unwrap(), r(b(2), Some(1)));
        assert_eq!("-12#10".parse::<Ref>().unwrap(), r(b(12), Some(10)));
        assert_eq!("597".parse::<Ref>().unwrap(), r(t(597), None));
        assert_eq!("0".parse::<Ref>().unwrap(), r(t(0), None));
        assert_eq!("597#2".parse::<Ref>().unwrap(), r(t(597), Some(2)));
    }

    #[test]
    fn commas_and_white_space_separate_references_and_quotes_around_them_drop() {
        let s = |args: &[&str]| {
            let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            split(&args)
                .iter()
                .map(|r| r.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(s(&[]), Vec::<String>::new());
        assert_eq!(s(&["595", "-1"]), ["595", "-1"]);
        assert_eq!(s(&["1,2"]), ["1", "2"]);
        assert_eq!(s(&["1, 2"]), ["1", "2"]);
        assert_eq!(s(&["595  -1\t597#2 "]), ["595", "-1", "597#2"]);
        assert_eq!(s(&["\"1,2\""]), ["1", "2"]);
        assert_eq!(s(&["\"1, 2\""]), ["1", "2"]);
        assert_eq!(s(&["'#2'"]), ["#2"]);
        assert_eq!(s(&["'1',", "'2'"]), ["1", "2"]);
        assert_eq!(s(&["1,,2,"]), ["1", "2"]);
        assert_eq!(s(&["", " , ", "''", "\"\""]), Vec::<String>::new());
        assert_eq!(s(&["1\"2"]), ["1\"2"]);
    }

    #[test]
    fn malformed_references_say_what_to_use() {
        let e = |s: &str| s.parse::<Ref>().unwrap_err().to_string();
        for s in [
            "-", "#", "-1#", "--1", "-1.2", "+1", "x", "-x", "#x", "-1 #2", "#2#3", "- 1", "597#",
            "597.2#1", "8+4#1",
        ] {
            assert_eq!(
                e(s),
                format!(
                    "Not a reply reference: {s}. Use -N, N, #k, -N#k or N#k; ai reply --help shows \
                     each."
                ),
                "{s}"
            );
        }
        for s in ["597.5", "8+4", "12.3+2"] {
            assert_eq!(
                e(s),
                format!(
                    "{s} names items or turns as ai agent zoom does; ai reply takes a turn's \
                     number, N, or N#k for one reply in its answer."
                )
            );
        }
        assert_eq!(e("-0"), "-0 counts no answer back: -1 is the latest.");
        assert_eq!(e("#0"), "#0 picks no reply: #1 is the first in an answer.");
        assert_eq!(
            e("5#0"),
            "5#0 picks no reply: #1 is the first in an answer."
        );
    }

    #[test]
    fn the_latest_answer_holds_every_final_reply_since_the_user_wrote() {
        assert_eq!(
            reply("reply.jsonl", "").unwrap(),
            "> The docs fork is done.\n>\n> It added a README section.\n\n\
             > The tests fork is done too.\n"
        );
        assert_eq!(
            reply("reply.jsonl", "-1").unwrap(),
            reply("reply.jsonl", "").unwrap()
        );
    }

    #[test]
    fn a_reply_is_picked_from_the_first() {
        assert_eq!(
            reply("reply.jsonl", "#2").unwrap(),
            "> The tests fork is done too.\n"
        );
        assert_eq!(
            reply("reply.jsonl", "-2#1").unwrap(),
            "> Two forks are on it.\n"
        );
    }

    #[test]
    fn local_commands_and_fork_reports_do_not_end_an_answer() {
        let latest = reply("reply.jsonl", "").unwrap();
        assert!(
            latest.contains("README section.\n\n> The tests fork"),
            "the /copy and /effort between two forks' replies do not split the answer: {latest}"
        );
        assert_eq!(
            reply("reply.jsonl", "-2").unwrap(),
            "> Two forks are on it.\n",
            "a prompt command is the user's message"
        );
    }

    #[test]
    fn a_compaction_between_turns_ends_an_answer_and_one_inside_a_turn_does_not() {
        assert_eq!(
            reply("reply.jsonl", "-4").unwrap(),
            "> Before the fold.\n",
            "the fold after turn 4's reply ends its answer"
        );
        assert_eq!(
            reply("reply.jsonl", "-3").unwrap(),
            "> Continuing after the fold.\n"
        );
        assert_eq!(
            reply("reply.jsonl", "5").unwrap(),
            reply("reply.jsonl", "-3").unwrap(),
            "turn 5 is compacted before its final reply, which stays in the answer to it"
        );
        assert_eq!(
            reply("compact.jsonl", "-2").unwrap(),
            "> Three are open: #12, #15 and #19.\n",
            "across a native compaction"
        );
    }

    #[test]
    fn answers_without_a_reply_are_skipped() {
        assert_eq!(
            reply("reply.jsonl", "-5").unwrap(),
            "> The first answer.\n\n> Its second reply, after an interrupted turn.\n",
            "the user's interrupted message between this answer and the fold has no reply"
        );
    }

    #[test]
    fn references_past_what_exists_say_what_does() {
        assert_eq!(
            err("reply.jsonl", "-6"),
            "-6 is past the oldest answer: this session has 5 answers."
        );
        assert_eq!(
            err("reply.jsonl", "#3"),
            "The latest answer has 2 replies, so #3 picks none."
        );
        assert_eq!(
            err("reply.jsonl", "-4#2"),
            "Answer -4 has 1 reply, so -4#2 picks none."
        );
    }

    #[test]
    fn a_session_without_replies_says_so() {
        let none = "This session has no replies yet, so -1 has nothing to quote.";
        let latest: Ref = "".parse().unwrap();
        let q = |path: &[&Item]| {
            quote(path, "", latest, Whose::Session)
                .unwrap_err()
                .to_string()
        };
        assert_eq!(q(&[]), none);
        let tree = Tree::new(fixture("reply.jsonl"));
        let path = tree.path(*tree.tips().last().unwrap());
        assert_eq!(q(&path[..2]), none, "narration is no reply");
    }

    /// The fixture's turns, by the message that opens each: 0 "Start.", 1 report Check, 2 report
    /// Check again, 3 "Stop that.", 4 "Fold soon.", 5 "Carry on.", 6 /forks, 7 "Tell me when they
    /// finish.", 8 report Docs, 9 report Tests.
    #[test]
    fn a_turn_names_the_answer_to_the_message_that_opens_it() {
        assert_eq!(
            reply("reply.jsonl", "0").unwrap(),
            reply("reply.jsonl", "-5").unwrap(),
            "the reports that follow the message stay in its answer"
        );
        assert_eq!(
            reply("reply.jsonl", "7").unwrap(),
            reply("reply.jsonl", "-1").unwrap()
        );
        assert_eq!(
            reply("reply.jsonl", "6").unwrap(),
            "> Two forks are on it.\n"
        );
        assert_eq!(
            reply("reply.jsonl", "7#2").unwrap(),
            "> The tests fork is done too.\n"
        );
        assert_eq!(reply("reply.jsonl", "4").unwrap(), "> Before the fold.\n");
    }

    #[test]
    fn a_turn_that_names_no_answer_says_what_to_use() {
        assert_eq!(
            err("reply.jsonl", "9"),
            "Turn 9 is the report \"Tests\", not your message: its reply is 7#2."
        );
        assert_eq!(
            err("reply.jsonl", "8#1"),
            "Turn 8 is the report \"Docs\", not your message: its reply is 7#1."
        );
        assert_eq!(
            err("reply.jsonl", "2"),
            "Turn 2 is the report \"Check again\", not your message: its reply is 0#2."
        );
        assert_eq!(
            err("reply.jsonl", "1"),
            "Turn 1 is the report \"Check\", not your message, and it has no reply."
        );
        assert_eq!(
            err("reply.jsonl", "3"),
            "Your message 3 has no reply before your next message or a fold."
        );
        assert_eq!(
            err("reply.jsonl", "10"),
            "This session has no turn 10: its turns run from 0 to 9."
        );
        assert_eq!(
            err("reply.jsonl", "7#3"),
            "The answer to your message 7 has 2 replies, so 7#3 picks none."
        );
        let empty = |whose| {
            quote(&[], "4", "4".parse().unwrap(), whose)
                .unwrap_err()
                .to_string()
        };
        assert_eq!(
            empty(Whose::Session),
            "This session has no turns yet, so 4 has nothing to quote."
        );
        assert_eq!(
            empty(Whose::Branch),
            "This branch has no turns yet, so 4 has nothing to quote.",
            "an attached session's branch of its thread"
        );
    }

    #[test]
    fn a_report_answered_after_a_fold_names_no_message() {
        let item = |e: usize, kind: Kind| Item {
            id: id(e),
            parent: e.checked_sub(1).map(id),
            ts: jiff::Timestamp::UNIX_EPOCH,
            ends_turn: matches!(kind, Kind::Text(_)),
            kind,
        };
        let items = [
            item(0, Kind::User("Go.".into())),
            item(1, Kind::Text("Going.".into())),
            item(2, Kind::Marker(Marker::Compaction)),
            item(
                3,
                Kind::Work {
                    from: "Late".into(),
                    text: "done".into(),
                },
            ),
            item(4, Kind::Text("The late fork is done.".into())),
        ];
        let path: Vec<&Item> = items.iter().collect();
        let q = |s: &str| quote(&path, s, s.parse().unwrap(), Whose::Session);
        assert_eq!(
            q("1").unwrap_err().to_string(),
            "Turn 1 is the report \"Late\", not your message, and its reply follows a fold, in no \
             answer to a message of yours."
        );
        assert_eq!(q("0").unwrap(), "> Going.\n");
        assert_eq!(q("").unwrap(), "> The late fork is done.\n");
    }

    fn id(e: usize) -> crate::agent::model::SourceId {
        crate::agent::model::SourceId {
            harness: "test".into(),
            session: "s".into(),
            entry: e.to_string(),
        }
    }

    /// A path through sessions, each a list of what its items say: an item follows the one
    /// before it, across sessions too, as a session attached to a thread follows the thread.
    fn sessions(of: &[&[Kind]]) -> Vec<Item> {
        let mut out: Vec<Item> = Vec::new();
        for (s, kinds) in of.iter().enumerate() {
            for kind in kinds.iter() {
                let id = crate::agent::model::SourceId {
                    session: format!("s-{s}"),
                    ..id(out.len())
                };
                out.push(Item {
                    id,
                    parent: out.last().map(|i| i.id.clone()),
                    ts: jiff::Timestamp::UNIX_EPOCH,
                    ends_turn: matches!(kind, Kind::Text(_)),
                    kind: kind.clone(),
                });
            }
        }
        out
    }

    fn user(s: &str) -> Kind {
        Kind::User(s.into())
    }

    fn text(s: &str) -> Kind {
        Kind::Text(s.into())
    }

    #[test]
    fn answers_count_back_into_the_sessions_before() {
        let before = [user("Go."), text("Going."), user("More."), text("Done.")];
        let items = sessions(&[&before, &[]]);
        let path: Vec<&Item> = items.iter().collect();
        let q = |s: &str| quote(&path, s, s.parse().unwrap(), Whose::Branch);
        assert_eq!(
            q("").unwrap(),
            "> Done.\n",
            "a session that has said nothing yet quotes the one before it"
        );

        let items = sessions(&[&before, &[user("And now?"), text("Now this.")]]);
        let path: Vec<&Item> = items.iter().collect();
        let q = |s: &str| quote(&path, s, s.parse().unwrap(), Whose::Branch);
        assert_eq!(q("-1").unwrap(), "> Now this.\n");
        assert_eq!(q("-2").unwrap(), "> Done.\n");
        assert_eq!(q("-3").unwrap(), "> Going.\n");
        assert_eq!(q("2").unwrap(), "> Now this.\n", "turns count on");
        assert_eq!(
            q("-4").unwrap_err().to_string(),
            "-4 is past the oldest answer: this branch has 3 answers."
        );
    }

    #[test]
    fn a_session_taking_the_thread_up_ends_the_answer_before_it() {
        let report = Kind::Work {
            from: "Late".into(),
            text: "done".into(),
        };
        let items = sessions(&[
            &[user("Start a fork."), text("A fork is on it.")],
            &[report, text("The late fork is done.")],
        ]);
        let path: Vec<&Item> = items.iter().collect();
        let q = |s: &str| quote(&path, s, s.parse().unwrap(), Whose::Branch);
        assert_eq!(
            q("").unwrap(),
            "> The late fork is done.\n",
            "the reply to a fork that reports after the fold is an answer of its own"
        );
        assert_eq!(q("-2").unwrap(), "> A fork is on it.\n");
        assert_eq!(q("0").unwrap(), "> A fork is on it.\n");
        assert_eq!(
            q("1").unwrap_err().to_string(),
            "Turn 1 is the report \"Late\", not your message, and its reply follows a fold, in no \
             answer to a message of yours."
        );
    }

    /// A background command's notification makes no item, so the reply to it follows the reply
    /// before the fold with no message between them: one turn, as the thread's view numbers it.
    #[test]
    fn a_reply_before_a_fold_stays_an_answer_when_the_next_session_writes_on_unasked() {
        let items = sessions(&[
            &[user("Build it."), text("The build runs in the background.")],
            &[text("The build passed.")],
        ]);
        let path: Vec<&Item> = items.iter().collect();
        assert_eq!(turns(&path).len(), 1);
        let q = |s: &str| quote(&path, s, s.parse().unwrap(), Whose::Branch);
        assert_eq!(q("").unwrap(), "> The build passed.\n");
        assert_eq!(q("-2").unwrap(), "> The build runs in the background.\n");
        assert_eq!(q("0").unwrap(), "> The build runs in the background.\n");
        assert_eq!(
            q("-3").unwrap_err().to_string(),
            "-3 is past the oldest answer: this branch has 2 answers."
        );
    }

    #[test]
    fn a_compaction_inside_a_turn_still_leaves_it_in_its_answer_in_a_later_session() {
        let call = Kind::ToolCall {
            id: "t".into(),
            name: "Bash".into(),
            input: serde_json::Value::Null,
        };
        let items = sessions(&[
            &[user("Go."), text("Going.")],
            &[
                user("Go on."),
                call,
                Kind::Marker(Marker::Compaction),
                text("Went on."),
            ],
        ]);
        let path: Vec<&Item> = items.iter().collect();
        let q = |s: &str| quote(&path, s, s.parse().unwrap(), Whose::Branch);
        assert_eq!(q("1").unwrap(), "> Went on.\n");
        assert_eq!(q("-1").unwrap(), "> Went on.\n");
        assert_eq!(q("-2").unwrap(), "> Going.\n");
    }

    #[test]
    fn quoting_marks_every_line() {
        assert_eq!(quoted("\n\na\n\n  b\n \nc  \n\n"), "> a\n>\n>   b\n>\n> c");
    }
}
