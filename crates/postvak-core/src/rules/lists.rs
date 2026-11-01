//! Named lists in `[lists]`, which conditions refer to with `{ list = "name" }`, and resolving those references.
use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Match, Matcher, OneOrMany, RulesError, TextMatch, check_name, domain_list, invalid};

/// The lists of a rules file by name.
pub type Lists = BTreeMap<String, NamedList>;

/// Domains for from_domain and to_domain, or a text condition (one of contains, equals, regex) for from, from_name, to,
/// cc, subject, body and header; a list cannot refer to another list
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    untagged,
    expecting = "a list of domains, or a table with one of contains, equals, regex"
)]
pub enum NamedList {
    Domains(Vec<String>),
    Text(TextMatch),
}

impl NamedList {
    /// The list in words: `a.example or b.example`, `contains "x" or "y"` or `matches regex "…"`.
    pub fn words(&self) -> String {
        let quoted = |values: &OneOrMany<String>| {
            let quoted: Vec<String> = values
                .as_slice()
                .iter()
                .map(|v| format!("\"{v}\""))
                .collect();
            quoted.join(" or ")
        };
        match self {
            NamedList::Domains(domains) => domains.join(" or "),
            NamedList::Text(TextMatch {
                contains: Some(values),
                ..
            }) => format!("contains {}", quoted(values)),
            NamedList::Text(TextMatch {
                equals: Some(values),
                ..
            }) => format!("equals {}", quoted(values)),
            NamedList::Text(t) => format!(
                "matches regex \"{}\"",
                t.regex.as_deref().unwrap_or_default()
            ),
        }
    }
}

/// A domain such as example.com, or { list = "name" } for every domain of that list
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged, expecting = "a domain, or { list = \"name\" }")]
pub enum Domain {
    Name(String),
    List(ListRef),
}

/// The domains of a list in [lists]
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ListRef {
    pub list: String,
}

/// From a table only: a derived struct also reads a sequence, which would take `["a.example"]` for a list's name.
impl<'de> Deserialize<'de> for ListRef {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut table = BTreeMap::<String, String>::deserialize(deserializer)?;
        match (table.remove("list"), table.is_empty()) {
            (Some(list), true) => Ok(ListRef { list }),
            _ => Err(serde::de::Error::custom("expected { list = \"name\" }")),
        }
    }
}

/// A new list, or a change to one, that an agent proposed; it changes nothing until a human approves it
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListProposal {
    /// The list it adds or changes
    pub name: String,
    /// Who proposed it; set by `rules propose`
    pub proposed_by: String,
    /// The list once approved
    pub value: NamedList,
}

/// Each proposal's list is well formed, and no list has two pending changes. Whether approving one would break a rule
/// is checked when it is proposed and when it is approved, so a stale proposal never stops the rules.
pub(crate) fn check_proposals(proposals: &[ListProposal]) -> Result<(), RulesError> {
    let mut names = BTreeSet::new();
    for proposal in proposals {
        if !names.insert(proposal.name.as_str()) {
            return Err(RulesError::List {
                list: proposal.name.clone(),
                reason: "has two pending changes in [[list_proposals]]".into(),
            });
        }
        check(&Lists::from([(
            proposal.name.clone(),
            proposal.value.clone(),
        )]))?;
    }
    Ok(())
}

/// `lists` as they read once `proposals` are approved.
pub fn with_proposals(lists: &Lists, proposals: &[ListProposal]) -> Lists {
    let mut lists = lists.clone();
    for proposal in proposals {
        lists.insert(proposal.name.clone(), proposal.value.clone());
    }
    lists
}

/// Every list `file` holds or has a pending change of, by name and sorted.
pub fn names(file: &super::RuleFile) -> BTreeSet<&str> {
    (file.lists.keys().map(String::as_str))
        .chain(file.list_proposals.iter().map(|p| p.name.as_str()))
        .collect()
}

/// The rules of `rules` that refer to the list `name`, at any depth.
pub fn users<'r>(rules: &'r [super::Rule], name: &str) -> Vec<&'r super::Rule> {
    (rules.iter())
        .filter(|rule| used_by([*rule]).iter().any(|used| used == name))
        .collect()
}

