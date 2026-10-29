use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths::{edit_toml, read_or_empty};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config.toml: {0}")]
    Parse(String),
    #[error("config.toml: {0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl From<toml_edit::TomlError> for ConfigError {
    fn from(e: toml_edit::TomlError) -> Self {
        ConfigError::Parse(e.to_string())
    }
}

/// Beside config.toml; every write holds it, so two front ends saving at once keep each other's change.
const LOCK: &str = ".config.lock";

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub accounts: Vec<AccountConfig>,
    #[serde(default, skip_serializing_if = "UiConfig::is_default")]
    pub ui: UiConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountConfig {
    pub name: String,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub username: String,
    pub password: PasswordSource,
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default = "default_sync_interval")]
    pub sync_interval_secs: u64,
    #[serde(default = "default_retention")]
    pub trash_retention_days: u64,
    #[serde(default = "default_true")]
    pub notify: bool,
    /// Download and index the text of every message in the background, so search covers bodies.
    #[serde(default = "default_true")]
    pub index_bodies: bool,
    /// Extra trusted root certificate (PEM), for servers with a private CA.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca_file: Option<PathBuf>,
    /// How the connection is encrypted; unset follows the port, see `AccountConfig::tls`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<Tls>,
    /// The servers whose `Authentication-Results` the `authenticated` rule condition believes, by authserv-id. Unset
    /// or empty: none, so `authenticated = true` never holds. No default is guessed from `host`: a provider that writes
    /// no such header leaves the topmost one to the sender, who can name any server in the provider's domain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trusted_authserv_ids: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tls {
    /// TLS from the first byte, as on port 993.
    Implicit,
    /// A plain greeting upgraded with STARTTLS, as on port 143; a server that does not offer it is refused.
    Starttls,
    /// STARTTLS when the server offers it, else a plain-text login. Only for servers that leave no other way.
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PasswordSource {
    Keyring { keyring: bool },
    Command { command: String },
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiConfig {
    /// Ask GitHub once a day whether a newer release is out.
    #[serde(default)]
    pub check_updates: bool,
    /// How HTML mail looks in the dark theme: adapted to it, or in the sender's colours on a white page.
    #[serde(default)]
    pub html_colours: HtmlColours,
    /// Show the logo a sender's domain publishes (BIMI): a DNS lookup and an HTTPS fetch to that domain.
    #[serde(default)]
    pub sender_images: bool,
    #[serde(default)]
    pub theme: Theme,
}

impl UiConfig {
    fn is_default(&self) -> bool {
        *self == UiConfig::default()
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Dark,
    Light,
    #[default]
    System,
}

/// HTML mail in the dark theme (GUI only: the CLI and MCP never render HTML).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HtmlColours {
    /// Styled with the dark colour scheme, and light pages turned dark.
    #[default]
    Adapt,
    /// The sender's colours on a white page, as in the light theme.
    Original,
}

impl Theme {
    pub fn as_str(self) -> &'static str {
        match self {
            Theme::Dark => "dark",
            Theme::Light => "light",
            Theme::System => "system",
        }
    }
}

fn default_port() -> u16 {
    993
}
fn default_sync_interval() -> u64 {
    120
}
fn default_retention() -> u64 {
    30
}
fn default_true() -> bool {
    true
}

impl Config {
    pub fn parse(text: &str) -> Result<Config, ConfigError> {
        let cfg: Config = toml::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        Config::parse(&read_or_empty(path)?)
    }

