//! `postvak mcp install`: registers the server with an agent host, through its CLI or its config file, or prints a snippet for other hosts.
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};

use super::parse_scopes;
use postvak_core::paths::{flatpak_id, stable_exe, write_atomic_keeping_dir_mode};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Target {
    ClaudeCode,
    ClaudeDesktop,
    Codex,
    Copilot,
    Cursor,
    Gemini,
    Json,
    Opencode,
}

/// The command a host runs. The binary path is absolute: hosts started from the Dock do not inherit the shell's PATH.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub command: PathBuf,
    pub args: Vec<String>,
}

impl Entry {
    pub fn new(exe: &Path, scopes: &str, accounts: &[String], save_dir: Option<&Path>) -> Entry {
        let mut args = vec![
            "mcp".to_string(),
            "--scopes".to_string(),
            scopes.to_string(),
        ];
        for account in accounts {
            args.push("--account".to_string());
            args.push(account.clone());
        }
        if let Some(dir) = save_dir {
            args.push("--save-dir".to_string());
            args.push(dir.display().to_string());
        }
        Entry {
            command: exe.to_path_buf(),
            args,
        }
    }

    /// The same command through `flatpak run`: a host outside the sandbox cannot see the binary inside it.
    pub fn through_flatpak(self, app_id: &str) -> Entry {
        let mut args = vec!["run".to_string(), app_id.to_string()];
        args.extend(self.args);
        Entry {
            command: PathBuf::from("flatpak"),
            args,
        }
    }

    fn server(&self) -> Value {
        json!({ "command": self.command, "args": self.args })
    }

    fn argv(&self) -> Vec<String> {
        std::iter::once(self.command.display().to_string())
            .chain(self.args.iter().cloned())
            .collect()
    }
}

fn shell_words(words: &[String]) -> String {
    let quote = |w: &String| {
        if !w.is_empty()
            && w.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_./:,=@".contains(c))
        {
            w.clone()
        } else {
            format!("'{}'", w.replace('\'', r"'\''"))
        }
    };
    words.iter().map(quote).collect::<Vec<_>>().join(" ")
}

pub fn json_snippet(entry: &Entry) -> String {
    let snippet = json!({ "mcpServers": { "postvak": entry.server() } });
    // `{:#}` pretty-prints a JSON value like `to_string_pretty`, without a serializer that could fail.
    format!("{snippet:#}\n")
}

/// A host whose config Postvak edits itself: its name, the file, and the key that holds its servers.
struct ConfigFile {
    app: &'static str,
    path: PathBuf,
    section: &'static str,
}

impl ConfigFile {
    fn of(target: Target) -> Result<Option<ConfigFile>> {
        let dirs = directories::BaseDirs::new().context("no home directory found")?;
        let (app, path, section) = match target {
            // `~/Library/Application Support/Claude/` on macOS, `~/.config/Claude/` on Linux.
            Target::ClaudeDesktop => (
                "Claude Desktop",
                dirs.config_dir().join("Claude/claude_desktop_config.json"),
                "mcpServers",
            ),
            Target::Cursor => (
                "Cursor",
                dirs.home_dir().join(".cursor/mcp.json"),
                "mcpServers",
            ),
            // opencode reads `$XDG_CONFIG_HOME` or `~/.config` on macOS too, where `config_dir` is elsewhere.
            Target::Opencode => (
                "opencode",
                std::env::var_os("XDG_CONFIG_HOME")
                    .map(PathBuf::from)
                    .filter(|dir| dir.is_absolute())
                    .unwrap_or_else(|| dirs.home_dir().join(".config"))
                    .join("opencode/opencode.json"),
                "mcp",
            ),
            Target::ClaudeCode
            | Target::Codex
            | Target::Copilot
            | Target::Gemini
            | Target::Json => {
                return Ok(None);
            }
        };
        Ok(Some(ConfigFile { app, path, section }))
    }