/// Every list is well formed on its own, whether a rule uses it or not.
pub(crate) fn check(lists: &Lists) -> Result<(), RulesError> {
    for (name, list) in lists {
        let refused = |reason: String| RulesError::List {
            list: name.clone(),
            reason,
        };
        check_name("name", name).map_err(refused)?;
        let path = format!("lists.{name}");
        let checked = match list {
            NamedList::Domains(domains) => domain_list(name, &path, domains).map(drop),
            NamedList::Text(t) if t.list.is_some() => {
                return Err(refused("a list cannot refer to another list".into()));
            }
            NamedList::Text(t) => {
                Matcher::build(name, &path, &t.contains, &t.equals, &t.regex).map(drop)
            }
        };
        checked.map_err(|e| match e {
            RulesError::Invalid { reason, .. } => refused(reason),
            other => other,
        })?;
    }
    Ok(())
}

/// `m` with every list reference replaced by the list's values: what the rule means, which the engine compiles and the
/// clock compares, so changing a list restarts the clock of each rule that uses it.
pub(crate) fn resolve(
    rule: &str,
    m: &Match,
    lists: &Lists,
    path: &str,
) -> Result<Match, RulesError> {
    let text = |field: &str, t: &TextMatch| -> Result<TextMatch, RulesError> {
        let Some(name) = &t.list else {
            return Ok(t.clone());
        };
        let path = format!("{path}.{field}");
        if t.contains.is_some() || t.equals.is_some() || t.regex.is_some() {
            return Err(invalid(
                rule,
                format!("{path}: list cannot be given with contains, equals or regex"),
            ));
        }
        match lookup(rule, &path, lists, name)? {
            NamedList::Text(listed) => Ok(listed.clone()),
            NamedList::Domains(_) => Err(invalid(
                rule,
                format!("{path}: list '{name}' holds domains; use it in from_domain or to_domain"),
            )),
        }
    };
    let optional =
        |field: &str, t: &Option<TextMatch>| t.as_ref().map(|t| text(field, t)).transpose();
    let domains = |field: &str, entries: &Option<OneOrMany<Domain>>| {
        let Some(entries) = entries else {
            return Ok(None);
        };
        if entries
            .as_slice()
            .iter()
            .all(|d| matches!(d, Domain::Name(_)))
        {
            return Ok(Some(entries.clone()));
        }
        let path = format!("{path}.{field}");
        let mut names = Vec::new();
        for entry in entries.as_slice() {
            match entry {
                Domain::Name(name) => names.push(Domain::Name(name.clone())),
                Domain::List(ListRef { list }) => match lookup(rule, &path, lists, list)? {
                    NamedList::Domains(listed) => {
                        names.extend(listed.iter().cloned().map(Domain::Name))
                    }
                    NamedList::Text(_) => {
                        return Err(invalid(
                            rule,
                            format!(
                                "{path}: list '{list}' is a text condition; use it in from, from_name, to, cc, subject, body or header"
                            ),
                        ));
                    }
                },
            }
        }
        Ok(Some(OneOrMany::Many(names)))
    };
    let header = m
        .header
        .as_ref()
        .map(|headers| {
            headers
                .iter()
                .map(|(name, t)| text(&format!("header.{name}"), t).map(|t| (name.clone(), t)))
                .collect::<Result<BTreeMap<_, _>, _>>()
        })
        .transpose()?;
    let branches = |key: &str, entries: &Option<Vec<Match>>| {
        entries
            .as_ref()
            .map(|entries| {
                entries
                    .iter()
                    .enumerate()
                    .map(|(i, entry)| resolve(rule, entry, lists, &format!("{path}.{key}[{i}]")))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()
    };
    Ok(Match {
        from: optional("from", &m.from)?,
        from_name: optional("from_name", &m.from_name)?,
        to: optional("to", &m.to)?,
        cc: optional("cc", &m.cc)?,
        subject: optional("subject", &m.subject)?,
        body: optional("body", &m.body)?,
        header,
        from_domain: domains("from_domain", &m.from_domain)?,
        to_domain: domains("to_domain", &m.to_domain)?,
        any: branches("any", &m.any)?,
        none: branches("none", &m.none)?,
        ..m.clone()
    })
}

fn lookup<'l>(
    rule: &str,
    path: &str,
    lists: &'l Lists,
    name: &str,
) -> Result<&'l NamedList, RulesError> {
    lists
        .get(name)
        .ok_or_else(|| invalid(rule, format!("{path}: no list named '{name}' in [lists]")))
}

