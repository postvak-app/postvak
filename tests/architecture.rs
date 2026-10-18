//! The module ownership rules from AGENTS.md's module map that crate boundaries cannot express, checked on the source
//! text.
//!
//! Comments, string contents and `#[cfg(test)]` items are ignored: the rules are about shipped code. Each check is
//! a plain text scan, so it only enforces what can be checked without false positives; allowed exceptions are listed
//! below with their reason.

// Test code: unwrap, expect and panic are how a test fails.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

/// Paths into `rules` that `postvak-mcp` outside `backend.rs` may name: the data types its tool arguments deserialize
/// into and describe in their schemas. Loading, evaluating or editing rules stays in `backend.rs`.
const MCP_RULES_TYPES: [&str; 2] = ["Rule", "Action"];

/// Functions outside `rules/edit.rs` that may write next to the rules path, as `(file, fn, reason)`.
const RULES_WRITE_EXCEPTIONS: [(&str, &str, &str); 1] = [(
    "crates/postvak-gui/src/state.rs",
    "open_rules_file",
    "creates an empty rules.toml when there is none, so the editor has a file to open; never changes an existing one",
)];

/// A source file with comments and `#[cfg(test)]` items blanked; `code` also blanks string contents.
/// Both keep every character's line, so positions map back to the file.
struct Source {
    path: String,
    text: Vec<char>,
    code: Vec<char>,
}

impl Source {
    fn parse(path: &str, raw: &str) -> Source {
        let (mut text, mut code) = strip(raw);
        blank_cfg_test(&mut text, &mut code);
        Source {
            path: path.to_string(),
            text,
            code,
        }
    }

    fn line(&self, pos: usize) -> usize {
        1 + self.code[..pos].iter().filter(|&&c| c == '\n').count()
    }

    fn violation(&self, pos: usize, rule: &str, detail: &str) -> String {
        format!(
            "{}:{}: {detail}\n  rule: {rule} (AGENTS.md, module map)",
            self.path,
            self.line(pos)
        )
    }
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Blanks comments (both copies) and string and char literal contents (the `code` copy only), keeping newlines.
fn strip(raw: &str) -> (Vec<char>, Vec<char>) {
    let src: Vec<char> = raw.chars().collect();
    let mut text = src.clone();
    let mut code = src.clone();
    let blank = |v: &mut Vec<char>, from: usize, to: usize| {
        for c in &mut v[from..to] {
            if *c != '\n' {
                *c = ' ';
            }
        }
    };
    let n = src.len();
    let mut i = 0;
    while i < n {
        let c = src[i];
        let next = src.get(i + 1).copied();
        let prev_ident = i > 0 && is_ident(src[i - 1]);
        if c == '/' && next == Some('/') {
            let end = src[i..]
                .iter()
                .position(|&c| c == '\n')
                .map_or(n, |p| i + p);
            blank(&mut text, i, end);
            blank(&mut code, i, end);
            i = end;
        } else if c == '/' && next == Some('*') {
            let (mut depth, mut j) = (1, i + 2);
            while j < n && depth > 0 {
                if src[j] == '/' && src.get(j + 1) == Some(&'*') {
                    depth += 1;
                    j += 2;
                } else if src[j] == '*' && src.get(j + 1) == Some(&'/') {
                    depth -= 1;
                    j += 2;
                } else {
                    j += 1;
                }
            }
            blank(&mut text, i, j);
            blank(&mut code, i, j);
            i = j;
        } else if c == 'r'
            && (!prev_ident || (src[i - 1] == 'b' && (i < 2 || !is_ident(src[i - 2]))))
            && matches!(next, Some('"' | '#'))
        {
            let hashes = src[i + 1..].iter().take_while(|&&c| c == '#').count();
            let open = i + 1 + hashes;
            if src.get(open) != Some(&'"') {
                i += 1;
                continue;
            }
            let mut j = open + 1;
            while j < n
                && !(src[j] == '"'
                    && src[j + 1..]
                        .iter()
                        .take(hashes)
                        .filter(|&&c| c == '#')
                        .count()
                        == hashes)
            {
                j += 1;
            }
            blank(&mut code, open + 1, j.min(n));
            i = j + 1 + hashes;
        } else if c == '"' {
            let mut j = i + 1;
            while j < n && src[j] != '"' {
                j += if src[j] == '\\' { 2 } else { 1 };
            }
            blank(&mut code, i + 1, j.min(n));
            i = j + 1;
        } else if c == '\'' && next == Some('\\') {
            let end = src[i + 2..]
                .iter()
                .position(|&c| c == '\'')
                .map_or(n, |p| i + 2 + p);
            blank(&mut code, i + 1, end);
            i = end + 1;
        } else if c == '\'' && src.get(i + 2) == Some(&'\'') {
            blank(&mut code, i + 1, i + 2);
            i += 3;
        } else {
            i += 1;
        }
    }
    (text, code)
}

/// The position just past the `}` matching the `{` at `open`.
fn matching_brace(code: &[char], open: usize) -> usize {
    let mut depth = 0;
    for (j, &c) in code.iter().enumerate().skip(open) {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return j + 1;
                }
            }
            _ => {}
        }
    }
    code.len()
}