    pub fn account(&self, name: &str) -> Option<&AccountConfig> {
        self.accounts.iter().find(|a| a.name == name)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        let mut seen = std::collections::HashSet::new();
        for account in &self.accounts {
            let name = &account.name;
            let safe = !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            if !safe {
                return Err(ConfigError::Invalid(format!(
                    "account name '{name}' may only contain letters, digits, '-' and '_'"
                )));
            }
            if !seen.insert(name) {
                return Err(ConfigError::Invalid(format!(
                    "duplicate account name '{name}'"
                )));
            }
            if !account.address().contains('@') {
                return Err(ConfigError::Invalid(format!(
                    "account '{name}': `address` must be an email address; set it when the username is not one"
                )));
            }
            if account.password == (PasswordSource::Keyring { keyring: false }) {
                return Err(ConfigError::Invalid(format!(
                    "account '{name}': `password = {{ keyring = false }}` has no meaning; use `{{ keyring = true }}` or a `command`"
                )));
            }
            if account.trash_retention_days == 0 {
                return Err(ConfigError::Invalid(format!(
                    "account '{name}': trash_retention_days must be at least 1, or a rule delete keeps no backup"
                )));
            }
            if let Some(ca_file) = &account.ca_file
                && !ca_file.is_absolute()
            {
                return Err(ConfigError::Invalid(format!(
                    "account '{name}': ca_file must be an absolute path"
                )));
            }
            for id in account.trusted_authserv_ids.iter().flatten() {
                let token = !id.is_empty()
                    && id
                        .chars()
                        .all(|c| c.is_ascii_graphic() && !"\";()\\".contains(c));
                if !token {
                    return Err(ConfigError::Invalid(format!(
                        "account '{name}': trusted_authserv_ids holds '{}'; each must be a server name such as mx.example.com",
                        id.escape_default()
                    )));
                }
            }
            account.identity()?;
        }
        Ok(())
    }
}

/// Writes `account` into config.toml as the file reads now, in place of the account of the same name or added after
/// the others. The rest stays as it is on disk: comments, the other accounts and `[ui]`, so a front end holding an
/// older `Config` cannot drop what another one saved since. A key the account leaves at its default is only written
/// when the file already had it. Nothing is written unless the whole file validates; returns the config it now holds.
pub fn save_account(path: &Path, account: &AccountConfig) -> Result<Config, ConfigError> {
    let new = to_table(account)?;
    let defaults = account_defaults()?;
    edit_toml(path, LOCK, |doc| {
        let item = doc
            .entry("accounts")
            .or_insert(toml_edit::Item::ArrayOfTables(Default::default()));
        // `accounts = [{ … }]` holds the same tables inline; editing it as `[[accounts]]` blocks keeps every key.
        if item.is_value() {
            let inline = std::mem::take(item);
            let tables = match inline.as_array() {
                Some(list) if list.is_empty() => Ok(toml_edit::ArrayOfTables::new()),
                _ => inline.into_array_of_tables(),
            };
            *item =
                toml_edit::Item::ArrayOfTables(tables.map_err(|_| {
                    ConfigError::Invalid("`accounts` must be a list of tables".into())
                })?);
        }
        let accounts = item
            .as_array_of_tables_mut()
            .ok_or_else(|| ConfigError::Invalid("`accounts` must be a list of tables".into()))?;
        let index = accounts
            .iter()
            .position(|t| t.get("name").and_then(|n| n.as_str()) == Some(account.name.as_str()));
        match index.and_then(|index| accounts.get_mut(index)) {
            Some(table) => merge_table(table, &new, &defaults),
            None => {
                let mut table = toml_edit::Table::new();
                merge_table(&mut table, &new, &defaults);
                accounts.push(table);
            }
        }
        Config::parse(&doc.to_string())
    })
}

/// Sets `[ui] theme`, keeping the rest of the file's text and comments.
pub fn save_theme(path: &Path, theme: Theme) -> Result<(), ConfigError> {
    edit_toml(path, LOCK, |doc| {
        let ui = doc
            .entry("ui")
            .or_insert_with(toml_edit::table)
            .as_table_like_mut()
            .ok_or_else(|| ConfigError::Invalid("`ui` must be a table".into()))?;
        ui.insert("theme", toml_edit::value(theme.as_str()));
        Ok(())
    })
}

fn to_table(value: &impl Serialize) -> Result<toml_edit::Table, ConfigError> {
    let text = toml::to_string(value).map_err(|e| ConfigError::Invalid(e.to_string()))?;
    let doc: toml_edit::DocumentMut = text.parse()?;
    Ok(doc.as_table().clone())
}

/// The values an account gets for the keys it leaves out; the required keys are never defaults.
fn account_defaults() -> Result<toml_edit::Table, ConfigError> {
    let mut defaults = to_table(&AccountConfig::new(
        "",
        "",
        "",
        PasswordSource::Keyring { keyring: true },
    ))?;
    for required in ["name", "host", "username", "password"] {
        defaults.remove(required);
    }
    Ok(defaults)
}