/// The names of the lists any of `rules` refers to, at any depth and sorted.
pub fn used_by<'r>(rules: impl IntoIterator<Item = &'r super::Rule>) -> Vec<String> {
    let mut names = BTreeSet::new();
    for rule in rules {
        collect(&rule.matches, &mut names);
    }
    names.into_iter().collect()
}

fn collect(m: &Match, names: &mut BTreeSet<String>) {
    let texts = [&m.from, &m.from_name, &m.to, &m.cc, &m.subject, &m.body]
        .into_iter()
        .flatten()
        .chain(m.header.iter().flat_map(BTreeMap::values));
    names.extend(texts.filter_map(|t| t.list.clone()));
    let domains = [&m.from_domain, &m.to_domain]
        .into_iter()
        .flatten()
        .flat_map(OneOrMany::as_slice);
    names.extend(domains.filter_map(|d| match d {
        Domain::List(ListRef { list }) => Some(list.clone()),
        Domain::Name(_) => None,
    }));
    for entry in m.any.iter().chain(&m.none).flatten() {
        collect(entry, names);
    }
}

#[cfg(test)]
mod tests {
    use crate::rules::{compile, parse};

    const LISTS: &str = r#"
[lists]
kids = ["school.example", "daycare.example"]
security = { regex = "sign-?in alert|new device" }
"#;

    fn compiled(rules: &str) -> Result<Vec<crate::rules::CompiledRule>, String> {
        let file = parse(&format!("{LISTS}{rules}")).map_err(|e| e.to_string())?;
        compile(&file).map_err(|e| e.to_string())
    }

    fn rule(conditions: &str) -> String {
        format!("[[rules]]\nname = \"hold\"\n{conditions}\nactions = {{ flagged = true }}\n")
    }

    #[test]
    fn a_reference_compiles_to_the_lists_values_and_mixes_with_domains_in_place() {
        let rules = compiled(&rule(
            "match.any = [{ from_domain = [{ list = \"kids\" }, \"Extra.example\"] }, { subject = { list = \"security\" } }]",
        ))
        .unwrap();
        let any = &rules[0].conditions.any;
        assert_eq!(
            any[0].from_domain.as_deref(),
            Some(
                &[
                    "school.example".into(),
                    "daycare.example".into(),
                    "extra.example".into()
                ][..]
            )
        );
        assert!(
            any[1]
                .subject
                .as_ref()
                .unwrap()
                .is_match("New device signed in")
        );
    }

    #[test]
    fn a_header_condition_takes_a_text_list() {
        let rules = compiled(&rule("match.header.X-Alert = { list = \"security\" }")).unwrap();
        assert!(rules[0].conditions.headers[0].1.is_match("sign-in alert"));
    }

    #[test]
    fn a_bad_reference_is_refused_with_its_place() {
        for (conditions, wanted) in [
            (
                "match.from_domain = { list = \"kid\" }",
                "rule 'hold': match.from_domain: no list named 'kid' in [lists]",
            ),
            (
                "match.subject = { list = \"kids\" }",
                "rule 'hold': match.subject: list 'kids' holds domains; use it in from_domain or to_domain",
            ),
            (
                "match.none = [{ to_domain = { list = \"security\" } }]",
                "rule 'hold': match.none[0].to_domain: list 'security' is a text condition",
            ),
            (
                "match.subject = { list = \"security\", contains = \"x\" }",
                "rule 'hold': match.subject: list cannot be given with contains, equals or regex",
            ),
        ] {
            let error = compiled(&rule(conditions)).unwrap_err();
            assert!(error.starts_with(wanted), "{conditions}: {error}");
        }
    }

