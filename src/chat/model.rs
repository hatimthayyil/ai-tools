use std::{collections::HashMap, ops::Range};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One entry of one harness session.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SourceId {
    pub harness: String,
    pub session: String,
    pub entry: String,
}

/// What an item says.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "content", rename_all = "snake_case")]
pub enum Kind {
    /// The user's words.
    User(String),
    /// A report or message from another agent, which the assistant answers.
    Work {
        from: String,
        text: String,
    },
    /// The assistant's words.
    Text(String),
    ToolCall {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        id: String,
        output: String,
        is_error: bool,
    },
    Marker(Marker),
}

/// A harness event that is not content.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Marker {
    /// The harness compacted its context here.
    Compaction,
    /// The harness's own summary of the context it compacted.
    Summary,
    /// The user stopped the run.
    Interrupt,
}

impl Kind {
    /// Whether it opens a turn: a message from the user or another agent.
    pub fn opens(&self) -> bool {
        matches!(self, Kind::User(_) | Kind::Work { .. })
    }
}

/// One normalised source entry.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub id: SourceId,
    pub parent: Option<SourceId>,
    pub ts: Timestamp,
    pub kind: Kind,
    /// The harness marks it as the end of the assistant's turn.
    pub ends_turn: bool,
}

/// Where an item sits on the branch from its root: its turn, numbered from 0, and its number
/// in the turn, 0 for the message that opens it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Place {
    pub turn: u64,
    pub item: u64,
}

/// Items linked by their parents. A parent that is not an earlier item makes its item a root.
pub struct Tree {
    items: Vec<Item>,
    parents: Vec<Option<usize>>,
    tips: Vec<bool>,
    places: Vec<Place>,
}

impl Tree {
    pub fn new(items: Vec<Item>) -> Tree {
        let mut at = HashMap::with_capacity(items.len());
        let mut parents = Vec::with_capacity(items.len());
        let mut tips = vec![true; items.len()];
        let mut places: Vec<Place> = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            let p = item.parent.as_ref().and_then(|p| at.get(p).copied());
            if let Some(p) = p {
                tips[p] = false;
            }
            places.push(match p.map(|p: usize| places[p]) {
                None => Place { turn: 0, item: 0 },
                Some(q) if item.kind.opens() => Place {
                    turn: q.turn + 1,
                    item: 0,
                },
                Some(q) => Place {
                    turn: q.turn,
                    item: q.item + 1,
                },
            });
            parents.push(p);
            at.insert(&item.id, i);
        }
        Tree {
            items,
            parents,
            tips,
            places,
        }
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// Where item `i` sits on its branch.
    pub fn place(&self, i: usize) -> Place {
        self.places[i]
    }

    /// The items nothing follows, in item order: the last one is the newest item.
    pub fn tips(&self) -> Vec<usize> {
        (0..self.items.len()).filter(|&i| self.tips[i]).collect()
    }

    /// The positions from the root down to `tip`.
    pub fn branch(&self, tip: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut at = Some(tip);
        while let Some(i) = at {
            out.push(i);
            at = self.parents[i];
        }
        out.reverse();
        out
    }

    /// The items from the root down to `tip`.
    pub fn path(&self, tip: usize) -> Vec<&Item> {
        self.branch(tip)
            .into_iter()
            .map(|i| &self.items[i])
            .collect()
    }
}

/// A message (from the user or another agent) and the steps that answer it, up to the next
/// message on the path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Turn {
    /// Positions on the path. It starts with the message, unless the path does not.
    pub span: Range<usize>,
    /// The rest of the span, cut before each assistant text.
    pub steps: Vec<Range<usize>>,
    /// A message follows it on the path, or its last item that is not a marker ends it.
    pub closed: bool,
    /// The position its lines are keyed by: the item that ends it, so markers after it change
    /// no key; else its last.
    pub last: usize,
    /// Its final reply: the item that ends it, when that is the assistant's words. Only
    /// markers follow it, so it starts the last step.
    pub reply: Option<usize>,
}

impl Turn {
    /// Its work: the steps between its message and its final reply; every step while it has
    /// none.
    pub fn work(&self) -> &[Range<usize>] {
        &self.steps[..self.steps.len() - usize::from(self.reply.is_some())]
    }
}

