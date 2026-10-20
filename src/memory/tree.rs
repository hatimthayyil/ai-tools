use std::fmt;

pub const NODE: usize = 512;

/// The length a summariser is asked for, the only one it sees: about 365 bytes at the 7.3 bytes
/// a word summary lines average, so a model overshooting it still fits [`NODE`].
pub const WORDS: usize = 50;

/// The most bytes a line asked for in `words` words is accepted at: [`NODE`] for [`WORDS`], in
/// proportion, so every target keeps the same margin.
pub fn limit(words: usize) -> usize {
    words * NODE / WORDS
}

/// Node `(l, i)`: messages `[i·2^l, (i+1)·2^l)`, printed `id+n`, a leaf as a bare `id`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Coord {
    pub l: u32,
    pub i: u64,
}

impl Coord {
    pub fn new(l: u32, i: u64) -> Coord {
        Coord { l, i }
    }

    pub fn leaf(i: u64) -> Coord {
        Coord { l: 0, i }
    }

    pub fn id(self) -> u64 {
        self.i << self.l
    }

    pub fn n(self) -> u64 {
        1 << self.l
    }

    pub fn end(self) -> u64 {
        self.id() + self.n()
    }

    pub fn parent(self) -> Coord {
        Coord::new(self.l + 1, self.i / 2)
    }

    pub fn children(self) -> [Coord; 2] {
        let l = self.l - 1;
        [Coord::new(l, 2 * self.i), Coord::new(l, 2 * self.i + 1)]
    }

    /// The node `id+n` names, if it is one within `t` messages.
    pub fn at(id: u64, n: u64, t: u64) -> Option<Coord> {
        let ok = n.is_power_of_two() && id.is_multiple_of(n) && id.checked_add(n)? <= t;
        ok.then(|| Coord::new(n.trailing_zeros(), id / n))
    }

    /// `id+n`, or a bare `id` for `id+1`.
    pub fn parse(s: &str) -> Option<(u64, u64)> {
        let (id, n) = s.split_once('+').unwrap_or((s, "1"));
        let num = |p: &str| {
            p.bytes()
                .all(|b| b.is_ascii_digit())
                .then(|| p.parse().ok())
                .flatten()
        };
        Some((num(id)?, num(n)?))
    }
}

impl fmt::Display for Coord {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.l {
            0 => write!(f, "{}", self.i),
            _ => write!(f, "{}+{}", self.id(), self.n()),
        }
    }
}

/// A source that fits in [`NODE`] bytes is its own node: `kind: text`, or two lines joined.
pub fn free(text: String) -> Option<String> {
    (text.len() <= NODE).then_some(text)
}

/// `a` and `b` as one line; an empty side (hidden tool calls) adds nothing.
pub fn joined(a: &str, b: &str) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b.into(),
        (_, true) => a.into(),
        _ => format!("{a} {b}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addressing() {
        let c = Coord::new(3, 273);
        assert_eq!((c.id(), c.n(), c.end()), (2184, 8, 2192));
        assert_eq!(c.to_string(), "2184+8");
        assert_eq!(Coord::at(2184, 8, 2192), Some(c));
        assert_eq!(Coord::at(2184, 8, 2191), None);
        assert_eq!(Coord::at(2180, 8, 9999), None);
        assert_eq!(Coord::at(6, 3, 9999), None);
        assert_eq!(Coord::at(6, 0, 9999), None);
        assert_eq!(Coord::at(7, 1, 8), Some(Coord::leaf(7)));
        assert_eq!(Coord::leaf(7).to_string(), "7");
        assert_eq!(c.children(), [Coord::new(2, 546), Coord::new(2, 547)]);
        assert_eq!(c.parent(), Coord::new(4, 136));
        assert_eq!(Coord::parse("2184+8"), Some((2184, 8)));
        assert_eq!(Coord::parse("7"), Some((7, 1)));
        for bad in ["", "+1", "1+", "-1+2", "a+b", "1+2+3"] {
            assert_eq!(Coord::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn free_nodes_fit() {
        assert_eq!(free("x".repeat(NODE)).map(|s| s.len()), Some(NODE));
        assert_eq!(free("x".repeat(NODE + 1)), None);
        assert_eq!(joined("a", "b"), "a b");
    }
}