/// Makes `old` hold `new`'s values: keys `new` lacks go, changed values are replaced keeping their comments, and a
/// key `old` lacks is only added when it differs from `defaults`. Nested tables are written inline.
fn merge_table(old: &mut toml_edit::Table, new: &toml_edit::Table, defaults: &toml_edit::Table) {
    let gone: Vec<String> = old
        .iter()
        .map(|(key, _)| key.to_string())
        .filter(|key| !new.contains_key(key))
        .collect();
    for key in gone {
        old.remove(&key);
    }
    for (key, item) in new.iter() {
        let Ok(mut value) = item.clone().into_value() else {
            continue;
        };
        value.decor_mut().clear();
        match old.get_mut(key) {
            Some(existing) if same_value(existing, item) => {}
            Some(existing) => {
                if let Some(decor) = existing.as_value().map(|v| v.decor().clone()) {
                    *value.decor_mut() = decor;
                }
                *existing = toml_edit::Item::Value(value);
            }
            None if defaults.get(key).is_some_and(|d| same_value(d, item)) => {}
            None => {
                old.insert(key, toml_edit::Item::Value(value));
            }
        }
    }
}

/// Whether two items hold the same data, whatever their layout: an inline table equals a `[section]`.
fn same_value(a: &toml_edit::Item, b: &toml_edit::Item) -> bool {
    let plain = |item: &toml_edit::Item| {
        let mut doc = toml_edit::DocumentMut::new();
        doc.insert("v", item.clone());
        toml::from_str::<toml::Table>(&doc.to_string())
            .ok()
            .and_then(|mut table| table.remove("v"))
    };
    plain(a).is_some_and(|value| Some(value) == plain(b))
}

impl AccountConfig {
    /// An account with the defaults config.toml gives the settings it leaves out.
    pub fn new(name: &str, host: &str, username: &str, password: PasswordSource) -> AccountConfig {
        AccountConfig {
            name: name.into(),
            host: host.into(),
            port: default_port(),
            username: username.into(),
            password,
            address: None,
            aliases: Vec::new(),
            sync_interval_secs: default_sync_interval(),
            trash_retention_days: default_retention(),
            notify: default_true(),
            index_bodies: default_true(),
            ca_file: None,
            tls: None,
            trusted_authserv_ids: None,
        }
    }

    /// The `tls` setting, or else what the port implies: STARTTLS on 143, implicit TLS elsewhere.
    pub fn tls(&self) -> Tls {
        self.tls.unwrap_or(if self.port == 143 {
            Tls::Starttls
        } else {
            Tls::Implicit
        })
    }

    pub fn address(&self) -> &str {
        self.address.as_deref().unwrap_or(&self.username)
    }

    /// Whether the account trusts any authserv-id; without one, `authenticated = true` never holds.
    pub fn can_authenticate(&self) -> bool {
        self.trusted_authserv_ids
            .as_ref()
            .is_some_and(|ids| !ids.is_empty())
    }

    pub fn identity(&self) -> Result<Identity, ConfigError> {
        let aliases = regex::RegexSet::new(self.aliases.iter().map(|pattern| alias_regex(pattern)))
            .map_err(|e| ConfigError::Invalid(format!("account '{}': aliases: {e}", self.name)))?;
        let authserv = self
            .trusted_authserv_ids
            .iter()
            .flatten()
            .map(|id| id.trim_end_matches('.').to_lowercase())
            .collect();
        Ok(Identity {
            address: self.address().to_ascii_lowercase(),
            aliases,
            patterns: self.aliases.clone(),
            authserv,
        })
    }
}

/// An alias pattern as a regex over the whole address, ignoring case; `*` matches anything.
pub(crate) fn alias_regex(pattern: &str) -> String {
    format!("(?i)^{}$", regex::escape(pattern).replace(r"\*", ".*"))
}

#[derive(Debug, Clone)]
pub struct Identity {
    address: String,
    aliases: regex::RegexSet,
    /// The alias patterns as configured, for code that matches them outside this type, such as a search.
    patterns: Vec<String>,
    /// The trusted authserv-ids, lower-cased and without a trailing dot.
    authserv: Vec<String>,
}

impl Identity {
    pub fn is_me(&self, addr: &str) -> bool {
        let addr = addr.trim().to_ascii_lowercase();
        addr == self.address || self.aliases.is_match(&addr)
    }

    /// The account's own address, lower-cased.
    pub fn address(&self) -> &str {
        &self.address
    }

    /// The alias patterns as configured: whole addresses, with `*` for anything.
    pub fn alias_patterns(&self) -> &[String] {
        &self.patterns
    }