fn find_all(hay: &[char], needle: &str) -> Vec<usize> {
    let needle: Vec<char> = needle.chars().collect();
    (0..hay.len().saturating_sub(needle.len() - 1))
        .filter(|&i| hay[i..i + needle.len()] == needle[..])
        .collect()
}

/// Like `find_all`, but only where `needle` is not glued to a longer identifier on either side.
fn find_tokens(hay: &[char], needle: &str) -> Vec<usize> {
    let len = needle.chars().count();
    let first_ident = needle.chars().next().is_some_and(is_ident);
    let last_ident = needle.chars().last().is_some_and(is_ident);
    find_all(hay, needle)
        .into_iter()
        .filter(|&i| !(first_ident && i > 0 && is_ident(hay[i - 1])))
        .filter(|&i| !(last_ident && hay.get(i + len).is_some_and(|&c| is_ident(c))))
        .collect()
}

/// Blanks every item marked `#[cfg(test)]`: a `mod tests { … }`, a `fn`, a `use …;`, or a `mod x;` declaration.
fn blank_cfg_test(text: &mut [char], code: &mut [char]) {
    for at in find_all(code, "#[cfg(test)]") {
        let start = at + "#[cfg(test)]".len();
        let end = code[start..]
            .iter()
            .position(|&c| c == ';' || c == '{')
            .map_or(code.len(), |p| start + p);
        let end = if code.get(end) == Some(&'{') {
            matching_brace(code, end)
        } else {
            end + 1
        };
        let end = end.min(code.len());
        for v in [&mut *text, &mut *code] {
            for c in &mut v[at..end] {
                if *c != '\n' {
                    *c = ' ';
                }
            }
        }
    }
}

/// The files that `#[cfg(test)] mod x;` declarations in `source` (at `path`) pull in.
fn test_only_modules(path: &str, raw: &str) -> Vec<String> {
    let (_, code) = strip(raw);
    let code: String = code.into_iter().collect();
    let dir = match path.strip_suffix(".rs") {
        Some(p) if p.ends_with("/mod") || p.ends_with("/lib") || p.ends_with("/main") => {
            p.rsplit_once('/').unwrap().0.to_string()
        }
        Some(p) => p.to_string(),
        None => return Vec::new(),
    };
    let mut out = Vec::new();
    let mut rest = code.as_str();
    while let Some(at) = rest.find("#[cfg(test)]") {
        rest = &rest[at + "#[cfg(test)]".len()..];
        let item = rest.split([';', '{']).next().unwrap_or("");
        let words: Vec<&str> = item.split_whitespace().collect();
        if let [.., "mod", name] = words.as_slice()
            && rest[item.len()..].starts_with(';')
        {
            out.push(format!("{dir}/{name}.rs"));
            out.push(format!("{dir}/{name}/mod.rs"));
        }
    }
    out
}

