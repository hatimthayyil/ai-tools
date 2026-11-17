//! Errors as `ai` reports them: one message that names every cause.

/// `e` followed by every cause under it, outermost first. A cause comes after a space when the
/// message before it ends a sentence, else after `: `; one whose text is already in the message
/// adds nothing and is left out. Each part is written in its alternate form, which for gix's
/// errors leaves out the source location every frame carries.
pub fn line(e: &anyhow::Error) -> String {
    let mut out = String::new();
    for cause in e.chain() {
        let part = format!("{cause:#}");
        let part = part.trim();
        if part.is_empty() || out.contains(part) {
            continue;
        }
        if !out.is_empty() {
            out.push_str(match out.ends_with(['.', '!', '?', ':']) {
                true => " ",
                false => ": ",
            });
        }
        out.push_str(part);
    }
    out
}

#[cfg(test)]
mod tests {
    use std::io;

    use anyhow::{Context, anyhow};

    use super::*;

    #[test]
    fn every_cause_follows_the_message_above_it() {
        let e = Err::<(), _>(io::Error::other("disk full"))
            .context("Cannot write the index")
            .context("Cannot commit ~/ai")
            .unwrap_err();
        assert_eq!(
            line(&e),
            "Cannot commit ~/ai: Cannot write the index: disk full"
        );
    }

    #[test]
    fn a_cause_after_a_sentence_follows_a_space() {
        let e = Err::<(), _>(anyhow!("No such file."))
            .context("Not a hook input with a prompt.")
            .unwrap_err();
        assert_eq!(line(&e), "Not a hook input with a prompt. No such file.");
    }

    #[test]
    fn a_cause_the_message_already_holds_is_left_out() {
        let e = Err::<(), _>(io::Error::other("File exists"))
            .context("File exists")
            .context("The lock is held: File exists")
            .unwrap_err();
        assert_eq!(line(&e), "The lock is held: File exists");
    }

    #[test]
    fn an_error_without_causes_is_its_message() {
        assert_eq!(
            line(&anyhow!("Agent dev already exists.")),
            "Agent dev already exists."
        );
    }
}
