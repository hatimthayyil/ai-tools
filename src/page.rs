use std::{io::Write, ops::Range};

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

/// The parts of a search's matches, given each match's size, oldest first: part 1 the newest
/// that fit in [`PART`] bytes (at least one), each later part the newest of those left.
pub fn newest_first(sizes: &[u64]) -> Vec<Range<usize>> {
    let (mut parts, mut end) = (Vec::new(), sizes.len());
    while end > 0 {
        let (mut start, mut size) = (end - 1, sizes[end - 1]);
        while start > 0 && size + sizes[start - 1] <= PART {
            start -= 1;
            size += sizes[start];
        }
        parts.push(start..end);
        end = start;
    }
    parts
}

/// Which of a search's matches to print and the footer after them, for `part` as [`pick`] gives
/// it: all of them for `None`, else that part of [`newest_first`]. `sizes` are the newest
/// matches kept of `hits`; `next` gives the command for part `k + 1`.
pub fn search(
    sizes: &[u64],
    hits: u64,
    part: Option<u64>,
    next: impl FnOnce(u64) -> String,
) -> Result<(Range<usize>, String)> {
    let kept = sizes.len() as u64;
    let all = match kept < hits {
        true => format!("Newest {kept} of {hits} matches."),
        false if hits == 1 => "1 match.".into(),
        false => format!("{hits} matches."),
    };
    let Some(k) = part else {
        return Ok((0..sizes.len(), all));
    };
    let parts = newest_first(sizes);
    let n = parts.len() as u64;
    let Some(r) = k.checked_sub(1).and_then(|i| parts.get(i as usize)) else {
        bail!("No part {k}: it has {n}.");
    };
    if n == 1 {
        return Ok((r.clone(), all));
    }
    let of = match kept < hits {
        true => format!("the newest {kept} of {hits} matches"),
        false => format!("{hits} matches"),
    };
    let mut footer = format!("Part {k} of {n}, newest first: {} of {of}.", r.len());
    if k < n {
        footer += &format!(" Next: {}", next(k + 1));
    }
    Ok((r.clone(), footer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn searches_page_the_newest_first() {
        assert_eq!(newest_first(&[]), Vec::<Range<usize>>::new());
        assert_eq!(newest_first(&[5, 15_000, 4_000, 3_000]), [2..4, 0..2]);
        assert_eq!(newest_first(&[30_000, 1]), [1..2, 0..1]);
        let next = |k| format!("next {k}");
        assert_eq!(
            search(&[1, 2], 2, None, next).unwrap(),
            (0..2, "2 matches.".into())
        );
        assert_eq!(
            search(&[1], 1, Some(1), next).unwrap(),
            (0..1, "1 match.".into())
        );
        assert_eq!(
            search(&[1, 2], 9, None, next).unwrap(),
            (0..2, "Newest 2 of 9 matches.".into())
        );
        let sizes = [15_000, 15_000, 15_000];
        assert_eq!(
            search(&sizes, 3, Some(1), next).unwrap(),
            (
                2..3,
                "Part 1 of 3, newest first: 1 of 3 matches. Next: next 2".into()
            )
        );
        assert_eq!(
            search(&sizes, 5, Some(3), next).unwrap(),
            (
                0..1,
                "Part 3 of 3, newest first: 1 of the newest 3 of 5 matches.".into()
            )
        );
        assert!(search(&sizes, 3, Some(4), next).is_err());
        assert!(search(&sizes, 3, Some(0), next).is_err());
    }

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
