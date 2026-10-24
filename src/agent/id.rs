use std::fmt;

use crate::{agent::model::Place, memory::Coord};

/// A line or item of a thread's branch, as printed. Turns are numbered from 0 along the
/// branch; items from 0 within their turn, item 0 being the message that opens it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Id {
    /// Turns `first` to `first + n - 1`: `t`, or `t+n`.
    Turns { first: u64, n: u64 },
    /// Items `first` to `first + n - 1` of turn `turn`: `t.i`, or `t.i+n`.
    Items { turn: u64, first: u64, n: u64 },
}

impl Id {
    pub fn item(p: Place) -> Id {
        Id::Items {
            turn: p.turn,
            first: p.item,
            n: 1,
        }
    }

    /// The line of `level` over the items `first` to `last`: a chunk line (0), a turn line (1)
    /// or a node over turns.
    pub fn line(level: u32, first: Place, last: Place) -> Id {
        match level {
            0 => Id::Items {
                turn: first.turn,
                first: first.item,
                n: last.item - first.item + 1,
            },
            _ => Id::Turns {
                first: first.turn,
                n: last.turn - first.turn + 1,
            },
        }
    }

    /// The turns node `c` covers.
    pub fn node(c: Coord) -> Id {
        Id::Turns {
            first: c.id(),
            n: c.n(),
        }
    }

    pub fn parse(s: &str) -> Option<Id> {
        let num = |p: &str| {
            (!p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
                .then(|| p.parse::<u64>().ok())
                .flatten()
        };
        let (head, n) = match s.split_once('+') {
            Some((h, n)) => (h, num(n).filter(|&n| n > 0)?),
            None => (s, 1),
        };
        Some(match head.split_once('.') {
            Some((t, i)) => Id::Items {
                turn: num(t)?,
                first: num(i)?,
                n,
            },
            None => Id::Turns {
                first: num(head)?,
                n,
            },
        })
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let (head, n) = match *self {
            Id::Turns { first, n } => (first.to_string(), n),
            Id::Items { turn, first, n } => (format!("{turn}.{first}"), n),
        };
        match n {
            1 => write!(f, "{head}"),
            n => write!(f, "{head}+{n}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_read_back() {
        let turns = |first, n| Id::Turns { first, n };
        let items = |turn, first, n| Id::Items { turn, first, n };
        for (s, id) in [
            ("12", turns(12, 1)),
            ("8+4", turns(8, 4)),
            ("5+3", turns(5, 3)),
            ("12.3", items(12, 3, 1)),
            ("12.0", items(12, 0, 1)),
            ("12.3+7", items(12, 3, 7)),
        ] {
            assert_eq!(Id::parse(s), Some(id), "{s}");
            assert_eq!(id.to_string(), s);
        }
        assert_eq!(Id::parse("7+1").unwrap().to_string(), "7");
        assert_eq!(Id::parse("7.2+1").unwrap().to_string(), "7.2");
        for bad in [
            "", "+", "7+", "+4", "7+0", "7.", ".3", "7.3.1", "7-9", "#7", " 7", "7+2+2", "a", "1.x",
        ] {
            assert_eq!(Id::parse(bad), None, "{bad}");
        }
        assert_eq!(Id::node(Coord::new(2, 3)).to_string(), "12+4");
        assert_eq!(Id::node(Coord::leaf(5)).to_string(), "5");
    }
}