    #[test]
    fn a_bad_list_is_refused_even_when_no_rule_uses_it() {
        for (list, wanted) in [
            (
                "bad = [\"not a domain\"]",
                "list 'bad': lists.bad: 'not a domain' is not a domain",
            ),
            ("bad = []", "list 'bad': lists.bad must not be empty"),
            (
                "bad = { regex = \"(\" }",
                "list 'bad': lists.bad: invalid regex",
            ),
            (
                "bad = { list = \"kids\" }",
                "list 'bad': a list cannot refer to another list",
            ),
            (
                "bad = { contains = \"a\", regex = \"b\" }",
                "list 'bad': lists.bad needs exactly one of",
            ),
        ] {
            let error = compile(&parse(&format!("[lists]\n{list}\n")).unwrap())
                .unwrap_err()
                .to_string();
            assert!(error.starts_with(wanted), "{list}: {error}");
        }
    }

    #[test]
    fn changing_a_list_changes_the_definition_of_each_rule_that_uses_it_only() {
        let rules = "[[rules]]\nname = \"kids\"\nmatch.from_domain = { list = \"kids\" }\nactions = { flagged = true }\n\n\
                     [[rules]]\nname = \"plain\"\nmatch.from_domain = \"x.example\"\nactions = { flagged = true }\n";
        let definitions = |kids: &str| -> Vec<String> {
            let file = parse(&format!("[lists]\nkids = [{kids}]\n{rules}")).unwrap();
            compile(&file)
                .unwrap()
                .into_iter()
                .map(|r| r.definition)
                .collect()
        };
        let before = definitions("\"a.example\"");
        let after = definitions("\"a.example\", \"b.example\"");
        assert_ne!(before[0], after[0]);
        assert_eq!(before[1], after[1]);
    }

    #[test]
    fn a_rule_without_lists_keeps_the_definition_it_had() {
        let file = parse(&rule(
            "match.from_domain = [\"a.example\"]\nmatch.subject = { contains = \"x\" }",
        ))
        .unwrap();
        assert_eq!(
            compile(&file).unwrap()[0].definition,
            r#"[null,null,{"subject":{"contains":"x"},"from_domain":["a.example"]},{"flagged":true}]"#
        );
    }

    #[test]
    fn used_names_each_list_a_rule_refers_to() {
        let file = parse(&format!(
            "{LISTS}{}",
            rule("match.subject = { list = \"security\" }\nmatch.any = [{ from_domain = [{ list = \"kids\" }] }]")
        ))
        .unwrap();
        assert_eq!(super::used_by(&file.rules), ["kids", "security"]);
    }

    #[test]
    fn from_name_takes_a_text_list() {
        let file = parse(&format!(
            "{LISTS}{}",
            rule("match.from_name = { list = \"security\" }")
        ))
        .unwrap();
        assert_eq!(super::used_by(&file.rules), ["security"]);
        assert!(
            compile(&file).unwrap()[0]
                .definition
                .contains(r#""from_name":{"regex":"sign-?in alert|new device"}"#)
        );
    }

    #[test]
    fn a_rule_previewed_alone_reads_the_files_lists_only_when_it_uses_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rules.toml");
        let alone = |conditions: &str| parse(&rule(conditions)).unwrap().rules;
        std::fs::write(&path, LISTS).unwrap();
        assert!(
            crate::rules::compile_for(&path, &alone("match.from_domain = { list = \"kids\" }"))
                .is_ok()
        );
        std::fs::write(&path, "not toml [").unwrap();
        assert!(crate::rules::compile_for(&path, &alone("match.read = true")).is_ok());
        assert!(
            crate::rules::compile_for(&path, &alone("match.from_domain = { list = \"kids\" }"))
                .is_err()
        );
    }

    #[test]
    fn lists_round_trip_through_toml() {
        let file = parse(&format!(
            "{LISTS}{}",
            rule("match.from_domain = { list = \"kids\" }")
        ))
        .unwrap();
        let again = parse(&toml::to_string(&file).unwrap()).unwrap();
        assert_eq!(again.lists, file.lists);
        assert_eq!(again.rules[0].matches, file.rules[0].matches);
    }
}