    /// Whether the account believes an `Authentication-Results` header written by this authserv-id.
    pub fn trusts_authserv_id(&self, id: &str) -> bool {
        self.authserv
            .contains(&id.trim_end_matches('.').to_lowercase())
    }

    /// Whether the account trusts any authserv-id; without one, `authenticated = true` never holds.
    pub fn can_authenticate(&self) -> bool {
        !self.authserv.is_empty()
    }

    /// Whether the account's provider vouches for mail with these headers and this From field: see
    /// [`auth::authenticated`](crate::message::auth::authenticated).
    pub fn authenticated(&self, raw_headers: &[u8], from: &str) -> bool {
        crate::message::auth::authenticated(raw_headers, from, |id| self.trusts_authserv_id(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_patterns_take_star_as_the_only_wildcard() {
        let alias = |pattern: &str| regex::Regex::new(&alias_regex(pattern)).unwrap();
        assert!(alias("*@shop.example.com").is_match("Orders@SHOP.example.com"));
        assert!(!alias("*@shop.example.com").is_match("orders@shop.example.com.evil"));
        assert!(alias("a+b@x.org").is_match("a+b@x.org"));
        assert!(!alias("a+b@x.org").is_match("aab@xxorg"));
        assert!(!alias("me?@x.org").is_match("mex@x.org"));
    }

    const SAMPLE: &str = r#"
[[accounts]]
name = "work"
host = "imap.example.com"
username = "pieter@example.com"
password = { keyring = true }
aliases = ["p@example.org", "*@shop.example.com"]

[[accounts]]
name = "home"
host = "mail.home.test"
port = 143
username = "pieter"
address = "pieter@home.test"
password = { command = "pass show mail/home" }
notify = false
trusted_authserv_ids = ["mx.home.test"]
"#;

    #[test]
    fn an_explicit_tls_setting_beats_the_port() {
        let cfg = Config::parse(&format!("{SAMPLE}tls = \"implicit\"\n")).unwrap();
        assert_eq!(cfg.account("home").unwrap().tls(), Tls::Implicit);
        let cfg =
            Config::parse(&SAMPLE.replace("port = 143", "port = 1143\ntls = \"none\"")).unwrap();
        assert_eq!(cfg.account("home").unwrap().tls(), Tls::None);
        assert!(Config::parse(&format!("{SAMPLE}tls = \"plain\"\n")).is_err());
    }

    #[test]
    fn parses_accounts_with_defaults() {
        let cfg = Config::parse(SAMPLE).unwrap();
        let work = cfg.account("work").unwrap();
        assert_eq!(work.port, 993);
        assert_eq!(work.sync_interval_secs, 120);
        assert_eq!(work.trash_retention_days, 30);
        assert!(work.notify);
        assert!(matches!(
            work.password,
            PasswordSource::Keyring { keyring: true }
        ));
        assert_eq!(work.tls(), Tls::Implicit);
        let home = cfg.account("home").unwrap();
        assert_eq!(home.port, 143);
        assert_eq!(home.tls(), Tls::Starttls);
        assert!(!home.notify);
        assert!(
            matches!(&home.password, PasswordSource::Command { command } if command == "pass show mail/home")
        );
    }

    #[test]
    fn new_gives_the_defaults_config_toml_gives() {
        let cfg = Config::parse(SAMPLE).unwrap();
        let mut work = cfg.account("work").unwrap().clone();
        work.aliases.clear();
        let new = AccountConfig::new(
            "work",
            "imap.example.com",
            "pieter@example.com",
            PasswordSource::Keyring { keyring: true },
        );
        assert_eq!(new, work);
    }

    #[test]
    fn address_defaults_to_username_when_it_is_an_email() {
        let cfg = Config::parse(SAMPLE).unwrap();
        assert_eq!(cfg.account("work").unwrap().address(), "pieter@example.com");
        assert_eq!(cfg.account("home").unwrap().address(), "pieter@home.test");
    }

    #[test]
    fn identity_matches_address_and_aliases_case_insensitively() {
        let cfg = Config::parse(SAMPLE).unwrap();
        let id = cfg.account("work").unwrap().identity().unwrap();
        assert!(id.is_me("Pieter@Example.com"));
        assert!(id.is_me("p@example.org"));
        assert!(id.is_me("orders@shop.example.com"));
        assert!(!id.is_me("someone@example.com"));
    }

    #[test]
    fn no_authserv_id_is_trusted_by_default() {
        // A regression: the IMAP host's domain was trusted by default, so on a server that writes no
        // Authentication-Results a sender's own header naming mx.example.org counted.
        for host in ["imap.example.org", "127.0.0.1", "localhost"] {
            let id = AccountConfig::new(
                "a",
                host,
                "me@example.org",
                PasswordSource::Keyring { keyring: true },
            )
            .identity()
            .unwrap();
            assert!(!id.can_authenticate(), "{host}");
            assert!(!id.trusts_authserv_id("mx.example.org"), "{host}");
            assert!(!id.authenticated(
                b"Authentication-Results: mx.example.org; dmarc=pass header.from=paypal.com\r\n\r\n",
                "service@paypal.com"
            ));
        }
    }

    #[test]
    fn only_listed_authserv_ids_are_trusted() {
        let text = SAMPLE.replace(
            "aliases = [\"p@example.org\", \"*@shop.example.com\"]",
            "trusted_authserv_ids = [\"mx.google.com\", \"Spam.Example.com\"]",
        );
        let cfg = Config::parse(&text).unwrap();
        let work = cfg.account("work").unwrap().identity().unwrap();
        assert!(work.trusts_authserv_id("mx.google.com"));
        assert!(work.trusts_authserv_id("Spam.Example.com."));
        assert!(work.can_authenticate());
        assert!(!work.trusts_authserv_id("mx.example.com"));
        let none = text.replace("[\"mx.google.com\", \"Spam.Example.com\"]", "[]");
        let work = Config::parse(&none).unwrap().accounts[0]
            .identity()
            .unwrap();
        assert!(!work.trusts_authserv_id("mx.example.com"));
        assert!(!work.can_authenticate());
        for bad in ["\"\"", "\"mx example\"", "\"a;dmarc=pass\""] {
            let text = text.replace("\"mx.google.com\"", bad);
            assert!(
                matches!(Config::parse(&text), Err(ConfigError::Invalid(e)) if e.contains("trusted_authserv_ids")),
                "{bad}"
            );
        }
    }

    #[test]
    fn rejects_duplicate_and_unsafe_names() {
        let dup = SAMPLE.replace("name = \"home\"", "name = \"work\"");
        assert!(matches!(Config::parse(&dup), Err(ConfigError::Invalid(_))));
        let bad = SAMPLE.replace("name = \"home\"", "name = \"ho/me\"");
        assert!(matches!(Config::parse(&bad), Err(ConfigError::Invalid(_))));
    }

    #[test]
    fn rejects_a_trash_retention_of_zero_days() {
        let bad = SAMPLE.replace(
            "name = \"home\"",
            "name = \"home\"\ntrash_retention_days = 0",
        );
        assert!(
            matches!(Config::parse(&bad), Err(ConfigError::Invalid(e)) if e.contains("trash_retention_days")),
        );
    }

    #[test]
    fn rejects_keyring_false() {
        let bad = SAMPLE.replace("{ keyring = true }", "{ keyring = false }");
        assert!(
            matches!(Config::parse(&bad), Err(ConfigError::Invalid(e)) if e.contains("keyring = false")),
            "keyring = false must not quietly use the keyring"
        );
    }

    #[test]
    fn rejects_unknown_account_keys() {
        let typo = SAMPLE.replace("aliases = ", "alias = ");
        assert!(
            matches!(Config::parse(&typo), Err(ConfigError::Parse(e)) if e.contains("alias")),
            "a misspelled key must not be ignored"
        );
    }

    #[test]
    fn rejects_unknown_top_level_keys() {
        let typo = SAMPLE.replace("[[accounts]]", "[[account]]");
        assert!(
            matches!(Config::parse(&typo), Err(ConfigError::Parse(e)) if e.contains("account")),
            "a misspelled table must not leave the user without accounts"
        );
    }

    #[test]
    fn rejects_username_without_at_and_no_address() {
        let bad = SAMPLE.replace("address = \"pieter@home.test\"\n", "");
        assert!(matches!(Config::parse(&bad), Err(ConfigError::Invalid(_))));
    }

    #[test]
    fn rejects_an_address_that_is_no_email_address() {
        let bad = SAMPLE.replace("pieter@home.test", "pieter");
        assert!(matches!(Config::parse(&bad), Err(ConfigError::Invalid(_))));
    }

    #[test]
    fn relative_ca_file_is_rejected() {
        let text = "[[accounts]]\nname = \"self\"\nhost = \"localhost\"\nusername = \"me@example.com\"\npassword = { keyring = true }\nca_file = \"certs/ca.pem\"\n";
        let err = Config::parse(text).unwrap_err().to_string();
        assert!(err.contains("ca_file must be an absolute path"), "{err}");
        let absolute = text.replace("certs/ca.pem", "/etc/ssl/self/ca.pem");
        assert!(
            Config::parse(&absolute).unwrap().accounts[0]
                .ca_file
                .is_some()
        );
    }

    #[test]
    fn load_missing_file_is_empty_and_save_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let cfg = Config::load(&path).unwrap();
        assert!(cfg.accounts.is_empty());
        let cfg = Config::parse(SAMPLE).unwrap();
        for account in &cfg.accounts {
            save_account(&path, account).unwrap();
        }
        let again = Config::load(&path).unwrap();
        assert_eq!(again.accounts, cfg.accounts);
    }

    #[test]
    fn ui_theme_defaults_to_system() {
        assert_eq!(Config::parse(SAMPLE).unwrap().ui.theme, Theme::System);
    }

    #[test]
    fn ui_theme_parses_and_rejects_unknown_values() {
        let dark = format!("{SAMPLE}\n[ui]\ntheme = \"dark\"\n");
        assert_eq!(Config::parse(&dark).unwrap().ui.theme, Theme::Dark);
        let blue = format!("{SAMPLE}\n[ui]\ntheme = \"blue\"\n");
        assert!(matches!(Config::parse(&blue), Err(ConfigError::Parse(_))));
        let typo = format!("{SAMPLE}\n[ui]\ntheme_ = \"dark\"\n");
        assert!(matches!(Config::parse(&typo), Err(ConfigError::Parse(_))));
    }

    #[test]
    fn html_colours_default_to_adapt_and_parse() {
        let ui =
            |text: &str| Config::parse(&format!("{SAMPLE}\n{text}")).map(|c| c.ui.html_colours);
        assert_eq!(ui("").unwrap(), HtmlColours::Adapt);
        assert_eq!(
            ui("[ui]\nhtml_colours = \"original\"\n").unwrap(),
            HtmlColours::Original
        );
        assert_eq!(
            ui("[ui]\nhtml_colours = \"adapt\"\n").unwrap(),
            HtmlColours::Adapt
        );
        assert!(matches!(
            ui("[ui]\nhtml_colours = \"inverted\"\n"),
            Err(ConfigError::Parse(_))
        ));
    }

    #[test]
    fn save_theme_keeps_the_rest_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let text = format!("# my accounts\n{SAMPLE}");
        std::fs::write(&path, &text).unwrap();
        save_theme(&path, Theme::Dark).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.starts_with(&text), "{saved}");
        assert!(saved.contains("[ui]\ntheme = \"dark\""), "{saved}");
        save_theme(&path, Theme::Light).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(
            saved.contains("theme = \"light\"") && !saved.contains("dark"),
            "{saved}"
        );
        let cfg = Config::load(&path).unwrap();
        assert_eq!((cfg.ui.theme, cfg.accounts.len()), (Theme::Light, 2));
    }