    /// The server's entry under `section`: opencode takes the whole command line as one array.
    fn server(&self, entry: &Entry) -> Value {
        match self.section {
            "mcp" => json!({ "type": "local", "command": entry.argv(), "enabled": true }),
            _ => entry.server(),
        }
    }

    fn snippet(&self, entry: &Entry) -> String {
        format!(
            "{:#}\n",
            json!({ self.section: { "postvak": self.server(entry) } })
        )
    }
}

/// Sets `<section>.postvak` to `server`, or removes it when `server` is None, keeping every other key.
/// Returns the new file text, or None when there is nothing to change. The old file is copied to `.bak`
/// unless it already is Postvak's own output, so the user's original survives re-runs. A file that is
/// not a JSON object is refused untouched; `dry_run` writes nothing.
pub fn edit_config(
    path: &Path,
    section: &str,
    server: Option<Value>,
    dry_run: bool,
) -> Result<Option<String>> {
    let old = match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    if old.is_none() && server.is_none() {
        return Ok(None);
    }
    let mut config: Value = match old.as_deref() {
        Some(text) if !text.trim().is_empty() => serde_json::from_str(text).with_context(|| {
            format!(
                "{} is not valid JSON; fix it or move it away first",
                path.display()
            )
        })?,
        _ => json!({}),
    };
    let own_output = serde_json::to_string_pretty(&config)? + "\n";
    let Some(root) = config.as_object_mut() else {
        bail!("{} is not a JSON object", path.display());
    };
    match server {
        Some(server) => {
            let Some(servers) = root
                .entry(section)
                .or_insert_with(|| json!({}))
                .as_object_mut()
            else {
                bail!("{section} in {} is not a JSON object", path.display());
            };
            servers.insert("postvak".into(), server);
        }
        None => match root.get_mut(section) {
            Some(servers) => {
                let Some(servers) = servers.as_object_mut() else {
                    bail!("{section} in {} is not a JSON object", path.display());
                };
                if servers.remove("postvak").is_none() {
                    return Ok(None);
                }
            }
            None => return Ok(None),
        },
    }
    let text = serde_json::to_string_pretty(&config)? + "\n";
    if !dry_run {
        if old.as_deref().is_some_and(|old| old != own_output) {
            std::fs::copy(path, path.with_extension("json.bak"))?;
        }
        write_atomic_keeping_dir_mode(path, text.as_bytes())?;
    }
    Ok(Some(text))
}

/// The program that registers servers and the host's name, for hosts that have one.
fn host_cli(target: Target) -> Option<(&'static str, &'static str)> {
    match target {
        Target::ClaudeCode => Some(("claude", "Claude Code")),
        Target::Codex => Some(("codex", "Codex")),
        Target::Copilot => Some(("copilot", "Copilot CLI")),
        Target::Gemini => Some(("gemini", "Gemini CLI")),
        Target::ClaudeDesktop | Target::Cursor | Target::Json | Target::Opencode => None,
    }
}