/// Every `postvak_core` path named in `code`, relative to its root, with `use` groups expanded:
/// `postvak_core::rules::{self, Rule}` gives `rules` and `rules::Rule`.
fn core_paths(code: &[char]) -> Vec<(usize, String)> {
    const ROOT: &str = "postvak_core::";
    let mut out = Vec::new();
    for at in find_tokens(code, ROOT) {
        let mut paths = Vec::new();
        use_tree(code, at + ROOT.len(), String::new(), &mut paths);
        out.extend(paths.into_iter().map(|p| (at, p)));
    }
    out
}

fn skip_ws(code: &[char], mut i: usize) -> usize {
    while code.get(i).is_some_and(|c| c.is_whitespace()) {
        i += 1;
    }
    i
}

/// Parses one use tree at `i` under `prefix`, pushing each full path; returns where it ended.
fn use_tree(code: &[char], i: usize, prefix: String, out: &mut Vec<String>) -> usize {
    let mut i = skip_ws(code, i);
    if code.get(i) == Some(&'{') {
        i += 1;
        loop {
            i = skip_ws(code, i);
            match code.get(i) {
                Some('}') | None => return i + 1,
                Some(',') => i += 1,
                _ => i = use_tree(code, i, prefix.clone(), out),
            }
        }
    }
    if code.get(i) == Some(&'*') {
        out.push(format!("{prefix}*"));
        return i + 1;
    }
    let start = i;
    while code.get(i).is_some_and(|&c| is_ident(c)) {
        i += 1;
    }
    let segment: String = code[start..i].iter().collect();
    if segment.is_empty() {
        if !prefix.is_empty() {
            out.push(prefix.trim_end_matches("::").to_string());
        }
        return i;
    }
    let path = if segment == "self" {
        prefix.trim_end_matches("::").to_string()
    } else {
        format!("{prefix}{segment}")
    };
    let after = skip_ws(code, i);
    if code.get(after) == Some(&':') && code.get(after + 1) == Some(&':') {
        return use_tree(code, after + 2, format!("{path}::"), out);
    }
    out.push(path);
    let alias = skip_ws(code, i);
    if code[alias..].starts_with(&['a', 's'])
        && code.get(alias + 2).is_some_and(|c| c.is_whitespace())
    {
        i = skip_ws(code, alias + 2);
        while code.get(i).is_some_and(|&c| is_ident(c)) {
            i += 1;
        }
    }
    i
}

