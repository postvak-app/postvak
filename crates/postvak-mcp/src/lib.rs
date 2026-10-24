//! `postvak mcp`: the agent surface over MCP on stdio, limited by scopes set in the host's config.
mod backend;
pub mod install;
#[cfg(test)]
mod junk_sweep;
#[cfg(test)]
mod tests;
mod tools;

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Result, bail};
use rmcp::ServiceExt;

pub use backend::Backend;
pub use tools::Server;

use postvak_core::config::Config;
use postvak_core::paths::Paths;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    FilesWrite,
    MailModify,
    Read,
    ReadBodies,
    RulesPropose,
    RulesWrite,
}

impl Scope {
    pub const ALL: [Scope; 6] = [
        Scope::Read,
        Scope::ReadBodies,
        Scope::RulesPropose,
        Scope::RulesWrite,
        Scope::FilesWrite,
        Scope::MailModify,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Scope::FilesWrite => "files:write",
            Scope::MailModify => "mail:modify",
            Scope::Read => "read",
            Scope::ReadBodies => "read:bodies",
            Scope::RulesPropose => "rules:propose",
            Scope::RulesWrite => "rules:write",
        }
    }
}

/// Scopes earlier versions granted. A host config written then still names them, so they are skipped with a warning
/// rather than keeping the server from starting. `accounts:write` let a prompt-injected agent run a shell command.
const REMOVED: [&str; 1] = ["accounts:write"];

/// A comma-separated scope list; an unknown scope, or none at all, names the valid ones.
pub fn parse_scopes(list: &str) -> Result<BTreeSet<Scope>> {
    let valid = || Scope::ALL.map(Scope::as_str).join(", ");
    let mut scopes = BTreeSet::new();
    for word in list.split(',').map(str::trim).filter(|w| !w.is_empty()) {
        if REMOVED.contains(&word) {
            eprintln!(
                "postvak: scope '{word}' was removed and grants nothing; add accounts with `postvak account add`"
            );
            continue;
        }
        match Scope::ALL.into_iter().find(|s| s.as_str() == word) {
            Some(scope) => scopes.insert(scope),
            None => bail!("unknown scope '{word}'; valid scopes: {}", valid()),
        };
    }
    if scopes.is_empty() {
        bail!("no scopes given; valid scopes: {}", valid());
    }
    Ok(scopes)
}

/// Serves on stdin and stdout until the host closes the pipe. `save_dir` replaces the Downloads folder as the one
/// `attachment_save` writes into.
pub fn run(
    config: &Config,
    paths: &Paths,
    scopes: &str,
    accounts: &[String],
    save_dir: Option<&Path>,
) -> Result<()> {
    let mut backend = Backend::new(config, paths, accounts)?;
    if let Some(dir) = save_dir {
        if !dir.is_absolute() {
            bail!("--save-dir must be an absolute path");
        }
        backend = backend.saving_into(Some(dir.to_path_buf()));
    }
    let server = Server::new(backend, parse_scopes(scopes)?);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let running = server.serve(rmcp::transport::stdio()).await?;
        running.waiting().await?;
        Ok(())
    })
}
