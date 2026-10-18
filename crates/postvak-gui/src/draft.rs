//! Rules in plain language: the one-line summary the Rules view shows for a rule, and what a preview found.
use postvak_core::message::clean;
use postvak_core::rules::{Action, Match, OneOrMany, Rule};

/// `subject contains "a" or "b"`: a text condition, whose list matches any of its values.
fn text_label(
    field: &str,
    contains: Option<&OneOrMany<String>>,
    equals: Option<&OneOrMany<String>>,
    regex: Option<&String>,
) -> String {
    let quoted = |values: &OneOrMany<String>| {
        let quoted: Vec<String> = values
            .as_slice()
            .iter()
            .map(|v| format!("\"{v}\""))
            .collect();
        quoted.join(" or ")
    };
    match (contains, equals) {
        (Some(values), _) => format!("{field} contains {}", quoted(values)),
        (None, Some(values)) => format!("{field} equals {}", quoted(values)),
        (None, None) => format!(
            "{field} matches regex \"{}\"",
            regex.map_or("", String::as_str)
        ),
    }
}

/// Each condition of `m` in words, in the order of the rules docs; `any` and `none` list their entries, each entry's
/// conditions joined by "and".
pub(crate) fn match_labels(m: &Match) -> Vec<String> {
    let mut labels = Vec::new();
    for (field, condition) in [
        ("from", &m.from),
        ("to", &m.to),
        ("cc", &m.cc),
        ("subject", &m.subject),
        ("body", &m.body),
    ] {
        if let Some(t) = condition {
            labels.push(text_label(
                field,
                t.contains.as_ref(),
                t.equals.as_ref(),
                t.regex.as_ref(),
            ));
        }
    }
    for h in m
        .header
        .as_ref()
        .map(OneOrMany::as_slice)
        .unwrap_or_default()
    {
        let field = format!("header {}", h.name);
        labels.push(text_label(
            &field,
            h.contains.as_ref(),
            h.equals.as_ref(),
            h.regex.as_ref(),
        ));
    }
    labels.extend(m.older_than.as_ref().map(|v| format!("older than {v}")));
    labels.extend(m.larger_than.as_ref().map(|v| format!("larger than {v}")));
    labels.extend(
        m.seen
            .map(|read| if read { "read" } else { "unread" }.to_string()),
    );
    labels.extend(
        m.flagged
            .map(|flagged| if flagged { "flagged" } else { "not flagged" }.to_string()),
    );
    labels.extend(
        m.to_me
            .map(|to_me| if to_me { "to me" } else { "not to me" }.to_string()),
    );
    labels.extend(m.alias.as_ref().map(|v| format!("alias {v}")));
    labels.extend(m.tag.as_ref().map(|v| format!("tag {v}")));
    for (word, entries) in [("any", &m.any), ("none", &m.none)] {
        if let Some(entries) = entries {
            let entries: Vec<String> = entries
                .iter()
                .map(|e| match_labels(e).join(" and "))
                .collect();
            labels.push(format!("{word} of ({})", entries.join("; ")));
        }
    }
    labels
}

pub(crate) fn action_label(action: &Action) -> String {
    match action {
        Action::MarkRead => "mark read".into(),
        Action::Move(folder) => format!("move to {folder}"),
        Action::Tag(keyword) => format!("tag {keyword}"),
        other => other.label(),
    }
}

/// `subject contains "code" → delete`: what a rule matches and what it does, one line, without control characters.
pub(crate) fn summary(rule: &Rule) -> String {
    let actions: Vec<String> = rule.actions.iter().map(action_label).collect();
    clean(
        &format!(
            "{} → {}",
            match_labels(&rule.matches).join(", "),
            actions.join(", ")
        ),
        false,
    )
}

