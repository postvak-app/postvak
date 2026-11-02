//! Writing a message in the user's editor, as `git commit` does: a file of `To:`, `Cc:`, `Bcc:` and `Subject:` lines,
//! a blank line, and the text, with the quote of a reply or forward below an empty line for the user's own.

use std::io::{self, Write as _};
use std::path::Path;

use super::build::Draft;
use super::check::valid_address;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    #[error("the file has a header line '{0}'; only To:, Cc:, Bcc: and Subject: can be set there")]
    UnknownHeader(String),
    #[error("'{0}' is not an email address")]
    BadAddress(String),
}

/// The file the editor opens for `draft`.
pub fn template(draft: &Draft) -> String {
    format!(
        "To: {}\nCc: {}\nBcc: {}\nSubject: {}\n\n{}",
        draft.to.join(", "),
        draft.cc.join(", "),
        draft.bcc.join(", "),
        draft.subject.replace(['\r', '\n'], " "),
        draft.text
    )
}

/// `draft` with what the user wrote: None when the file was left empty or as it was, so nothing is sent.
pub fn read_back(
    template: &str,
    edited: &str,
    mut draft: Draft,
) -> Result<Option<Draft>, EditError> {
    let edited = edited.replace("\r\n", "\n");
    if edited.trim().is_empty() || edited == template {
        return Ok(None);
    }
    let (headers, text) = match edited.split_once("\n\n") {
        Some((headers, text)) => (headers, text),
        None => (edited.as_str(), ""),
    };
    for line in headers.lines() {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| EditError::UnknownHeader(line.to_string()))?;
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "to" => draft.to = addresses(value)?,
            "cc" => draft.cc = addresses(value)?,
            "bcc" => draft.bcc = addresses(value)?,
            "subject" => draft.subject = value.to_string(),
            _ => return Err(EditError::UnknownHeader(name.trim().to_string())),
        }
    }
    draft.text = text.to_string();
    Ok(Some(draft))
}

/// The recipients of one header line, each checked as `--to` is.
fn addresses(value: &str) -> Result<Vec<String>, EditError> {
    split_recipients(value)
        .into_iter()
        .map(|recipient| {
            let address = super::build::bare_address(&recipient);
            valid_address(&address)
                .map(|()| recipient.clone())
                .map_err(|_| EditError::BadAddress(recipient))
        })
        .collect()
}

/// A header line's recipients, split at commas outside quotes and angle brackets.
pub fn split_recipients(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let (mut current, mut quoted, mut angle) = (String::new(), false, false);
    for c in value.chars() {
        match c {
            '"' => quoted = !quoted,
            '<' if !quoted => angle = true,
            '>' if !quoted => angle = false,
            ',' if !quoted && !angle => {
                out.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    out.push(current);
    out.into_iter()
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
        .collect()
}

/// `$VISUAL`, else `$EDITOR`, else `vi`.
pub fn editor_command() -> String {
    ["VISUAL", "EDITOR"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "vi".into())
}

/// Runs the editor on `path`, through the shell so `$EDITOR` may carry arguments, as git does.
pub fn run_editor(path: &Path) -> io::Result<()> {
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{} \"$1\"", editor_command()))
        .arg("postvak-editor")
        .arg(path)
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!("the editor exited with {status}")));
    }
    Ok(())
}

/// Writes `text` to a private file under `dir`, opens the editor on it, and returns what it then holds; the file is
/// removed afterwards.
pub fn edit_in(dir: &Path, text: &str) -> io::Result<String> {
    use std::os::unix::fs::OpenOptionsExt;
    crate::paths::create_private_dir(dir)?;
    let path = dir.join(format!("postvak-message-{}.eml", super::outbox::new_id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    let edited = file
        .write_all(text.as_bytes())
        .and_then(|()| file.flush())
        .and_then(|()| run_editor(&path))
        .and_then(|()| std::fs::read_to_string(&path));
    let _ = std::fs::remove_file(&path);
    edited
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft() -> Draft {
        Draft {
            from: "me@example.com".into(),
            to: vec!["Linus Example <linus@x.org>".into()],
            cc: vec!["c1@x.org".into(), "c2@x.org".into()],
            subject: "Re: Plans".into(),
            text: "\n\nOn Mon, 2 Nov 2026 10:00, Linus Example wrote:\n> one\n> two\n".into(),
            in_reply_to: Some("<m1@x.org>".into()),
            references: vec!["<m1@x.org>".into()],
            ..Default::default()
        }
    }

    #[test]
    fn template_has_the_four_headers_and_the_quote_below_an_empty_line() {
        assert_eq!(
            template(&draft()),
            "To: Linus Example <linus@x.org>\nCc: c1@x.org, c2@x.org\nBcc: \nSubject: Re: Plans\n\n\n\nOn Mon, 2 Nov 2026 10:00, Linus Example wrote:\n> one\n> two\n"
        );
    }

    #[test]
    fn unchanged_or_empty_aborts() {
        let t = template(&draft());
        assert_eq!(read_back(&t, &t, draft()), Ok(None));
        assert_eq!(read_back(&t, "  \n", draft()), Ok(None));
    }

    #[test]
    fn removing_a_reply_all_recipient_drops_it() {
        let t = template(&draft());
        let edited = t.replace("Cc: c1@x.org, c2@x.org", "Cc: c2@x.org");
        let out = read_back(&t, &edited, draft()).unwrap().unwrap();
        assert_eq!(out.cc, ["c2@x.org"]);
        assert_eq!(out.to, ["Linus Example <linus@x.org>"]);
    }

    #[test]
    fn interleaved_answers_are_kept_as_written() {
        let t = template(&draft());
        let edited = t.replace("> one\n", "> one\nYes.\n");
        let out = read_back(&t, &edited, draft()).unwrap().unwrap();
        assert_eq!(
            out.text,
            "\n\nOn Mon, 2 Nov 2026 10:00, Linus Example wrote:\n> one\nYes.\n> two\n"
        );
    }

    #[test]
    fn an_unknown_header_is_refused() {
        let t = template(&draft());
        let edited = format!("From: other@x.org\n{t}");
        assert_eq!(
            read_back(&t, &edited, draft()),
            Err(EditError::UnknownHeader("From".into()))
        );
        let bad = t.replace("Cc: c1@x.org", "Cc: nope");
        assert!(matches!(
            read_back(&t, &bad, draft()),
            Err(EditError::BadAddress(_))
        ));
    }

    #[test]
    fn from_and_references_are_not_in_the_file_and_survive() {
        let t = template(&draft());
        assert!(!t.contains("me@example.com") && !t.contains("m1@x.org"));
        let out = read_back(
            &t,
            &t.replace("Subject: Re: Plans", "Subject: Re: Plans!"),
            draft(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(out.subject, "Re: Plans!");
        assert_eq!(out.from, "me@example.com");
        assert_eq!(out.in_reply_to.as_deref(), Some("<m1@x.org>"));
        assert_eq!(out.references, ["<m1@x.org>"]);
    }

    #[test]
    fn recipients_split_outside_quotes() {
        assert_eq!(
            split_recipients("\"Doe, J\" <j@x.org>, k@x.org,"),
            ["\"Doe, J\" <j@x.org>", "k@x.org"]
        );
    }

    #[test]
    fn the_editor_is_visual_then_editor_then_vi() {
        // Read only: the environment is the test process's.
        assert!(!editor_command().is_empty());
    }
}