/// Rule 1: in `postvak-mcp`, only `backend.rs` touches the store, rules or the daemon.
fn check_mcp(source: &Source) -> Vec<String> {
    const RULE: &str = "in postvak-mcp, only backend.rs touches the store, rules or the daemon";
    let in_mcp = source.path.starts_with("crates/postvak-mcp/src/");
    if !in_mcp || source.path == "crates/postvak-mcp/src/backend.rs" {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (at, path) in core_paths(&source.code) {
        let segments: Vec<&str> = path.split("::").collect();
        let allowed = match segments.as_slice() {
            ["store" | "daemon", ..] => false,
            ["rules", item, ..] => MCP_RULES_TYPES.contains(item),
            ["rules"] => false,
            _ => true,
        };
        if !allowed {
            out.push(source.violation(at, RULE, &format!("names `{path}`; go through `Backend`")));
        }
    }
    out
}

/// Every `fn` in `source` with its name, where the keyword starts, and its body's range.
fn functions(code: &[char]) -> Vec<(String, usize, usize, usize)> {
    let mut out = Vec::new();
    for at in find_tokens(code, "fn") {
        let name_start = skip_ws(code, at + 2);
        let name_end = (name_start..code.len())
            .find(|&j| !is_ident(code[j]))
            .unwrap_or(code.len());
        if name_end == name_start {
            continue;
        }
        let Some(open) = code[name_end..].iter().position(|&c| c == '{' || c == ';') else {
            continue;
        };
        let open = name_end + open;
        if code[open] == '{' {
            let name = code[name_start..name_end].iter().collect();
            out.push((name, at, open, matching_brace(code, open)));
        }
    }
    out
}

/// Rule 2: only `rules/edit.rs` in `postvak-core` writes rules.toml. Flags any function outside it that both names the rules path
/// and calls a file write.
fn check_rules_writes(source: &Source) -> Vec<String> {
    const RULE: &str = "only postvak-core's rules/edit.rs writes rules.toml";
    const PATH_MARKERS: [&str; 3] = ["rules_file", "rules_path", "\"rules.toml\""];
    const WRITES: [&str; 7] = [
        "write_atomic",
        "write_atomic_keeping_dir_mode",
        "fs::write",
        "File::create",
        "OpenOptions",
        "fs::rename",
        "fs::remove_file",
    ];
    if source.path == "crates/postvak-core/src/rules/edit.rs" {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (name, at, open, close) in functions(&source.code) {
        let excepted = RULES_WRITE_EXCEPTIONS
            .iter()
            .any(|(file, f, _)| *file == source.path && *f == name);
        let names_path = PATH_MARKERS
            .iter()
            .any(|m| !find_tokens(&source.text[open..close], m).is_empty());
        let write = WRITES.iter().find_map(|w| {
            find_tokens(&source.code[open..close], w)
                .first()
                .map(|p| (w, open + p))
        });
        if let (false, true, Some((w, pos))) = (excepted, names_path, write) {
            out.push(source.violation(
                pos,
                RULE,
                &format!(
                    "`fn {name}` (line {}) names the rules path and calls `{w}`; use `rules::edit`",
                    source.line(at)
                ),
            ));
        }
    }
    out
}

/// Rule 3: in `postvak-gui`, `state.rs` is the only code that changes state; the view modules draw from `&App` and
/// return `UiAction`s. So a view never takes `&mut App`, reaches the daemon, or edits rules.
fn check_gui_views(source: &Source) -> Vec<String> {
    const RULE: &str = "in postvak-gui, state.rs is the only code that changes state; views draw and return UiActions";
    let Some(file) = source.path.strip_prefix("crates/postvak-gui/src/") else {
        return Vec::new();
    };
    if matches!(file, "lib.rs" | "state.rs" | "test_support.rs") {
        return Vec::new();
    }
    let mut out = Vec::new();
    for at in find_tokens(&source.code, "&mut") {
        let next = skip_ws(&source.code, at + 4);
        if find_tokens(&source.code[next..(next + 3).min(source.code.len())], "App").first()
            == Some(&0)
        {
            out.push(source.violation(at, RULE, "takes `&mut App`; return a `UiAction` instead"));
        }
    }
    for at in find_tokens(&source.code, ".client") {
        out.push(source.violation(
            at,
            RULE,
            "uses the daemon client; return a `UiAction` instead",
        ));
    }
    for (at, path) in core_paths(&source.code) {
        if path.starts_with("daemon") || path.starts_with("rules::edit") {
            out.push(source.violation(
                at,
                RULE,
                &format!("names `{path}`; return a `UiAction` instead"),
            ));
        }
    }
    for at in find_tokens(&source.code, "rules::edit") {
        out.push(source.violation(at, RULE, "edits rules; return a `UiAction` instead"));
    }
    // `postvak_core::rules::edit` matches both scans; one report per line is enough.
    out.sort();
    out.dedup_by(|a, b| a.split(": ").next() == b.split(": ").next());
    out
}

fn check(source: &Source) -> Vec<String> {
    let mut out = check_mcp(source);
    out.extend(check_rules_writes(source));
    out.extend(check_gui_views(source));
    out
}

fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn module_ownership_rules_from_agents_md_hold() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    for krate in std::fs::read_dir(root.join("crates")).unwrap() {
        let src = krate.unwrap().path().join("src");
        if src.is_dir() {
            rust_files(&src, &mut files);
        }
    }
    files.sort();
    let sources: Vec<(String, String)> = files
        .iter()
        .map(|f| {
            let rel = f
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            (rel, std::fs::read_to_string(f).unwrap())
        })
        .collect();
    let test_only: Vec<String> = sources
        .iter()
        .flat_map(|(p, raw)| test_only_modules(p, raw))
        .collect();
    assert!(test_only.contains(&"crates/postvak-mcp/src/tests.rs".to_string()));
    let mut violations = Vec::new();
    for (path, raw) in &sources {
        if !test_only.contains(path) {
            violations.extend(check(&Source::parse(path, raw)));
        }
    }
    assert!(
        violations.is_empty(),
        "module ownership rules broken:\n{}",
        violations.join("\n")
    );
}

#[test]
fn every_rules_write_exception_still_exists() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for (file, name, _reason) in RULES_WRITE_EXCEPTIONS {
        let source = Source::parse(file, &std::fs::read_to_string(root.join(file)).unwrap());
        assert!(
            functions(&source.code).iter().any(|(n, ..)| n == name),
            "{file} has no `fn {name}`; drop it from RULES_WRITE_EXCEPTIONS"
        );
    }
}