/// The host's own invocations at user scope; an add follows a remove so a re-run replaces the old entry.
pub fn cli_commands(program: &str, entry: Option<&Entry>) -> Vec<Vec<String>> {
    // Codex and Copilot keep servers per user only; Claude Code and Gemini default to the project.
    let scope: &[&str] = match program {
        "claude" | "gemini" => &["--scope", "user"],
        _ => &[],
    };
    let base = |verb: &str| {
        let mut words = vec![program, "mcp", verb];
        words.extend(scope);
        words.push("postvak");
        words.into_iter().map(String::from).collect::<Vec<_>>()
    };
    let mut commands = vec![base("remove")];
    if let Some(entry) = entry {
        let mut add = base("add");
        // Gemini takes the command as a positional and only the arguments after `--`.
        if program == "gemini" {
            add.push(entry.command.display().to_string());
            add.push("--".into());
            add.extend(entry.args.iter().cloned());
        } else {
            add.push("--".into());
            add.extend(entry.argv());
        }
        commands.push(add);
    }
    commands
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

/// Registers or removes the server for `target`. Returns the text for stdout and, when something was
/// installed, a hint for stderr so that stdout stays machine-readable.
pub fn install(
    target: Target,
    scopes: &str,
    accounts: &[String],
    save_dir: Option<&Path>,
    remove: bool,
    dry_run: bool,
) -> Result<(String, Option<String>)> {
    install_from(
        target,
        scopes,
        accounts,
        save_dir,
        remove,
        dry_run,
        flatpak_id().as_deref(),
    )
}

/// `flatpak` is the app id when this runs in the Flatpak sandbox.
fn install_from(
    target: Target,
    scopes: &str,
    accounts: &[String],
    save_dir: Option<&Path>,
    remove: bool,
    dry_run: bool,
    flatpak: Option<&str>,
) -> Result<(String, Option<String>)> {
    let granted: Vec<&str> = parse_scopes(scopes)?
        .into_iter()
        .map(|s| s.as_str())
        .collect();
    // Absolute, as the host starts the server in a working directory of its own.
    let save_dir = save_dir
        .map(|dir| {
            dir.canonicalize()
                .with_context(|| format!("no directory {}", dir.display()))
        })
        .transpose()?;
    let exe = stable_exe().context("finding the postvak binary")?;
    let entry = Entry::new(&exe, &granted.join(","), accounts, save_dir.as_deref());
    let entry = match flatpak {
        Some(id) => entry.through_flatpak(id),
        None => entry,
    };
    let wanted = (!remove).then_some(&entry);
    let out = if let Some((program, app)) = host_cli(target) {
        // In the Flatpak the host's CLI is not on PATH, so this prints the commands.
        install_cli(program, app, wanted, remove, dry_run)?
    } else if let Some(file) = ConfigFile::of(target)? {
        // The sandbox cannot reach another app's config.
        match flatpak {
            Some(_) if remove => bail!(
                "the Flatpak cannot edit {}'s config; remove {}.postvak from {} yourself",
                file.app,
                file.section,
                file.path.display()
            ),
            Some(_) => bail!(
                "the Flatpak cannot edit {}'s config; add this to {} yourself:\n{}",
                file.app,
                file.path.display(),
                file.snippet(&entry)
            ),
            None => install_file(&file, wanted, &entry, dry_run)?,
        }
    } else {
        json_snippet(&entry)
    };
    let hint = (!remove).then(|| {
        format!(
            "Scopes: {}. For an inbox assistant: --scopes read,read:headers,read:bodies,rules:propose,mail:modify\n",
            granted.join(", ")
        )
    });
    Ok((out, hint))
}

/// Real runs never echo the config: it may hold other servers' secrets.
fn install_file(
    file: &ConfigFile,
    wanted: Option<&Entry>,
    entry: &Entry,
    dry_run: bool,
) -> Result<String> {
    let server = wanted.map(|entry| file.server(entry));
    let changed = edit_config(&file.path, file.section, server, dry_run)?.is_some();
    let (app, path, section) = (file.app, file.path.display(), file.section);
    Ok(match (wanted.is_some(), changed, dry_run) {
        (false, false, _) => format!("nothing to remove in {path}\n"),
        (false, true, true) => format!("would remove {section}.postvak from {path}\n"),
        (false, true, false) => {
            format!("removed Postvak from {path}\nRestart {app} to apply.\n")
        }
        (true, _, true) => format!("would write to {path}:\n{}", file.snippet(entry)),
        (true, _, false) => {
            format!("updated {path}\nRestart {app} to load Postvak.\n")
        }
    })
}

fn install_cli(
    program: &str,
    app: &str,
    wanted: Option<&Entry>,
    remove: bool,
    dry_run: bool,
) -> Result<String> {
    let commands = cli_commands(program, wanted);
    if dry_run || !on_path(program) {
        let mut out = if dry_run {
            "would run:\n".to_string()
        } else {
            format!("{program} is not on PATH; run:\n")
        };
        for command in &commands {
            out.push_str(&format!("  {}\n", shell_words(command)));
        }
        return Ok(out);
    }
    for (index, command) in commands.iter().enumerate() {
        let status = std::process::Command::new(&command[0])
            .args(&command[1..])
            .status()
            .with_context(|| format!("running {}", shell_words(command)))?;
        // In an install the leading remove clears an entry that may not exist; every other command must succeed.
        if (remove || index > 0) && !status.success() {
            bail!("`{}` failed", shell_words(command));
        }
    }
    Ok(if remove {
        format!("removed from {app}\n")
    } else {
        format!("registered with {app} at user scope\n")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> Entry {
        Entry::new(
            Path::new("/opt/homebrew/bin/postvak"),
            "read,rules:propose",
            &["work".into()],
            None,
        )
    }

    #[test]
    fn snippet_names_the_binary_and_its_arguments() {
        let snippet: serde_json::Value = serde_json::from_str(&json_snippet(&entry())).unwrap();
        assert_eq!(
            snippet["mcpServers"]["postvak"]["command"],
            "/opt/homebrew/bin/postvak"
        );
        assert_eq!(
            snippet["mcpServers"]["postvak"]["args"],
            serde_json::json!(["mcp", "--scopes", "read,rules:propose", "--account", "work"])
        );
    }

    #[test]
    fn a_flatpak_entry_runs_the_app_through_flatpak() {
        let entry = entry().through_flatpak("io.github.postvak_app.postvak");
        assert_eq!(
            shell_words(&entry.argv()),
            "flatpak run io.github.postvak_app.postvak mcp --scopes read,rules:propose --account work"
        );
    }

    #[test]
    fn in_a_flatpak_the_json_entry_runs_flatpak() {
        let (out, _) = install_from(
            Target::Json,
            "read",
            &[],
            None,
            false,
            false,
            Some("io.github.postvak_app.postvak"),
        )
        .unwrap();
        assert!(out.contains(r#""command": "flatpak""#), "{out}");
        assert!(out.contains(r#""io.github.postvak_app.postvak""#), "{out}");
    }

    #[test]
    fn in_a_flatpak_a_config_host_gets_the_entry_to_add_by_hand() {
        let accounts = ["work".to_string()];
        let id = Some("io.github.postvak_app.postvak");
        let error = install_from(
            Target::ClaudeDesktop,
            "read",
            &accounts,
            None,
            false,
            false,
            id,
        )
        .unwrap_err();
        let error = error.to_string();
        assert!(error.contains("claude_desktop_config.json"), "{error}");
        assert!(
            error.contains(
                r#""args": [
        "run",
        "io.github.postvak_app.postvak",
        "mcp",
        "--scopes",
        "read",
        "--account",
        "work"
      ]"#
            ),
            "{error}"
        );
        let error =
            install_from(Target::ClaudeDesktop, "read", &[], None, true, false, id).unwrap_err();
        assert!(
            error.to_string().contains("remove mcpServers.postvak"),
            "{error}"
        );
    }

    #[test]
    fn desktop_config_is_created_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Claude/claude_desktop_config.json");
        edit_config(&path, "mcpServers", Some(entry().server()), false).unwrap();
        let config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config["mcpServers"]["postvak"]["args"][0], "mcp");
    }

    #[test]
    fn other_servers_and_keys_survive_and_a_backup_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude_desktop_config.json");
        let old = r#"{"theme":"dark","mcpServers":{"other":{"command":"x"}}}"#;
        std::fs::write(&path, old).unwrap();
        edit_config(&path, "mcpServers", Some(entry().server()), false).unwrap();
        let config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config["theme"], "dark");
        assert_eq!(config["mcpServers"]["other"]["command"], "x");
        assert!(config["mcpServers"]["postvak"].is_object());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("claude_desktop_config.json.bak")).unwrap(),
            old
        );
    }

    #[test]
    fn invalid_json_is_refused_and_nothing_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude_desktop_config.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(edit_config(&path, "mcpServers", Some(entry().server()), false).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
        assert!(!dir.path().join("claude_desktop_config.json.bak").exists());
    }

    fn read_json(path: &Path) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn remove_keeps_other_servers_and_never_adds_an_empty_section() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude_desktop_config.json");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"other":{"command":"x"},"postvak":{"command":"p"}}}"#,
        )
        .unwrap();
        assert!(
            edit_config(&path, "mcpServers", None, false)
                .unwrap()
                .is_some()
        );
        let config = read_json(&path);
        assert!(config["mcpServers"].get("postvak").is_none());
        assert_eq!(config["mcpServers"]["other"]["command"], "x");

        let bare = dir.path().join("bare.json");
        std::fs::write(&bare, r#"{"theme":"dark"}"#).unwrap();
        assert!(
            edit_config(&bare, "mcpServers", None, false)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            std::fs::read_to_string(&bare).unwrap(),
            r#"{"theme":"dark"}"#
        );

        let missing = dir.path().join("none.json");
        assert!(
            edit_config(&missing, "mcpServers", None, false)
                .unwrap()
                .is_none()
        );
        assert!(!missing.exists());
    }

    #[test]
    fn dry_run_writes_neither_config_nor_backup() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("none.json");
        let preview = edit_config(&missing, "mcpServers", Some(entry().server()), true)
            .unwrap()
            .unwrap();
        assert!(preview.contains("postvak") && !missing.exists());

        let path = dir.path().join("claude_desktop_config.json");
        let old = r#"{"theme":"dark"}"#;
        std::fs::write(&path, old).unwrap();
        edit_config(&path, "mcpServers", Some(entry().server()), true).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), old);
        assert!(!dir.path().join("claude_desktop_config.json.bak").exists());
    }

    #[test]
    fn the_original_backup_survives_a_re_run() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude_desktop_config.json");
        let bak = dir.path().join("claude_desktop_config.json.bak");
        let original = r#"{"theme":"dark"}"#;
        std::fs::write(&path, original).unwrap();
        edit_config(&path, "mcpServers", Some(entry().server()), false).unwrap();
        let wider = Entry::new(
            Path::new("/opt/homebrew/bin/postvak"),
            "read,read:bodies",
            &[],
            None,
        );
        edit_config(&path, "mcpServers", Some(wider.server()), false).unwrap();
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), original);
        assert_eq!(
            read_json(&path)["mcpServers"]["postvak"]["args"][2],
            "read,read:bodies"
        );
    }

    #[test]
    fn the_backup_keeps_the_config_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude_desktop_config.json");
        std::fs::write(&path, "{}\n ").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        edit_config(&path, "mcpServers", Some(entry().server()), false).unwrap();
        let bak = dir.path().join("claude_desktop_config.json.bak");
        assert_eq!(
            std::fs::metadata(bak).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn an_existing_config_folder_keeps_its_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Claude");
        std::fs::create_dir(&folder).unwrap();
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
        edit_config(
            &folder.join("claude_desktop_config.json"),
            "mcpServers",
            Some(entry().server()),
            false,
        )
        .unwrap();
        assert_eq!(
            std::fs::metadata(&folder).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[test]
    fn claude_code_replaces_an_existing_entry() {
        let commands = cli_commands("claude", Some(&entry()));
        assert_eq!(
            commands[0],
            ["claude", "mcp", "remove", "--scope", "user", "postvak"]
        );
        assert_eq!(
            &commands[1][..7],
            ["claude", "mcp", "add", "--scope", "user", "postvak", "--"]
        );
        assert_eq!(commands[1][7], "/opt/homebrew/bin/postvak");
        assert_eq!(cli_commands("claude", None).len(), 1);
    }

    #[test]
    fn codex_and_copilot_take_the_whole_command_after_the_separator() {
        for program in ["codex", "copilot"] {
            let commands = cli_commands(program, Some(&entry()));
            assert_eq!(commands[0], [program, "mcp", "remove", "postvak"]);
            assert_eq!(
                shell_words(&commands[1]),
                format!(
                    "{program} mcp add postvak -- /opt/homebrew/bin/postvak mcp --scopes read,rules:propose --account work"
                )
            );
        }
    }

    #[test]
    fn gemini_takes_the_command_before_the_separator_and_its_arguments_after() {
        let commands = cli_commands("gemini", Some(&entry()));
        assert_eq!(
            commands[0],
            ["gemini", "mcp", "remove", "--scope", "user", "postvak"]
        );
        assert_eq!(
            shell_words(&commands[1]),
            "gemini mcp add --scope user postvak /opt/homebrew/bin/postvak -- mcp --scopes read,rules:propose --account work"
        );
    }

    #[test]
    fn opencode_takes_the_command_line_as_one_array_under_mcp() {
        let file = ConfigFile {
            app: "opencode",
            path: PathBuf::new(),
            section: "mcp",
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.json");
        std::fs::write(
            &path,
            r#"{"$schema":"https://opencode.ai/config.json","theme":"dark"}"#,
        )
        .unwrap();
        edit_config(&path, "mcp", Some(file.server(&entry())), false).unwrap();
        let config = read_json(&path);
        assert_eq!(config["theme"], "dark");
        assert_eq!(
            config["mcp"]["postvak"],
            serde_json::json!({
                "type": "local",
                "command": ["/opt/homebrew/bin/postvak", "mcp", "--scopes", "read,rules:propose", "--account", "work"],
                "enabled": true
            })
        );
        assert!(edit_config(&path, "mcp", None, false).unwrap().is_some());
        assert!(read_json(&path)["mcp"].get("postvak").is_none());
    }

    #[test]
    fn every_target_has_exactly_one_way_in() {
        use Target::*;
        for target in [
            ClaudeCode,
            ClaudeDesktop,
            Codex,
            Copilot,
            Cursor,
            Gemini,
            Json,
            Opencode,
        ] {
            let ways = usize::from(host_cli(target).is_some())
                + usize::from(ConfigFile::of(target).unwrap().is_some())
                + usize::from(target == Json);
            assert_eq!(ways, 1, "{target:?}");
        }
    }

    #[test]
    fn shell_quoting_survives_spaces() {
        let e = Entry::new(Path::new("/Users/me/My Tools/postvak"), "read", &[], None);
        assert!(shell_words(&e.argv()).starts_with("'/Users/me/My Tools/postvak' mcp"));
    }

    #[test]
    fn install_json_validates_scopes_and_lists_them() {
        let (text, hint) =
            install(Target::Json, "read,rules:propose", &[], None, false, false).unwrap();
        assert!(text.contains("mcpServers") && serde_json::from_str::<Value>(&text).is_ok());
        assert!(hint.unwrap().contains("read, rules:propose"));
        let (_, removed_hint) = install(Target::Json, "read", &[], None, true, false).unwrap();
        assert!(removed_hint.is_none());
        assert!(install(Target::Json, "read,everything", &[], None, false, false).is_err());
    }

    #[test]
    fn a_symlinked_config_is_written_through_and_stays_a_link() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("dotfiles.json");
        std::fs::write(&target, "{}").unwrap();
        let link = dir.path().join("claude_desktop_config.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        edit_config(&link, "mcpServers", Some(entry().server()), false).unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            std::fs::read_to_string(&target)
                .unwrap()
                .contains("postvak")
        );
    }
}