/// The turns along `path`, in order.
pub fn turns(path: &[&Item]) -> Vec<Turn> {
    let user = |i: usize| path[i].kind.opens();
    let mut starts: Vec<usize> = (0..path.len()).filter(|&i| user(i)).collect();
    if !path.is_empty() && !user(0) {
        starts.insert(0, 0);
    }
    let ends: Vec<usize> = starts.iter().skip(1).copied().chain([path.len()]).collect();
    starts
        .iter()
        .zip(&ends)
        .map(|(&start, &end)| {
            let mut steps: Vec<Range<usize>> = Vec::new();
            let body = start + usize::from(user(start));
            for (i, item) in (body..end).zip(&path[body..end]) {
                match steps.last_mut() {
                    Some(s) if !matches!(item.kind, Kind::Text(_)) => s.end = i + 1,
                    _ => steps.push(i..i + 1),
                }
            }
            let ending = (start..end)
                .rev()
                .find(|&i| !matches!(path[i].kind, Kind::Marker(_)))
                .filter(|&i| path[i].ends_turn);
            Turn {
                span: start..end,
                steps,
                closed: end < path.len() || ending.is_some(),
                last: ending.unwrap_or(end - 1),
                reply: ending.filter(|&i| matches!(path[i].kind, Kind::Text(_))),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(entry: &str) -> SourceId {
        SourceId {
            harness: "test".into(),
            session: "s".into(),
            entry: entry.into(),
        }
    }

    /// Items from `(entry, parent, kind)`: `u` user, `w` work, `t` text, `e` text that ends the
    /// turn, `c` call, `r` result, `m` marker.
    fn items(spec: &[(&str, &str, char)]) -> Vec<Item> {
        spec.iter()
            .map(|&(e, p, k)| Item {
                id: id(e),
                parent: (!p.is_empty()).then(|| id(p)),
                ts: Timestamp::UNIX_EPOCH,
                kind: match k {
                    'u' => Kind::User(e.into()),
                    'w' => Kind::Work {
                        from: "agent".into(),
                        text: e.into(),
                    },
                    't' | 'e' => Kind::Text(e.into()),
                    'c' => Kind::ToolCall {
                        id: e.into(),
                        name: "Read".into(),
                        input: Value::Null,
                    },
                    'r' => Kind::ToolResult {
                        id: e.into(),
                        output: String::new(),
                        is_error: false,
                    },
                    _ => Kind::Marker(Marker::Compaction),
                },
                ends_turn: k == 'e',
            })
            .collect()
    }

    fn entries(path: &[&Item]) -> Vec<String> {
        path.iter().map(|i| i.id.entry.clone()).collect()
    }

    #[test]
    fn tips_and_paths_follow_parents() {
        let tree = Tree::new(items(&[
            ("u1", "", 'u'),
            ("t1", "u1", 't'),
            ("u2", "t1", 'u'),
            ("t2", "u2", 't'),
            ("u2b", "t1", 'u'),
            ("t3", "u2b", 't'),
            ("x", "gone", 't'),
            ("y", "z", 't'),
            ("z", "", 't'),
        ]));
        assert_eq!(tree.tips(), [3, 5, 6, 7, 8]);
        assert_eq!(entries(&tree.path(3)), ["u1", "t1", "u2", "t2"]);
        assert_eq!(tree.branch(5), [0, 1, 4, 5]);
        assert_eq!(entries(&tree.path(5)), ["u1", "t1", "u2b", "t3"]);
        assert_eq!(entries(&tree.path(6)), ["x"]);
        assert_eq!(entries(&tree.path(7)), ["y"], "a parent must come first");
    }

    #[test]
    fn turns_and_steps() {
        let all = items(&[
            ("t0", "", 't'),
            ("u1", "", 'u'),
            ("t1", "", 't'),
            ("c1", "", 'c'),
            ("r1", "", 'r'),
            ("c2", "", 'c'),
            ("r2", "", 'r'),
            ("t2", "", 't'),
            ("c3", "", 'c'),
            ("r3", "", 'r'),
            ("t3", "", 't'),
            ("m", "", 'm'),
            ("u2", "", 'u'),
            ("c4", "", 'c'),
            ("r4", "", 'r'),
            ("t4", "", 't'),
            ("w1", "", 'w'),
        ]);
        let path: Vec<&Item> = all.iter().collect();
        let x = |span: Range<usize>, steps: &[(usize, usize)], closed: bool| Turn {
            last: span.end - 1,
            span,
            steps: steps.iter().map(|&(a, b)| a..b).collect(),
            closed,
            reply: None,
        };
        assert_eq!(
            turns(&path),
            [
                x(0..1, &[(0, 1)], true),
                x(1..12, &[(2, 7), (7, 10), (10, 12)], true),
                x(12..16, &[(13, 15), (15, 16)], true),
                x(16..17, &[], false),
            ]
        );
        assert_eq!(turns(&path[1..7]), [x(0..6, &[(1, 6)], false)]);
        assert_eq!(turns(&[]), []);
    }

    #[test]
    fn a_turn_closes_where_the_harness_ends_it() {
        let all = items(&[
            ("u1", "", 'u'),
            ("c1", "", 'c'),
            ("r1", "", 'r'),
            ("e1", "", 'e'),
            ("m1", "", 'm'),
            ("m2", "", 'm'),
            ("u2", "", 'u'),
            ("e2", "", 'e'),
            ("c2", "", 'c'),
        ]);
        let path: Vec<&Item> = all.iter().collect();
        let ends = |n: usize| {
            turns(&path[..n])
                .iter()
                .map(|x| (x.closed, x.last))
                .collect::<Vec<_>>()
        };
        assert_eq!(ends(3), [(false, 2)], "no end yet");
        assert_eq!(ends(4), [(true, 3)]);
        assert_eq!(ends(6), [(true, 3)], "markers after the end move nothing");
        assert_eq!(ends(8), [(true, 3), (true, 7)]);
        assert_eq!(
            ends(9),
            [(true, 3), (false, 8)],
            "a step after the end reopens the turn"
        );
    }

    #[test]
    fn a_turn_s_work_runs_from_its_message_to_its_final_reply() {
        let all = items(&[
            ("u1", "", 'u'),
            ("t1", "", 't'),
            ("c1", "", 'c'),
            ("r1", "", 'r'),
            ("e1", "", 'e'),
            ("m1", "", 'm'),
            ("u2", "", 'u'),
            ("e2", "", 'e'),
            ("u3", "", 'u'),
            ("t3", "", 't'),
            ("c3", "", 'c'),
            ("u4", "", 'u'),
            ("c4", "", 'c'),
            ("e4", "", 'e'),
            ("c5", "", 'c'),
        ]);
        let path: Vec<&Item> = all.iter().collect();
        let parts = |n: usize| {
            turns(&path[..n])
                .iter()
                .map(|x| (x.reply, x.work().iter().map(|r| (r.start, r.end)).collect()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            parts(11),
            [
                (Some(4), vec![(1, 4)]),
                (Some(7), vec![]),
                (None, vec![(9, 11)]),
            ],
            "the reply and the markers after it are no work; a turn a message cuts short has none"
        );
        assert_eq!(parts(14)[3], (Some(13), vec![(12, 13)]));
        assert_eq!(
            parts(15)[3],
            (None, vec![(12, 13), (13, 15)]),
            "a step after the end makes the reply work"
        );
    }

    #[test]
    fn places_count_turns_along_each_branch() {
        let tree = Tree::new(items(&[
            ("t0", "", 't'),
            ("u1", "t0", 'u'),
            ("t1", "u1", 't'),
            ("c1", "t1", 'c'),
            ("u2", "c1", 'u'),
            ("u2b", "t1", 'u'),
            ("t3", "u2b", 't'),
            ("u9", "", 'u'),
        ]));
        let place = |i| {
            let p = tree.place(i);
            (p.turn, p.item)
        };
        assert_eq!(
            (0..8).map(place).collect::<Vec<_>>(),
            [
                (0, 0),
                (1, 0),
                (1, 1),
                (1, 2),
                (2, 0),
                (2, 0),
                (2, 1),
                (0, 0)
            ]
        );
        for tip in tree.tips() {
            let path = tree.path(tip);
            let branch = tree.branch(tip);
            for (t, turn) in turns(&path).iter().enumerate() {
                for p in turn.span.clone() {
                    let want = (t as u64, (p - turn.span.start) as u64);
                    assert_eq!(place(branch[p]), want, "tip {tip}, position {p}");
                }
            }
        }
    }
}