/// Packages no crate below the front ends may reach, by name prefix: the GUI toolkit, the HTML engine and the MCP SDK.
const FRONT_END_ONLY: [&str; 4] = ["slint", "i-slint-", "blitz-", "rmcp"];

/// What `cargo metadata` says for the host: packages by id, and each package's dependencies as `(id, dev_only)`.
struct Graph {
    names: std::collections::HashMap<String, String>,
    deps: std::collections::HashMap<String, Vec<(String, bool)>>,
}

impl Graph {
    fn from_metadata(metadata: &serde_json::Value) -> Graph {
        let names = metadata["packages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                (
                    p["id"].as_str().unwrap().to_string(),
                    p["name"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        let deps = metadata["resolve"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|node| {
                let deps = node["deps"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|dep| {
                        let kinds = dep["dep_kinds"].as_array().unwrap();
                        let dev_only = kinds.iter().all(|k| k["kind"] == "dev");
                        (dep["pkg"].as_str().unwrap().to_string(), dev_only)
                    })
                    .collect();
                (node["id"].as_str().unwrap().to_string(), deps)
            })
            .collect();
        Graph { names, deps }
    }

    fn id(&self, name: &str) -> &str {
        self.names
            .iter()
            .find(|(_, n)| *n == name)
            .map(|(id, _)| id.as_str())
            .unwrap()
    }

    fn name(&self, id: &str) -> &str {
        &self.names[id]
    }

    /// Normal and build dependencies of `id`, `(id, dev_only)` pairs filtered to the shipped ones.
    fn shipped(&self, id: &str) -> impl Iterator<Item = &str> {
        self.deps
            .get(id)
            .into_iter()
            .flatten()
            .filter(|(_, dev)| !dev)
            .map(|(d, _)| d.as_str())
    }
}

/// The dependency rules from AGENTS.md's module map: nothing below the front ends reaches Slint, Blitz or rmcp, and
/// only `postvak-core` depends on rusqlite (the front ends only in tests).
fn check_graph(graph: &Graph, workspace: &[&str]) -> Vec<String> {
    let mut violations = Vec::new();
    // Breadth-first from postvak-core, remembering who pulled each package in so a violation names its path.
    let core = graph.id("postvak-core");
    let mut via: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    let mut queue = std::collections::VecDeque::from([core]);
    while let Some(id) = queue.pop_front() {
        for dep in graph.shipped(id) {
            if dep != core && !via.contains_key(dep) {
                via.insert(dep, id);
                queue.push_back(dep);
            }
        }
    }
    let mut reached: Vec<&str> = via.keys().copied().collect();
    reached.sort_by_key(|id| graph.name(id));
    for id in reached {
        if FRONT_END_ONLY.iter().any(|p| graph.name(id).starts_with(p)) {
            let mut path = vec![graph.name(id)];
            let mut at = id;
            while let Some(&from) = via.get(at) {
                path.push(graph.name(from));
                at = from;
            }
            path.reverse();
            violations.push(format!(
                "postvak-core depends on {} ({}); AGENTS.md: nothing below the front ends depends on Slint, Blitz or rmcp",
                graph.name(id),
                path.join(" -> ")
            ));
        }
    }
    for krate in workspace.iter().filter(|k| **k != "postvak-core") {
        for dep in graph.shipped(graph.id(krate)) {
            if graph.name(dep).starts_with("rusqlite") {
                violations.push(format!(
                    "{krate} depends on {}; AGENTS.md: only postvak-core depends on rusqlite (the front ends only in tests)",
                    graph.name(dep)
                ));
            }
        }
    }
    violations
}

#[test]
fn dependency_rules_from_agents_md_hold() {
    // The host only: other platforms' crates may not be downloaded, and CI runs this on each platform it tests.
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let version = std::process::Command::new(rustc)
        .arg("-vV")
        .output()
        .unwrap();
    let version = String::from_utf8(version.stdout).unwrap();
    let host = version
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .unwrap();
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let output = std::process::Command::new(cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--filter-platform",
            host,
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "cargo metadata: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let graph = Graph::from_metadata(&metadata);
    let violations = check_graph(
        &graph,
        &["postvak", "postvak-core", "postvak-gui", "postvak-mcp"],
    );
    assert!(
        violations.is_empty(),
        "dependency rules broken:\n{}",
        violations.join("\n")
    );
}

#[cfg(test)]
mod checks_fire {
    use super::*;

    fn violations(path: &str, src: &str) -> Vec<String> {
        check(&Source::parse(path, src))
    }

    #[test]
    fn mcp_outside_backend_may_name_rule_types_only() {
        let ok = "use postvak_core::rules::{Action, Rule};\nfn f(r: postvak_core::rules::Rule) { let _ = Action::Archive; }\n\
                  // postvak_core::store::Store in a comment\nconst S: &str = \"postvak_core::daemon::Client\";\n\
                  #[cfg(test)]\nmod tests { use postvak_core::store::Store; }\n";
        assert_eq!(
            violations("crates/postvak-mcp/src/tools.rs", ok),
            Vec::<String>::new()
        );
        for bad in [
            "use postvak_core::store::Store;",
            "use postvak_core::{config, daemon::Client};",
            "use postvak_core::rules::{self, Rule};",
            "fn f() { postvak_core::rules::load(p); }",
            "use postvak_core::rules::edit;",
            "use postvak_core::{rules::{self as r}, config};",
        ] {
            let found = violations("crates/postvak-mcp/src/tools.rs", &format!("\n{bad}\n"));
            assert_eq!(found.len(), 1, "{bad}: {found:?}");
            assert!(
                found[0].starts_with("crates/postvak-mcp/src/tools.rs:2: "),
                "{found:?}"
            );
            assert!(found[0].contains("AGENTS.md"));
        }
        assert!(
            violations(
                "crates/postvak-mcp/src/backend.rs",
                "use postvak_core::store::Store;"
            )
            .is_empty()
        );
    }

    #[test]
    fn rules_toml_writes_outside_edit_rs_are_flagged() {
        let bad = "fn save(paths: &Paths) {\n    write_atomic(&paths.rules_file(), b\"x\").unwrap();\n}\n";
        let found = violations("src/cli/mod.rs", bad);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].starts_with("src/cli/mod.rs:2: `fn save`"),
            "{found:?}"
        );
        let joined =
            "fn f(dir: &Path) { std::fs::write(dir.join(\"rules.toml\"), \"\").unwrap(); }";
        assert_eq!(
            violations("crates/postvak-core/src/sync.rs", joined).len(),
            1
        );
        let unrelated = "fn f(p: &Paths) { write_atomic(&p.config_file(), b\"\"); }\n\
                         fn g(p: &Paths) { let _ = rules::load(&p.rules_file()); }";
        assert!(violations("crates/postvak-core/src/sync.rs", unrelated).is_empty());
        assert!(violations("crates/postvak-core/src/rules/edit.rs", bad).is_empty());
        let excepted =
            "fn open_rules_file(&mut self) { write_atomic(&self.paths.rules_file(), b\"\"); }";
        assert!(violations("crates/postvak-gui/src/state.rs", excepted).is_empty());
    }

    #[test]
    fn gui_views_never_change_state() {
        let ok = "use super::state::{App, UiAction};\npub(crate) fn show(app: &App) -> Vec<UiAction> { vec![] }\n\
                  #[cfg(test)]\nmod tests { fn at(h: &mut App) {} }";
        assert_eq!(
            violations("crates/postvak-gui/src/list.rs", ok),
            Vec::<String>::new()
        );
        for bad in [
            "fn show(app: &mut App) {}",
            "fn show(app: &App) { app.client.send(c); }",
            "use postvak_core::daemon::Client;",
            "fn f(p: &Path) { postvak_core::rules::edit::approve(p, \"x\"); }",
            "fn f(p: &Path) { rules::edit::approve(p, \"x\"); }",
        ] {
            let found = violations("crates/postvak-gui/src/rules.rs", bad);
            assert_eq!(found.len(), 1, "{bad}: {found:?}");
        }
        assert!(
            violations(
                "crates/postvak-gui/src/state.rs",
                "fn f(app: &mut App) { self.client.send(c); }"
            )
            .is_empty()
        );
    }

    /// Metadata with `postvak-core -> helper -> slint` and `postvak-mcp -> rusqlite`, both forbidden, beside the allowed
    /// `postvak-gui -> slint` and `postvak-gui -dev-> rusqlite`.
    #[test]
    fn dependency_rules_flag_a_front_end_crate_below_and_rusqlite_beside_core() {
        let package = |name: &str| serde_json::json!({"id": format!("{name}-id"), "name": name});
        let dep = |name: &str, kind: Option<&str>| serde_json::json!({"pkg": format!("{name}-id"), "dep_kinds": [{"kind": kind}]});
        let node = |name: &str, deps: Vec<serde_json::Value>| serde_json::json!({"id": format!("{name}-id"), "deps": deps});
        let metadata = |core_deps: Vec<serde_json::Value>, mcp_deps: Vec<serde_json::Value>| {
            serde_json::json!({
                "packages": (["postvak-core", "postvak-gui", "postvak-mcp", "helper", "slint", "rusqlite"].map(package)),
                "resolve": {"nodes": [
                    node("postvak-core", core_deps),
                    node("postvak-gui", vec![dep("postvak-core", None), dep("slint", None), dep("rusqlite", Some("dev"))]),
                    node("postvak-mcp", mcp_deps),
                    node("helper", vec![dep("slint", Some("build"))]),
                    node("slint", vec![]),
                    node("rusqlite", vec![]),
                ]},
            })
        };
        let workspace = ["postvak-core", "postvak-gui", "postvak-mcp"];
        let ok = metadata(
            vec![dep("rusqlite", None)],
            vec![dep("postvak-core", None), dep("rusqlite", Some("dev"))],
        );
        assert_eq!(
            check_graph(&Graph::from_metadata(&ok), &workspace),
            Vec::<String>::new()
        );
        let bad = metadata(vec![dep("helper", None)], vec![dep("rusqlite", None)]);
        let found = check_graph(&Graph::from_metadata(&bad), &workspace);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(
            found[0].contains("(postvak-core -> helper -> slint)"),
            "{found:?}"
        );
        assert!(
            found[1].starts_with("postvak-mcp depends on rusqlite"),
            "{found:?}"
        );
    }

    #[test]
    fn strip_keeps_lines_and_blanks_literals() {
        let src = "let a = r#\"crate::store \"# ; // crate::daemon\nlet c = '\"'; let l: &'a str = \"x\";\n/* a\n b */ crate::rules";
        let s = Source::parse("x.rs", src);
        let code: String = s.code.iter().collect();
        assert!(!code.contains("store") && !code.contains("daemon"));
        assert!(code.contains("&'a str"));
        assert_eq!(s.line(code.find("crate::rules").unwrap()), 4);
    }

    #[test]
    fn cfg_test_module_files_are_found() {
        let found = test_only_modules(
            "crates/postvak-mcp/src/lib.rs",
            "mod backend;\n#[cfg(test)]\nmod tests;\nmod tools;",
        );
        assert_eq!(
            found,
            [
                "crates/postvak-mcp/src/tests.rs",
                "crates/postvak-mcp/src/tests/mod.rs"
            ]
        );
        let found = test_only_modules(
            "crates/postvak-core/src/daemon/mod.rs",
            "#[cfg(test)]\npub(crate) mod test_support;",
        );
        assert_eq!(found[0], "crates/postvak-core/src/daemon/test_support.rs");
    }
}