    #[test]
    fn save_account_keeps_comments_and_writes_only_what_changed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let text = "# my accounts\n\n# the day job\n[[accounts]]\nname = \"work\"\nhost = \"imap.example.com\" # via the VPN\nport = 993\nusername = \"pieter@example.com\"\npassword = { keyring = true }\n\n[[accounts]]\nname = \"home\"\nhost = \"mail.home.test\"\nusername = \"pieter@home.test\"\npassword = { keyring = true }\n";
        std::fs::write(&path, text).unwrap();
        let mut work = Config::load(&path).unwrap().accounts[0].clone();
        work.port = 994;
        work.aliases = vec!["p@example.org".into()];
        save_account(&path, &work).unwrap();
        let password = PasswordSource::Command {
            command: "pass show mail/extra".into(),
        };
        let extra = AccountConfig::new("extra", "imap.extra.test", "me@extra.test", password);
        let cfg = save_account(&path, &extra).unwrap();

        let saved = std::fs::read_to_string(&path).unwrap();
        for kept in [
            "# my accounts",
            "# the day job",
            "host = \"imap.example.com\" # via the VPN",
        ] {
            assert!(saved.contains(kept), "lost {kept:?}:\n{saved}");
        }
        assert!(
            saved.contains("port = 994")
                && saved.contains("aliases = [\"p@example.org\"]")
                && saved.contains("name = \"home\"\nhost = \"mail.home.test\"\n"),
            "{saved}"
        );
        assert!(
            !saved.contains("sync_interval_secs"),
            "defaults are left out:\n{saved}"
        );
        assert!(
            saved.contains("password = { command = \"pass show mail/extra\" }"),
            "{saved}"
        );
        let again = Config::load(&path).unwrap();
        assert_eq!(again.accounts, cfg.accounts);
        let names: Vec<&str> = cfg.accounts.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["work", "home", "extra"]);
        assert_eq!(cfg.accounts[0], work);
    }

    #[test]
    fn save_account_leaves_out_a_default_ui_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        save_account(&path, &Config::parse(SAMPLE).unwrap().accounts[0]).unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("[ui]"));
    }

    #[test]
    fn save_account_keeps_an_account_saved_since_the_config_was_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, SAMPLE).unwrap();
        let stale = Config::load(&path).unwrap();
        let password = PasswordSource::Keyring { keyring: true };
        let added = AccountConfig::new("added", "imap.added.test", "me@added.test", password);
        save_account(&path, &added).unwrap();
        save_theme(&path, Theme::Light).unwrap();

        let mut changed = stale.accounts[0].clone();
        changed.port = 1993;
        let saved = save_account(&path, &changed).unwrap();
        assert!(saved.account("added").is_some(), "{saved:?}");
        assert_eq!(saved.ui.theme, Theme::Light);
        assert_eq!(saved.accounts[0].port, 1993);
        assert_eq!(Config::load(&path).unwrap().accounts, saved.accounts);
    }

    #[test]
    fn concurrent_saves_keep_every_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::thread::scope(|scope| {
            for i in 0..8 {
                let path = &path;
                scope.spawn(move || {
                    let password = PasswordSource::Keyring { keyring: true };
                    let account = AccountConfig::new(
                        &format!("a{i}"),
                        "imap.test",
                        "me@test.example",
                        password,
                    );
                    save_account(path, &account).unwrap();
                });
            }
            scope.spawn(|| save_theme(&path, Theme::Dark).unwrap());
        });
        let saved = Config::load(&path).unwrap();
        assert_eq!(saved.accounts.len(), 8);
        assert_eq!(saved.ui.theme, Theme::Dark);
    }

    #[test]
    fn save_account_writes_nothing_when_the_file_would_not_validate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, SAMPLE).unwrap();
        let mut bad = Config::parse(SAMPLE).unwrap().accounts[0].clone();
        bad.trash_retention_days = 0;
        assert!(matches!(
            save_account(&path, &bad),
            Err(ConfigError::Invalid(_))
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SAMPLE);
    }

    #[test]
    fn save_account_into_a_file_with_only_ui_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[ui]\ntheme = \"dark\"\n").unwrap();
        let account = Config::parse(SAMPLE).unwrap().accounts[0].clone();
        let saved = save_account(&path, &account).unwrap();
        assert_eq!(
            (saved.accounts, saved.ui.theme),
            (vec![account], Theme::Dark)
        );
    }

    #[test]
    fn save_account_edits_an_inline_accounts_list() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "accounts = []\n").unwrap();
        let account = Config::parse(SAMPLE).unwrap().accounts[0].clone();
        let saved = save_account(&path, &account).unwrap();
        assert_eq!(saved.accounts, [account]);
    }

    #[test]
    fn check_updates_is_off_unless_turned_on() {
        assert!(!Config::default().ui.check_updates);
        assert!(!Config::parse(SAMPLE).unwrap().ui.check_updates);
        let theme_only = format!("{SAMPLE}\n[ui]\ntheme = \"dark\"\n");
        assert!(!Config::parse(&theme_only).unwrap().ui.check_updates);
        let on = format!("{SAMPLE}\n[ui]\ncheck_updates = true\n");
        assert!(Config::parse(&on).unwrap().ui.check_updates);
    }

    #[test]
    fn sender_images_are_off_unless_turned_on() {
        assert!(!Config::default().ui.sender_images);
        assert!(!Config::parse(SAMPLE).unwrap().ui.sender_images);
        let on = format!("{SAMPLE}\n[ui]\nsender_images = true\n");
        assert!(Config::parse(&on).unwrap().ui.sender_images);
    }
}
