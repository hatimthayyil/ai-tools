use std::io::Write;

use anyhow::{Result, bail};

/// The largest part of a paged output, in bytes: under Claude Code's 30,000-character cap on
/// a command's output.
pub const PART: u64 = 20_000;

/// `text` in parts of at most `max` bytes, each cut after its last line end when that keeps
/// at least half of it, else at a character boundary.
pub fn pages(text: &str, max: u64) -> Vec<&str> {
    let max = max.max(1) as usize;
    let (mut out, mut rest) = (Vec::new(), text);
    while rest.len() > max {
        let cut = rest.floor_char_boundary(max);
        let at = match rest[..cut].rfind('\n') {
            Some(k) if k + 1 >= cut / 2 => k + 1,
            _ if cut == 0 => rest.ceil_char_boundary(1),
            _ => cut,
        };
        out.push(&rest[..at]);
        rest = &rest[at..];
    }
    if !rest.is_empty() || out.is_empty() {
        out.push(rest);
    }
    out
}

/// Part `part` (from 1) of `text` paged by [`pages`], and how many parts there are.
pub fn page(text: &str, max: u64, part: u64) -> Result<(&str, u64)> {
    let all = pages(text, max);
    let n = all.len() as u64;
    match part.checked_sub(1).and_then(|k| all.get(k as usize)) {
        Some(p) => Ok((p, n)),
        None => bail!("No part {part}: it has {n}."),
    }
}

/// The part of a view to print: the one asked for, else the whole (`None`) when `whole`, as on
/// a terminal, else the first.
pub fn pick(part: Option<u64>, whole: bool) -> Option<u64> {
    part.or((!whole).then_some(1))
}

/// Prints `text`: whole for `part` `None`, else that part of its [`PART`]-byte parts, ending
/// with `Part k of n.` and, while parts remain, the command `next` gives for part `k + 1`.
pub fn print(
    out: &mut dyn Write,
    text: &str,
    part: Option<u64>,
    next: impl FnOnce(u64) -> String,
) -> Result<()> {
    let (shown, n) = match part {
        Some(k) => page(text, PART, k)?,
        None => (text, 1),
    };
    writeln!(out, "{}", shown.strip_suffix('\n').unwrap_or(shown))?;
    match part {
        Some(k) if k < n => writeln!(out, "Part {k} of {n}. Next: {}", next(k + 1))?,
        Some(k) if n > 1 => writeln!(out, "Part {k} of {n}.")?,
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_cut_at_line_ends_and_characters() {
        assert_eq!(pages("abc", 10), ["abc"]);
        assert_eq!(pages("ab\ncdef\ngh", 8), ["ab\ncdef\n", "gh"]);
        assert_eq!(pages("a\nbcdefgh", 6), ["a\nbcde", "fgh"]);
        assert_eq!(pages("ééé", 3), ["é", "é", "é"]);
        assert_eq!(pages("ééé", 1), ["é", "é", "é"]);
        assert_eq!(page("ab\ncd", 3, 2).unwrap(), ("cd", 2));
        assert!(page("ab", 3, 2).is_err() && page("ab", 3, 0).is_err());
    }

    #[test]
    fn a_terminal_reads_whole_unless_a_part_is_asked() {
        assert_eq!(pick(None, true), None);
        assert_eq!(pick(None, false), Some(1));
        assert_eq!(pick(Some(2), true), Some(2));
    }

    #[test]
    fn parts_end_with_the_next_command() {
        let text = format!("{}\n{}\n", "a".repeat(15_000), "b".repeat(15_000));
        let print = |part| {
            let mut out = Vec::new();
            super::print(&mut out, &text, part, |k| format!("next {k}")).unwrap();
            String::from_utf8(out).unwrap()
        };
        assert_eq!(print(None), text);
        let first = print(Some(1));
        assert!(first.ends_with("a\nPart 1 of 2. Next: next 2\n"), "{first}");
        assert!(print(Some(2)).ends_with("b\nPart 2 of 2.\n"));
        let short = |part| {
            let mut out = Vec::new();
            super::print(&mut out, "x\n", part, |_| unreachable!()).unwrap();
            String::from_utf8(out).unwrap()
        };
        assert_eq!((short(None), short(Some(1))), ("x\n".into(), "x\n".into()));
    }
}