/// "would delete 4 cached messages": what a preview on cached mail found. `needs_body` is
/// `CompiledRule::needs_body`, which also looks inside `any` and `none`.
pub(crate) fn preview_text(rule: &Rule, matched: usize, needs_body: bool) -> String {
    if matched == 0 {
        return "matches no cached message".into();
    }
    let verbs: Vec<String> = rule
        .actions
        .iter()
        .map(|action| match action {
            Action::Move(_) => "move".into(),
            Action::Notify => "notify about".into(),
            Action::Silent => "silence".into(),
            Action::Stop => "stop later rules for".into(),
            Action::Tag(_) => "tag".into(),
            other => action_label(other),
        })
        .collect();
    let target = rule
        .actions
        .iter()
        .find_map(|action| match action {
            Action::Move(folder) => Some(format!(" to {folder}")),
            _ => None,
        })
        .unwrap_or_default();
    let noun = if matched == 1 { "message" } else { "messages" };
    let body = if needs_body {
        "; only mail whose body is downloaded is checked"
    } else {
        ""
    };
    clean(
        &format!(
            "would {} {matched} cached {noun}{target}{body}",
            verbs.join(" and ")
        ),
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use postvak_core::rules::parse;

    const ALL_FIELDS: &str = r#"
[[rules]]
name = "everything"
match.from = { equals = "a@example.com" }
match.to = { contains = "team" }
match.cc = { regex = "(?i)boss" }
match.subject = { contains = "code" }
match.body = { regex = "\\d{6}" }
match.header = { name = "List-Id", contains = "github.com" }
match.older_than = "1h"
match.larger_than = "5MB"
match.seen = true
match.flagged = false
match.to_me = false
match.alias = "*@shop.example.com"
match.tag = "$label1"
actions = [{ move = "Codes" }, { tag = "$label2" }, "mark_read", "flag", "silent", "stop"]
"#;

    /// What the form cannot edit: value lists, a header list, `any` and `none`.
    const GROUPED: &str = r#"
[[rules]]
name = "grouped"
match.subject = { contains = ["receipt", "invoice"] }
match.from = { contains = "shop" }
match.header = [{ name = "List-Id", contains = "a" }, { name = "X-Kind", equals = "b" }]
match.any = [{ seen = true }, { to_me = false, alias = "x@example.com" }]
match.none = [{ tag = "$label1" }]
actions = ["flag"]
"#;

    fn rule(text: &str) -> Rule {
        parse(text).unwrap().rules.remove(0)
    }

    #[test]
    fn the_summary_reads_conditions_then_actions() {
        assert_eq!(
            summary(&rule(
                "[[rules]]\nname = \"x\"\nmatch.subject = { contains = \"verification code\" }\nactions = [\"delete\"]\n"
            )),
            "subject contains \"verification code\" → delete"
        );
        assert_eq!(
            summary(&rule(ALL_FIELDS)),
            "from equals \"a@example.com\", to contains \"team\", cc matches regex \"(?i)boss\", \
             subject contains \"code\", body matches regex \"\\d{6}\", header List-Id contains \"github.com\", \
             older than 1h, larger than 5MB, read, not flagged, not to me, alias *@shop.example.com, tag $label1 \
             → move to Codes, tag $label2, mark read, flag, silent, stop"
        );
    }

    #[test]
    fn the_summary_reads_lists_and_groups() {
        assert_eq!(
            summary(&rule(GROUPED)),
            "from contains \"shop\", subject contains \"receipt\" or \"invoice\", header List-Id contains \"a\", \
             header X-Kind equals \"b\", any of (read; not to me and alias x@example.com), none of (tag $label1) \
             → flag"
        );
    }

    #[test]
    fn the_summary_drops_control_characters() {
        let sneaky = rule(
            "[[rules]]\nname = \"x\"\nmatch.subject = { contains = \"\\u001b[2Jcode\" }\nactions = [\"delete\"]\n",
        );
        assert_eq!(summary(&sneaky), "subject contains \"[2Jcode\" → delete");
    }

    #[test]
    fn the_preview_line_counts_messages_and_names_the_actions() {
        let delete = rule("[[rules]]\nname = \"x\"\nmatch.seen = true\nactions = [\"delete\"]\n");
        assert_eq!(
            preview_text(&delete, 4, false),
            "would delete 4 cached messages"
        );
        assert_eq!(
            preview_text(&delete, 1, false),
            "would delete 1 cached message"
        );
        assert_eq!(preview_text(&delete, 0, false), "matches no cached message");
        let filed = rule(
            "[[rules]]\nname = \"x\"\nmatch.seen = true\nactions = [{ move = \"Lists/GitHub\" }, \"mark_read\"]\n",
        );
        assert_eq!(
            preview_text(&filed, 3, false),
            "would move and mark read 3 cached messages to Lists/GitHub"
        );
        let flag = rule("[[rules]]\nname = \"x\"\nmatch.seen = true\nactions = [\"flag\"]\n");
        assert_eq!(
            preview_text(&flag, 2, true),
            "would flag 2 cached messages; only mail whose body is downloaded is checked"
        );
    }
}
