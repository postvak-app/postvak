use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use clap::{Parser, Subcommand};

use postvak_core::account;
use postvak_core::config::{self, AccountConfig, Config, Identity, PasswordSource, Tls};
use postvak_core::credentials::Secret;
use postvak_core::daemon::Client;
use postvak_core::daemon::client::LazyClient;
use postvak_core::daemon::wire::Status;
use postvak_core::message::clean;
use postvak_core::output::{Fields, RowKind};
use postvak_core::page;
use postvak_core::paths::Paths;
use postvak_core::rules::edit::Place;
use postvak_core::rules::{Action, CompiledRule, Rule, RuleFile};
use postvak_core::store::{Filter, Message, Store};
use postvak_core::sync::{self, Event};
use postvak_core::time;
use postvak_core::trash::Trash;

#[derive(Parser)]
#[command(
    name = "postvak",
    version,
    about = "Your inbox, organized by rules you write"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Args)]
struct Selection {
    /// Message uids in --folder, as `list` prints them
    #[arg(required = true)]
    uids: Vec<u32>,
    #[arg(long)]
    account: Option<String>,
    #[arg(long, default_value = "INBOX")]
    folder: String,
    /// Print what would happen without touching the server
    #[arg(long)]
    dry_run: bool,
}

#[derive(Clone, Copy, Default, clap::ValueEnum)]
enum GroupBy {
    /// The sender's address
    #[default]
    Address,
    /// The registrable domain of the sender's address
    Domain,
}

impl From<GroupBy> for postvak_core::senders::GroupBy {
    fn from(by: GroupBy) -> Self {
        match by {
            GroupBy::Address => postvak_core::senders::GroupBy::Address,
            GroupBy::Domain => postvak_core::senders::GroupBy::Domain,
        }
    }
}

/// How `list` and `search` page, shape and count their rows.
#[derive(clap::Args)]
struct Paging {
    #[arg(long, help = postvak_core::help::CURSOR)]
    cursor: Option<String>,
    #[arg(long, value_delimiter = ',', requires = "json", help = postvak_core::help::FIELDS)]
    fields: Vec<String>,
    #[arg(long, conflicts_with_all = ["cursor", "fields"], help = postvak_core::help::COUNT_ONLY)]
    count_only: bool,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Mark {
    Flagged,
    Read,
    Unflagged,
    Unread,
}

#[derive(Subcommand)]
enum Command {
    /// Run the daemon in the foreground: sync all accounts continuously and apply rules; fails when a daemon already runs; Ctrl-C stops
    Run {
        /// Exit after this many seconds with no client connected; what auto-start uses
        #[arg(long, hide = true)]
        idle_exit: Option<u64>,
    },
    /// Inspect or stop the daemon that syncs your accounts; other commands start it when needed
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Open the mail window; starts the daemon when needed
    Gui {
        /// Draw with the software renderer and ignore the saved window layout, for when the window will not open;
        /// entered by itself after two starts in a row that died before the window showed
        #[arg(long)]
        safe_mode: bool,
        /// Show this message (a uid in --folder, as `list` prints it); with a window already open, that window shows it
        #[arg(long, value_name = "UID")]
        open: Option<u32>,
        /// The account of the message to show; the first account when left out
        #[arg(long, requires = "open")]
        account: Option<String>,
        /// The folder of the message to show
        #[arg(long, default_value = "INBOX", requires = "open")]
        folder: String,
        /// Start out of the way, as at login: in the tray, minimized on macOS, or shown where there is no tray
        #[arg(long, conflicts_with = "open")]
        background: bool,
        /// Show the Rules view, where proposals wait for approval; with a window already open, that window shows it
        #[arg(long, conflicts_with_all = ["open", "background"])]
        rules: bool,
    },
    /// Open the window at login, hidden in the tray: a launch agent on macOS, an autostart entry on Linux
    Autostart {
        #[command(subcommand)]
        command: AutostartCommand,
    },
    /// Start the daemon at login: a launchd agent on macOS, a systemd user unit on Linux
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    #[command(about = postvak_core::help::SYNC)]
    Sync {
        #[arg(long)]
        account: Option<String>,
    },
    /// List or save a message's attachments
    Attachment {
        #[command(subcommand)]
        command: AttachmentCommand,
    },
    /// Inspect and test rules.toml
    Rules {
        #[command(subcommand)]
        command: RulesCommand,
    },
    #[command(about = postvak_core::help::FOLDERS)]
    Folders {
        #[arg(long)]
        account: Option<String>,
        #[arg(long)]
        json: bool,
    },
    #[command(about = postvak_core::help::LIST)]
    List {
        #[arg(long)]
        account: Option<String>,
        #[arg(long, default_value = "INBOX")]
        folder: String,
        #[arg(long, default_value_t = 50)]
        limit: u32,
        #[arg(long)]
        json: bool,
        /// Group by conversation, the most recently active thread first
        #[arg(long)]
        threads: bool,
        /// Only unread messages; with --threads, threads with an unread message
        #[arg(long)]
        unread: bool,
        /// Only flagged messages; with --threads, threads with a flagged message
        #[arg(long)]
        flagged: bool,
        /// Only messages with attachments, guessed from the headers; with --threads, threads with one
        #[arg(long)]
        attachments: bool,
        #[command(flatten)]
        paging: Paging,
    },
    #[command(about = postvak_core::help::SEARCH, after_help = postvak_core::help::SEARCH_SYNTAX)]
    Search {
        query: String,
        /// Only this account; the same as account: in the query
        #[arg(long)]
        account: Option<String>,
        /// Only this folder; the same as in: in the query
        #[arg(long)]
        folder: Option<String>,
        /// Ask the server too, which searches bodies not downloaded yet; each folder costs a round trip. One page: no --cursor or --count-only
        #[arg(long, conflicts_with_all = ["cursor", "count_only"])]
        server: bool,
        /// Only what matches as typed: no close matches such as corrected typos
        #[arg(long)]
        exact: bool,
        #[arg(long, default_value_t = 50)]
        limit: u32,
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        paging: Paging,
    },
    #[command(about = postvak_core::help::SENDERS)]
    Senders {
        #[arg(long)]
        account: Option<String>,
        #[arg(long, default_value = "INBOX")]
        folder: String,
        #[arg(long, value_enum, default_value_t)]
        group_by: GroupBy,
        /// At most this many groups per account
        #[arg(long, default_value_t = 50)]
        limit: u32,
        /// Only mail that arrived on or after this date: 2026-01-15, or 30d, 3w, 6mo, 1y back from now
        #[arg(long)]
        after: Option<String>,
        /// Only mail that arrived before this date: 2026-01-15, or 30d, 3w, 6mo, 1y back from now
        #[arg(long)]
        before: Option<String>,
        #[arg(long)]
        json: bool,
    },
    #[command(about = postvak_core::help::HEADERS)]
    Headers {
        uid: u32,
        #[arg(long)]
        account: Option<String>,
        #[arg(long, default_value = "INBOX")]
        folder: String,
        #[arg(long)]
        json: bool,
    },
    #[command(about = postvak_core::help::SHOW)]
    Show {
        uid: u32,
        #[arg(long)]
        account: Option<String>,
        #[arg(long, default_value = "INBOX")]
        folder: String,
        /// Print the raw RFC 5322 message instead of the text body
        #[arg(long)]
        raw: bool,
        #[arg(long)]
        json: bool,
    },
    #[command(about = postvak_core::help::UNSUBSCRIBE)]
    Unsubscribe {
        uid: u32,
        #[arg(long)]
        account: Option<String>,
        #[arg(long, default_value = "INBOX")]
        folder: String,
        /// Print what would happen without contacting the sender
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
    #[command(about = postvak_core::help::MARK)]
    Mark {
        #[arg(value_enum)]
        how: Mark,
        #[command(flatten)]
        selection: Selection,
    },
    #[command(about = postvak_core::help::MOVE)]
    Move {
        #[arg(long)]
        to: String,
        #[command(flatten)]
        selection: Selection,
    },
    #[command(about = postvak_core::help::ARCHIVE)]
    Archive {
        #[command(flatten)]
        selection: Selection,
    },
    #[command(about = postvak_core::help::DELETE)]
    Delete {
        #[command(flatten)]
        selection: Selection,
    },
    #[command(about = postvak_core::help::JUNK)]
    Junk {
        #[command(flatten)]
        selection: Selection,
        /// Not junk after all
        #[arg(long)]
        not: bool,
    },
    #[command(about = postvak_core::help::LOG)]
    Log {
        #[arg(long)]
        account: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
        #[arg(long)]
        json: bool,
    },
    /// Deleted mail kept for the retention period
    Trash {
        #[command(subcommand)]
        command: TrashCommand,
    },
    /// Manage accounts
    Account {
        #[command(subcommand)]
        command: AccountCommand,
    },
    /// List the local crash reports, newest first; they are never sent anywhere
    Crashes {
        #[command(subcommand)]
        command: Option<CrashesCommand>,
    },
    /// Print the agent guide, how an LLM should drive Postvak, or another page of the documentation
    Guide {
        /// The page; without one, the agent guide
        #[arg(value_parser = doc_pages())]
        page: Option<String>,
    },
    /// Serve Postvak to an agent host over MCP on stdio; hosts start this, see `postvak mcp install`
    #[command(args_conflicts_with_subcommands = true)]
    Mcp {
        /// Comma-separated: read, read:headers, read:bodies, rules:propose, rules:write, files:write, mail:modify
        #[arg(long, default_value = postvak_core::help::MCP_DEFAULT_SCOPES)]
        scopes: String,
        /// Only this account; repeatable; default every account
        #[arg(long)]
        account: Vec<String>,
        /// The one folder files:write saves attachments into, or below; default your Downloads folder
        #[arg(long)]
        save_dir: Option<PathBuf>,
        #[command(subcommand)]
        command: Option<McpCommand>,
    },
}

#[derive(Subcommand)]
enum CrashesCommand {
    /// Print one crash report, by the id `postvak crashes` lists
    Show { id: String },
}

#[derive(Subcommand)]
enum DaemonCommand {
    #[command(about = postvak_core::help::STATUS)]
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Stop the daemon; it starts again when a command needs it
    Stop,
}

#[derive(Subcommand)]
enum AutostartCommand {
    /// Open the window at login; in a Flatpak the desktop may ask first
    On,
    /// Stop opening the window at login
    Off,
    /// Say whether the window opens at login
    Status,
}

#[derive(Subcommand)]
enum ServiceCommand {
    /// Install and start the service; stops a daemon that is already running so the service's takes over
    Install {
        /// Print the file and the commands and change nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Stop and remove the service
    Remove {
        /// Print the commands and change nothing
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum McpCommand {
    /// Register `postvak mcp` with an agent host; re-run it to change the scopes
    Install {
        #[arg(value_enum)]
        target: InstallTarget,
        /// Comma-separated: read, read:headers, read:bodies, rules:propose, rules:write, files:write, mail:modify
        #[arg(long, default_value = postvak_core::help::MCP_DEFAULT_SCOPES)]
        scopes: String,
        /// Only this account; repeatable; default every account
        #[arg(long)]
        account: Vec<String>,
        /// The one folder files:write saves attachments into, or below; default your Downloads folder
        #[arg(long)]
        save_dir: Option<PathBuf>,
        /// Take the entry out again
        #[arg(long)]
        remove: bool,
        /// Print the change and write nothing
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum InstallTarget {
    ClaudeCode,
    ClaudeDesktop,
    Json,
}

#[derive(Subcommand)]
enum AttachmentCommand {
    #[command(about = postvak_core::help::ATTACHMENT_LIST)]
    List {
        uid: u32,
        #[arg(long)]
        account: Option<String>,
        #[arg(long, default_value = "INBOX")]
        folder: String,
        #[arg(long)]
        json: bool,
    },
    #[command(about = postvak_core::help::ATTACHMENT_SAVE)]
    Save {
        uid: u32,
        n: usize,
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        #[arg(long)]
        account: Option<String>,
        #[arg(long, default_value = "INBOX")]
        folder: String,
    },
}

#[derive(Subcommand)]
enum RulesCommand {
    #[command(about = postvak_core::help::RULES_CHECK)]
    Check,
    /// Dry run: print what each rule would do to the cached messages; naming a rule previews it even while disabled
    Test {
        #[arg(conflicts_with = "stdin")]
        name: Option<String>,
        #[arg(long)]
        account: Option<String>,
        /// Preview one rule read as JSON from stdin instead of rules.toml
        #[arg(long)]
        stdin: bool,
    },
    #[command(
        about = postvak_core::help::RULES_SCHEMA,
        after_help = "`postvak guide rules` explains every key, with examples."
    )]
    Schema,
    /// Read one rule, a JSON array of rules or {"rules": [...]} on stdin and add them disabled, in order, for a human to approve; all or none are stored
    Propose {
        /// Who proposes it, recorded as proposed_by = "cli:WHO"
        #[arg(long)]
        by: Option<String>,
        /// Put the rules right before this rule instead of at the end
        #[arg(long, conflicts_with = "after")]
        before: Option<String>,
        /// Put the rules right after this rule instead of at the end
        #[arg(long)]
        after: Option<String>,
        /// Replace your own pending proposal of the same name instead of refusing the name
        #[arg(long)]
        replace: bool,
        /// Propose changes to the existing rules of these names, stored as '<name> (edit)'; each rule runs unchanged until approved
        #[arg(long, conflicts_with_all = ["before", "after"])]
        edit: bool,
    },
    #[command(about = postvak_core::help::RULES_WITHDRAW)]
    Withdraw {
        name: String,
        /// Who proposed it, as given to `rules propose --by`
        #[arg(long)]
        by: Option<String>,
    },
    #[command(about = postvak_core::help::RULES_APPROVE)]
    Approve { name: String },
    #[command(about = postvak_core::help::RULES_REJECT)]
    Reject { name: String },
    #[command(about = postvak_core::help::RULES_LIST)]
    List {
        #[arg(long)]
        json: bool,
    },
    #[command(about = postvak_core::help::RULES_COVERAGE)]
    Coverage {
        #[arg(long)]
        account: Option<String>,
        #[arg(long, default_value = "INBOX")]
        folder: String,
        /// Count the proposals still waiting for approval as if they were enabled
        #[arg(long)]
        include_pending: bool,
        /// How to group the senders of unmatched mail
        #[arg(long, value_enum, default_value_t)]
        group_by: GroupBy,
        /// At most this many unmatched senders and contested messages per account; the counts cover everything
        #[arg(long, default_value_t = 20)]
        limit: u32,
        #[arg(long)]
        json: bool,
    },
    #[command(about = postvak_core::help::RULES_APPLY_EXISTING)]
    ApplyExisting {
        name: String,
        #[arg(long)]
        account: Option<String>,
        #[arg(long)]
        dry_run: bool,
        /// Run even when the rule would permanently delete mail from the server
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum TrashCommand {
    #[command(about = postvak_core::help::TRASH_LIST)]
    List {
        #[arg(long)]
        account: Option<String>,
    },
    #[command(about = postvak_core::help::TRASH_RESTORE)]
    Restore {
        file: String,
        #[arg(long)]
        account: Option<String>,
    },
    #[command(about = postvak_core::help::TRASH_PURGE)]
    Purge {
        #[arg(long)]
        account: Option<String>,
    },
}

#[derive(Subcommand)]
enum AccountCommand {
    #[command(about = postvak_core::help::ACCOUNT_ADD)]
    Add,
    #[command(about = postvak_core::help::ACCOUNT_LIST)]
    List {
        #[arg(long)]
        json: bool,
    },
    #[command(about = postvak_core::help::ACCOUNT_TRUST)]
    Trust {
        account: String,
        authserv_id: String,
    },
}

pub fn run() -> Result<()> {
    let cli = if opened_as_mac_app() {
        Cli {
            command: Command::Gui {
                safe_mode: false,
                open: None,
                account: None,
                folder: "INBOX".into(),
                background: false,
                rules: false,
            },
        }
    } else {
        Cli::parse()
    };
    let paths = match std::env::var_os("POSTVAK_HOME") {
        Some(home) => Ok(Paths::under(Path::new(&home))),
        None => Paths::discover(),
    };
    if let Ok(paths) = &paths {
        postvak_core::crash::install(paths.crash_dir(), process(&cli.command));
    }
    // The window explains its own startup errors, since it may have no terminal to print them in.
    if let Command::Gui {
        safe_mode,
        open,
        account,
        folder,
        background,
        rules,
    } = cli.command
    {
        let show = if rules {
            Some(postvak_core::notify::Show::Rules)
        } else {
            open.map(|uid| {
                postvak_core::notify::Show::Message(postvak_core::notify::Open {
                    account,
                    folder,
                    uid,
                })
            })
        };
        return cmd_gui(paths, safe_mode, show, background);
    }
    let paths = paths?;
    let config = Config::load(&paths.config_file())?;
    match cli.command {
        Command::Run { idle_exit } => cmd_run(&paths, idle_exit),
        Command::Crashes { command } => cmd_crashes(command, &paths),
        Command::Daemon { command } => cmd_daemon(command, &paths),
        // Handled above, before the config loads; kept for an exhaustive match.
        Command::Gui {
            safe_mode,
            background,
            ..
        } => cmd_gui(Ok(paths), safe_mode, None, background),
        Command::Autostart { command } => cmd_autostart(command, &paths),
        Command::Service { command } => cmd_service(command, &paths),
        Command::Sync { account } => cmd_sync(&config, &paths, account.as_deref()),
        Command::Attachment { command } => cmd_attachment(command, &config, &paths),
        Command::Rules { command } => cmd_rules(command, &config, &paths),
        Command::Senders {
            account,
            folder,
            group_by,
            limit,
            after,
            before,
            json,
        } => {
            let now = time::now();
            let after = after.as_deref().map(|d| parse_date(d, now)).transpose()?;
            let before = before.as_deref().map(|d| parse_date(d, now)).transpose()?;
            let rules = postvak_core::rules::coverage::rules(
                &postvak_core::rules::load(&paths.rules_file())?,
                false,
            )?;
            for acc in config::select(&config.accounts, account.as_deref())? {
                let store = Store::open_account(&paths, &acc.name)?;
                let identity = acc.identity()?;
                let ctx = postvak_core::rules::coverage::context(&acc.name, &identity, now);
                let senders = postvak_core::senders::in_folder(
                    &store,
                    &rules,
                    &ctx,
                    &folder,
                    after,
                    before,
                    group_by.into(),
                )?;
                for sender in senders.iter().take(limit as usize) {
                    if json {
                        print_json(postvak_core::output::sender(&acc.name, &folder, sender)?);
                    } else {
                        println!("{}", sender_line(&acc.name, sender));
                    }
                }
            }
            Ok(())
        }
        Command::Folders { account, json } => {
            for acc in config::select(&config.accounts, account.as_deref())? {
                let store = Store::open_account(&paths, &acc.name)?;
                for f in store.folders()? {
                    let total = store.message_count(&f.name)?;
                    let unread = store.unread_count(&f.name)?;
                    if json {
                        print_json(postvak_core::output::folder(&acc.name, &f, total, unread));
                    } else {
                        println!("{}\t{}\t{total}\t{unread}", acc.name, clean(&f.name, false));
                    }
                }
            }
            Ok(())
        }
        Command::List {
            account,
            folder,
            limit,
            json,
            threads,
            unread,
            flagged,
            attachments,
            paging,
        } => {
            let request = page::ListRequest {
                folder: &folder,
                filter: Filter {
                    unread,
                    flagged,
                    attachments,
                },
                threads,
            };
            let kind = if threads {
                RowKind::Thread
            } else {
                RowKind::Message
            };
            let fields = Fields::parse(&paging.fields, kind)?;
            let sources = sources(&config, &paths, account.as_deref(), false)?;
            if paging.count_only {
                print_count(page::list_count(&sources, &request)?, json);
                return Ok(());
            }
            let found = page::list(&sources, &request, limit, paging.cursor.as_deref())?;
            for row in &found.rows {
                if json {
                    print_json(postvak_core::output::list_row(row, &fields)?);
                } else {
                    let depth = row.depth.unwrap_or(0);
                    println!("{}", message_line(&row.account, &row.message, depth));
                }
            }
            print_next(found.next.as_deref(), json);
            Ok(())
        }
        Command::Search {
            query,
            account,
            folder,
            server,
            exact,
            limit,
            json,
            paging,
        } => {
            let fields = Fields::parse(&paging.fields, RowKind::Hit)?;
            let parsed = postvak_core::search::parse(&query, postvak_core::time::now());
            let account = postvak_core::search::named_account(account.as_deref(), &parsed)?;
            let sources = sources(&config, &paths, account, true)?;
            let request = page::SearchQuery {
                query: &parsed,
                scope: postvak_core::search::Scope::All,
                folder: folder.as_deref(),
                close: !exact,
            };
            if paging.count_only {
                print_count(page::search_count(&sources, &request)?, json);
                return Ok(());
            }
            let mut found = page::search(&sources, &request, limit, paging.cursor.as_deref())?;
            if server {
                let daemon = LazyClient::new(&paths);
                for source in &sources {
                    match daemon.search_server(&source.account, folder.as_deref(), &query) {
                        Ok(hits) => {
                            found = page::with_server_hits(
                                found,
                                &source.account,
                                &source.store,
                                &hits,
                                limit,
                            )?
                        }
                        Err(e) => eprintln!(
                            "{}: {}; showing local results only",
                            source.account,
                            clean(&format!("{e:#}"), false)
                        ),
                    }
                }
            }
            for row in &found.rows {
                if json {
                    print_json(postvak_core::output::search_row(row, &fields)?);
                } else {
                    let line = message_line(&row.account, &row.hit.message, 0);
                    match row.hit.matched {
                        postvak_core::store::Matched::Exact => println!("{line}"),
                        postvak_core::store::Matched::Close => println!("~ {line}"),
                    }
                }
            }
            print_next(found.next.as_deref(), json);
            Ok(())
        }
        Command::Unsubscribe {
            uid,
            account,
            folder,
            dry_run,
            json,
        } => {
            let (acc, _store, msg) =
                open_message(&config, &paths, account.as_deref(), &folder, uid)?;
            let report = postvak_core::unsubscribe::unsubscribe(&msg.headers, dry_run)?;
            if json {
                print_json(postvak_core::output::with_account(&acc.name, &report)?);
            } else {
                println!("{}", unsubscribe_line(&report));
            }
            Ok(())
        }
        Command::Headers {
            uid,
            account,
            folder,
            json,
        } => {
            let (acc, _store, msg) =
                open_message(&config, &paths, account.as_deref(), &folder, uid)?;
            let fields = postvak_core::message::header_fields(&msg.headers);
            if json {
                print_json(postvak_core::output::headers(&acc.name, &msg, &fields)?);
            } else {
                for field in fields {
                    println!(
                        "{}: {}",
                        clean(&field.name, false),
                        clean(&field.value, false)
                    );
                }
            }
            Ok(())
        }
        Command::Show {
            uid,
            account,
            folder,
            raw,
            json,
        } => {
            let (acc, store, msg) =
                open_message(&config, &paths, account.as_deref(), &folder, uid)?;
            let body = LazyClient::new(&paths).raw_message(&acc.name, &store, &msg)?;
            if raw {
                io::stdout().write_all(&body)?;
            } else if json {
                let text = postvak_core::message::body_text(&body);
                print_json(postvak_core::output::show(&acc.name, &msg, &text, false)?);
            } else {
                println!(
                    "From: {}\nTo: {}\nSubject: {}\n",
                    clean(msg.from_addr.as_deref().unwrap_or(""), false),
                    clean(msg.to_addr.as_deref().unwrap_or(""), false),
                    clean(msg.subject.as_deref().unwrap_or(""), false)
                );
                println!("{}", clean(&postvak_core::message::body_text(&body), true));
            }
            Ok(())
        }
        Command::Mark { how, selection } => {
            let action = match how {
                Mark::Flagged => Action::Flag,
                Mark::Read => Action::MarkRead,
                Mark::Unflagged => Action::Unflag,
                Mark::Unread => Action::MarkUnread,
            };
            cmd_act(&config, &paths, selection, action)
        }
        Command::Move { to, selection } => {
            if to.trim().is_empty() {
                bail!("--to must name a folder");
            }
            cmd_act(&config, &paths, selection, Action::Move(to))
        }
        Command::Archive { selection } => cmd_act(&config, &paths, selection, Action::Archive),
        Command::Delete { selection } => cmd_act(&config, &paths, selection, Action::Trash),
        Command::Junk { selection, not } => {
            let action = if not { Action::NotJunk } else { Action::Junk };
            cmd_act(&config, &paths, selection, action)
        }
        Command::Log {
            account,
            limit,
            json,
        } => {
            for acc in config::select(&config.accounts, account.as_deref())? {
                let store = Store::open_account(&paths, &acc.name)?;
                for e in store.log(limit)? {
                    if json {
                        print_json(postvak_core::output::with_account(&acc.name, &e)?);
                    } else {
                        println!(
                            "{}  {}  {:<20} {:<12} {}/{}  {}",
                            acc.name,
                            time::date_time(e.at),
                            clean(&e.rule_name, false),
                            clean(&e.action, false),
                            clean(&e.folder, false),
                            e.uid,
                            clean(e.subject.as_deref().unwrap_or(""), false)
                        );
                    }
                }
            }
            Ok(())
        }
        Command::Trash { command } => cmd_trash(command, &config, &paths),
        Command::Account { command } => match command {
            AccountCommand::Add => cmd_account_add(config, &paths),
            AccountCommand::List { json } => {
                for acc in &config.accounts {
                    if json {
                        print_json(postvak_core::output::account(acc));
                    } else {
                        println!(
                            "{}\t{}\t{}:{}",
                            acc.name,
                            clean(&acc.username, false),
                            acc.host,
                            acc.port
                        );
                    }
                }
                Ok(())
            }
            AccountCommand::Trust {
                account,
                authserv_id,
            } => {
                postvak_core::account::trust_authserv_id(&paths, &account, &authserv_id)?;
                println!(
                    "account '{}' now trusts the Authentication-Results of {}",
                    clean(&account, false),
                    clean(&authserv_id, false)
                );
                Ok(())
            }
        },
        Command::Guide { page } => {
            let text = match page.as_deref() {
                None => postvak_core::help::AGENT_GUIDE,
                Some(name) => match postvak_core::help::doc_page(name) {
                    Some(page) => page.text,
                    // clap only lets the page names through.
                    None => bail!("no page {name}"),
                },
            };
            print!("{text}");
            Ok(())
        }
        Command::Mcp {
            scopes,
            account,
            save_dir,
            command,
        } => cmd_mcp(
            &config,
            &paths,
            &scopes,
            &account,
            save_dir.as_deref(),
            command,
        ),
    }
}

/// The names `postvak guide` takes, each with its summary, which clap lists in `--help` and when a name is wrong.
fn doc_pages() -> clap::builder::PossibleValuesParser {
    clap::builder::PossibleValuesParser::new(
        postvak_core::help::DOC_PAGES
            .iter()
            .map(|page| clap::builder::PossibleValue::new(page.name).help(page.summary)),
    )
}

/// Runs a direct action on the selected uids; `--dry-run` only reads the local store.
fn cmd_act(config: &Config, paths: &Paths, selection: Selection, action: Action) -> Result<()> {
    let acc = config::single(&config.accounts, selection.account.as_deref())?;
    let store = Store::open_account(paths, &acc.name)?;
    let folder = clean(&selection.folder, false);
    if selection.dry_run {
        let mut missing = 0;
        for &uid in &selection.uids {
            match store.message(&selection.folder, uid)? {
                Some(m) => println!(
                    "{}  {folder}/{uid}  {}",
                    postvak_core::actions::planned_effect(&store, &m, &action)?,
                    clean(m.subject.as_deref().unwrap_or(""), false)
                ),
                None => {
                    eprintln!("{folder}/{uid}: not in the local store");
                    missing += 1;
                }
            }
        }
        if missing > 0 {
            bail!("{missing} messages are not in the local store; run `postvak sync` first");
        }
        return Ok(());
    }
    let results = LazyClient::new(paths).apply(
        &acc.name,
        &selection.folder,
        &selection.uids,
        &action,
        postvak_core::actions::RULE_NAME,
    )?;
    let mut failed = 0;
    for (uid, result) in &results {
        if let Err(message) = result {
            eprintln!("{folder}/{uid}: {}", clean(message, false));
            failed += 1;
        }
    }
    println!(
        "{}: {} of {} messages",
        clean(&action.command_label(), false),
        results.len() - failed,
        results.len()
    );
    if failed > 0 {
        bail!("{failed} of {} messages failed", results.len());
    }
    Ok(())
}

fn unsubscribe_line(report: &postvak_core::unsubscribe::Report) -> String {
    let url = clean(&report.url, false);
    match (report.method, report.done) {
        (_, true) => "unsubscribed: the sender's server accepted it".to_string(),
        ("one_click", false) => format!("would unsubscribe by one-click POST to {url}"),
        ("mail", false) => format!("send a mail to unsubscribe: {url}"),
        _ => format!("open this page to unsubscribe: {url}"),
    }
}

fn cmd_sync(config: &Config, paths: &Paths, account: Option<&str>) -> Result<()> {
    let accounts = config::select(&config.accounts, account)?;
    let names: Vec<&str> = accounts.iter().map(|acc| acc.name.as_str()).collect();
    let client = Client::connect_or_start(paths)?;
    let events = client.subscribe()?;
    std::thread::scope(|scope| {
        let printer = scope.spawn(|| print_new_mail(events, &names));
        let mut failed = false;
        for name in &names {
            failed |= !sync_account(&client, name);
        }
        // Closing the connection ends the event stream once the printer has drained it.
        drop(client);
        let _ = printer.join();
        if failed {
            bail!("sync failed for at least one account or folder");
        }
        Ok(())
    })
}

/// Prints the new mail of `accounts` until the stream ends.
fn print_new_mail(events: Receiver<Event>, accounts: &[&str]) {
    for event in &events {
        if let Event::NewMail { account, .. } = &event
            && accounts.contains(&account.as_str())
        {
            postvak_core::daemon::report(&event);
        }
    }
}

/// Syncs one account through the daemon and reports it with its pass's errors; false when it failed or had any.
fn sync_account(client: &Client, name: &str) -> bool {
    match client.request(name, sync::Command::SyncNow) {
        Ok(event) => {
            postvak_core::daemon::report(&event);
            let errors = match &event {
                Event::Synced { errors, .. } => errors.as_slice(),
                _ => &[],
            };
            for message in errors {
                postvak_core::daemon::report_error(name, message);
            }
            errors.is_empty()
        }
        Err(e) => {
            postvak_core::daemon::report_error(name, &format!("{e:#}"));
            false
        }
    }
}

fn print_status(status: &Status) {
    let minutes = status.uptime_secs / 60;
    println!(
        "pid {}, version {}, up {}h {}m, {} clients",
        status.pid,
        status.version,
        minutes / 60,
        minutes % 60,
        status.clients
    );
    for account in &status.accounts {
        let activity = account
            .activity
            .as_ref()
            .map_or("starting".into(), ToString::to_string);
        println!("{}  {activity}", account.name);
        if account.encrypted == Some(false) {
            println!("  note: {}", postvak_core::help::UNENCRYPTED);
        }
        if account.uidplus == Some(false) {
            println!("  note: {}", postvak_core::help::NO_UIDPLUS);
        }
    }
}

fn cmd_daemon(command: DaemonCommand, paths: &Paths) -> Result<()> {
    let Some(client) = Client::connect_any_version(paths)? else {
        let json = matches!(command, DaemonCommand::Status { json: true });
        println!("{}", if json { "null" } else { "no daemon running" });
        return Ok(());
    };
    match command {
        DaemonCommand::Status { json } => {
            let status = client.status()?;
            if json {
                print_json(postvak_core::output::status(&status)?);
            } else {
                print_status(&status);
            }
        }
        DaemonCommand::Stop => {
            let pid = client.stop_and_wait(paths)?;
            println!("stopped the daemon (pid {pid})");
        }
    }
    Ok(())
}

fn cmd_autostart(command: AutostartCommand, paths: &Paths) -> Result<()> {
    let text = match command {
        AutostartCommand::On => postvak_core::autostart::set(paths, true)?,
        AutostartCommand::Off => postvak_core::autostart::set(paths, false)?,
        AutostartCommand::Status => match postvak_core::autostart::is_on(paths)? {
            true => "on".to_string(),
            false => "off".to_string(),
        },
    };
    println!("{text}");
    Ok(())
}

fn cmd_service(command: ServiceCommand, paths: &Paths) -> Result<()> {
    let text = match command {
        ServiceCommand::Install { dry_run } => {
            postvak_core::daemon::service::install(paths, dry_run)?
        }
        ServiceCommand::Remove { dry_run } => postvak_core::daemon::service::remove(dry_run)?,
    };
    println!("{text}");
    Ok(())
}

fn cmd_attachment(command: AttachmentCommand, config: &Config, paths: &Paths) -> Result<()> {
    let (uid, account, folder) = match &command {
        AttachmentCommand::List {
            uid,
            account,
            folder,
            ..
        }
        | AttachmentCommand::Save {
            uid,
            account,
            folder,
            ..
        } => (*uid, account.as_deref(), folder.as_str()),
    };
    let (acc, store, msg) = open_message(config, paths, account, folder, uid)?;
    let raw = LazyClient::new(paths).raw_message(&acc.name, &store, &msg)?;
    match command {
        AttachmentCommand::List { json, .. } => {
            for a in postvak_core::message::attachments(&raw) {
                if json {
                    print_json(postvak_core::output::with_account(&acc.name, &a)?);
                } else {
                    println!(
                        "{}  {}  {}  {}",
                        a.index,
                        clean(&a.content_type, false),
                        a.size,
                        clean(a.name.as_deref().unwrap_or("-"), false)
                    );
                }
            }
        }
        AttachmentCommand::Save { n, dir, .. } => {
            let path = postvak_core::message::save_attachment(&raw, n, &dir)
                .with_context(|| format!("saving attachment {n}"))?;
            println!("{}", clean(&path.display().to_string(), false));
        }
    }
    Ok(())
}

/// A `--after` or `--before` value, read as `after:` and `before:` are in search.
fn parse_date(value: &str, now: i64) -> Result<i64> {
    postvak_core::search::date(value, now).with_context(|| {
        format!(
            "'{}' is not a date; use 2026-01-15, or 30d, 3w, 6mo or 1y",
            clean(value, false)
        )
    })
}

/// `account  count  unread  first..last  sender  rules`.
fn sender_line(account: &str, sender: &postvak_core::senders::Sender) -> String {
    format!(
        "{account}\t{}\t{}\t{}\t{}\t{}\t{}",
        sender.count,
        sender.unread,
        time::date_time(sender.first),
        time::date_time(sender.last),
        clean(
            if sender.sender.is_empty() {
                "(no address)"
            } else {
                &sender.sender
            },
            false
        ),
        clean(&sender.rules.join(", "), false)
    )
}

/// The account, its store and the stored message `folder/uid`.
fn open_message<'a>(
    config: &'a Config,
    paths: &Paths,
    account: Option<&str>,
    folder: &str,
    uid: u32,
) -> Result<(&'a AccountConfig, Store, Message)> {
    let acc = config::single(&config.accounts, account)?;
    let store = Store::open_account(paths, &acc.name)?;
    let msg = store
        .message(folder, uid)?
        .with_context(|| format!("no message {}/{uid}", clean(folder, false)))?;
    Ok((acc, store, msg))
}

/// One `--json` line, every string cleaned as in MCP results, so an agent reading it sees no hidden text.
fn print_json(value: serde_json::Value) {
    println!("{}", postvak_core::output::clean_value(value, false));
}

/// The stores of the accounts `name` selects, each with its identity when `identity`, for a page of `list` or
/// `search`.
fn sources(
    config: &Config,
    paths: &Paths,
    name: Option<&str>,
    identity: bool,
) -> Result<Vec<page::Source>> {
    config::select(&config.accounts, name)?
        .into_iter()
        .map(|acc| {
            Ok(page::Source {
                account: acc.name.clone(),
                store: Store::open_account(paths, &acc.name)?,
                identity: if identity {
                    Some(acc.identity()?)
                } else {
                    None
                },
            })
        })
        .collect()
}

/// A count of `list` or `search`: `{"count": N}` with `--json`, else the number.
fn print_count(count: u64, json: bool) {
    if json {
        print_json(postvak_core::output::count(count));
    } else {
        println!("{count}");
    }
}

/// Where the next page starts, when there is one: a last `{"next": …}` line with `--json`, else a line on stderr
/// so the rows on stdout stay one message each.
fn print_next(next: Option<&str>, json: bool) {
    let Some(next) = next else { return };
    if json {
        print_json(postvak_core::output::next(next));
    } else {
        eprintln!("more: --cursor {next}");
    }
}

/// `account  * INBOX/42  date  from  subject`; `*` marks unread, `depth` indents the subject in thread views.
fn message_line(account: &str, m: &Message, depth: usize) -> String {
    format!(
        "{account}  {} {:<14}  {}  {:<30}  {}{}",
        if m.is_seen() { " " } else { "*" },
        clean(&format!("{}/{}", m.folder, m.uid), false),
        time::date_time(m.internaldate),
        truncate(&clean(m.from_addr.as_deref().unwrap_or(""), false), 30),
        "  ".repeat(depth),
        clean(m.subject.as_deref().unwrap_or(""), false)
    )
}

fn truncate(s: &str, width: usize) -> String {
    let mut out: String = s.chars().take(width).collect();
    if s.chars().count() > width {
        out.pop();
        out.push('…');
    }
    out
}

/// Finder and the Dock start Postvak.app with no arguments; that opens the window rather than printing help.
/// The process a crash report names.
fn process(command: &Command) -> &'static str {
    match command {
        Command::Gui { .. } => "gui",
        Command::Mcp { command: None, .. } => "mcp",
        Command::Run { .. } => "daemon",
        _ => "cli",
    }
}

fn cmd_crashes(command: Option<CrashesCommand>, paths: &Paths) -> Result<()> {
    let dir = paths.crash_dir();
    match command {
        None => {
            let ids = postvak_core::crash::list(&dir)?;
            if ids.is_empty() {
                eprintln!("no crash reports in {}", dir.display());
            }
            for id in ids {
                println!("{id}");
            }
        }
        Some(CrashesCommand::Show { id }) => print!("{}", postvak_core::crash::show(&dir, &id)?),
    }
    Ok(())
}

fn opened_as_mac_app() -> bool {
    use std::io::IsTerminal as _;
    cfg!(target_os = "macos")
        && std::env::current_exe().is_ok_and(|exe| {
            let args: Vec<_> = std::env::args_os().skip(1).collect();
            postvak_core::paths::launched_as_app(&exe, &args, io::stdin().is_terminal())
        })
}

#[cfg(feature = "gui")]
fn cmd_gui(
    paths: Result<Paths>,
    safe_mode: bool,
    show: Option<postvak_core::notify::Show>,
    background: bool,
) -> Result<()> {
    postvak_gui::run(paths, safe_mode, show, background)
}

#[cfg(not(feature = "gui"))]
fn cmd_gui(
    _paths: Result<Paths>,
    _safe_mode: bool,
    _show: Option<postvak_core::notify::Show>,
    _background: bool,
) -> Result<()> {
    bail!("this postvak was built without the GUI; install it with the default features")
}

#[cfg(feature = "mcp")]
fn cmd_mcp(
    config: &Config,
    paths: &Paths,
    scopes: &str,
    accounts: &[String],
    save_dir: Option<&Path>,
    command: Option<McpCommand>,
) -> Result<()> {
    match command {
        None => postvak_mcp::run(config, paths, scopes, accounts, save_dir),
        Some(McpCommand::Install {
            target,
            scopes,
            account,
            save_dir,
            remove,
            dry_run,
        }) => {
            use postvak_mcp::install::{Target, install};
            if let Some(name) = account.iter().find(|name| config.account(name).is_none()) {
                bail!("no account named '{name}'");
            }
            let target = match target {
                InstallTarget::ClaudeCode => Target::ClaudeCode,
                InstallTarget::ClaudeDesktop => Target::ClaudeDesktop,
                InstallTarget::Json => Target::Json,
            };
            let (stdout, hint) = install(
                target,
                &scopes,
                &account,
                save_dir.as_deref(),
                remove,
                dry_run,
            )?;
            print!("{stdout}");
            if let Some(hint) = hint {
                eprint!("{hint}");
            }
            Ok(())
        }
    }
}

#[cfg(not(feature = "mcp"))]
fn cmd_mcp(
    _config: &Config,
    _paths: &Paths,
    _scopes: &str,
    _accounts: &[String],
    _save_dir: Option<&Path>,
    _command: Option<McpCommand>,
) -> Result<()> {
    bail!("this postvak was built without the MCP server; install it with the default features")
}

/// Starts even without accounts: a login service would otherwise restart it in a loop, and accounts added to
/// config.toml later start on their own.
fn cmd_run(paths: &Paths, idle_exit: Option<u64>) -> Result<()> {
    // Test hook: lets tests idle an auto-started daemon out in seconds.
    let override_secs = std::env::var("POSTVAK_IDLE_EXIT_SECS")
        .ok()
        .and_then(|secs| secs.parse().ok());
    let options = match idle_exit.map(|secs| override_secs.unwrap_or(secs)) {
        Some(secs) => postvak_core::daemon::Options::auto_started(Duration::from_secs(secs)),
        None => postvak_core::daemon::Options::foreground(),
    };
    postvak_core::daemon::run(paths, options)
}

fn cmd_rules(command: RulesCommand, config: &Config, paths: &Paths) -> Result<()> {
    match command {
        RulesCommand::Check => {
            println!("{} rules ok", compiled_rules(paths, None)?.len());
            for warning in rules_warnings(paths, config)? {
                eprintln!("warning: {}", clean(&warning, false));
            }
            Ok(())
        }
        RulesCommand::List { json } => {
            let file = postvak_core::rules::load(&paths.rules_file())?;
            for (position, r) in file.rules.iter().enumerate() {
                if json {
                    print_json(postvak_core::output::rule(r, position)?);
                } else {
                    let edits = r
                        .replaces
                        .as_deref()
                        .map(|target| format!(" edits {}", clean(target, false)))
                        .unwrap_or_default();
                    println!(
                        "{}\t{}\t{}{edits}",
                        if r.enabled { "on " } else { "off" },
                        clean(&r.name, false),
                        clean(r.proposed_by.as_deref().unwrap_or(""), false)
                    );
                }
            }
            Ok(())
        }
        RulesCommand::Coverage {
            account,
            folder,
            include_pending,
            group_by,
            limit,
            json,
        } => {
            let rules = postvak_core::rules::coverage::rules(
                &postvak_core::rules::load(&paths.rules_file())?,
                include_pending,
            )?;
            for acc in config::select(&config.accounts, account.as_deref())? {
                let store = Store::open_account(paths, &acc.name)?;
                let identity = acc.identity()?;
                let ctx = postvak_core::rules::coverage::context(&acc.name, &identity, time::now());
                let mut coverage = postvak_core::rules::coverage::in_folder(
                    &store,
                    &rules,
                    &ctx,
                    &folder,
                    group_by.into(),
                )?;
                coverage.unmatched_senders.truncate(limit as usize);
                coverage.contests.truncate(limit as usize);
                if json {
                    print_json(postvak_core::output::coverage(
                        &acc.name, &folder, &coverage,
                    )?);
                } else {
                    print_coverage(&acc.name, &folder, &coverage);
                }
            }
            Ok(())
        }
        RulesCommand::ApplyExisting {
            name,
            account,
            dry_run,
            yes,
        } => {
            let rules = vec![postvak_core::rules::enabled_rule(
                &paths.rules_file(),
                &name,
            )?];
            let accounts: Vec<&AccountConfig> =
                config::select(&config.accounts, account.as_deref())?
                    .into_iter()
                    .filter(|acc| rules.iter().any(|r| r.applies_to_account(&acc.name)))
                    .collect();
            if dry_run {
                for acc in accounts {
                    let store = Store::open_account(paths, &acc.name)?;
                    print_planned_actions(&rules, &store, acc, &acc.identity()?)?;
                }
                return Ok(());
            }
            if !yes {
                confirm_expunges(&rules, paths, &accounts)?;
            }
            let mut failed = false;
            let daemon = LazyClient::new(paths);
            for acc in accounts {
                let (evaluated, actions, errors) = daemon.apply_rule(&acc.name, &name)?;
                failed |= !errors.is_empty();
                for message in &errors {
                    postvak_core::daemon::report_error(&acc.name, message);
                }
                println!("{}: {actions} actions on {evaluated} messages", acc.name);
            }
            if failed {
                bail!("some actions failed; see the errors above");
            }
            Ok(())
        }
        RulesCommand::Test {
            name,
            account,
            stdin,
        } => {
            let rules = if stdin {
                let mut rule = read_rule_json()?;
                rule.enabled = true;
                postvak_core::rules::compile(&RuleFile { rules: vec![rule] })?
            } else {
                let mut rules = compiled_rules(paths, name.as_deref())?;
                if name.is_some() {
                    rules.iter_mut().for_each(|r| r.rule.enabled = true);
                }
                rules
            };
            for acc in config::select(&config.accounts, account.as_deref())? {
                let store = Store::open_account(paths, &acc.name)?;
                print_planned_actions(&rules, &store, acc, &acc.identity()?)?;
            }
            Ok(())
        }
        RulesCommand::Schema => {
            print!("{}", postvak_core::rules::schema());
            Ok(())
        }
        RulesCommand::Propose {
            by,
            before,
            after,
            replace,
            edit,
        } => {
            let value: serde_json::Value = serde_json::from_reader(io::stdin().lock())
                .context("reading rules as JSON from stdin")?;
            let mut proposals = postvak_core::rules::edit::read_proposals(value)?;
            let place = match (before, after) {
                (Some(name), _) => Some(Place::Before(name)),
                (None, Some(name)) => Some(Place::After(name)),
                (None, None) => None,
            };
            if let Some(place) = place {
                if proposals.place != Place::End {
                    bail!("give the place either in the JSON or with --before/--after, not both");
                }
                proposals.place = place;
            }
            proposals.replace |= replace;
            proposals.edit |= edit;
            if proposals.edit && proposals.place != Place::End {
                bail!("an edit keeps its rule's place: give --edit without --before or --after");
            }
            let names: Vec<String> = proposals
                .rules
                .iter()
                .map(|r| {
                    if proposals.edit {
                        postvak_core::rules::edit::edit_name(&r.name)
                    } else {
                        r.name.clone()
                    }
                })
                .map(|name| format!("'{}'", clean(&name, false)))
                .collect();
            postvak_core::rules::edit::propose_all(&paths.rules_file(), proposals, &cli_by(by))?;
            println!(
                "proposed {}; disabled until a human runs `postvak rules approve`",
                names.join(", ")
            );
            Ok(())
        }
        RulesCommand::Withdraw { name, by } => {
            postvak_core::rules::edit::withdraw(&paths.rules_file(), &name, &cli_by(by))?;
            println!("withdrew proposal '{}'", clean(&name, false));
            Ok(())
        }
        RulesCommand::Approve { name } => {
            let approved = postvak_core::rules::edit::approve_from_now(paths, &name, time::now())?;
            if approved == name {
                println!(
                    "enabled '{}'; it acts on mail that arrives from now on",
                    clean(&name, false)
                );
            } else {
                println!(
                    "changed '{}' as '{}' proposed; it acts as changed on mail that arrives from now on",
                    clean(&approved, false),
                    clean(&name, false)
                );
            }
            Ok(())
        }
        RulesCommand::Reject { name } => {
            postvak_core::rules::edit::reject(&paths.rules_file(), &name)?;
            println!("removed proposal '{}'", clean(&name, false));
            Ok(())
        }
    }
}

fn print_coverage(account: &str, folder: &str, coverage: &postvak_core::rules::coverage::Coverage) {
    println!(
        "{account} {}: {} messages, {} matched, {} unmatched, {} contested",
        clean(folder, false),
        coverage.messages,
        coverage.matched,
        coverage.unmatched,
        coverage.contested
    );
    if !coverage.unmatched_senders.is_empty() {
        println!("unmatched mail by sender:");
        for sender in &coverage.unmatched_senders {
            println!("  {}", sender_line(account, sender));
        }
    }
    if !coverage.contests.is_empty() {
        println!("contested:");
    }
    for contest in &coverage.contests {
        let m = &contest.message;
        println!(
            "  {}/{}\t{}",
            clean(&m.folder, false),
            m.uid,
            clean(m.subject.as_deref().unwrap_or(""), false)
        );
        println!("    match: {}", clean(&contest.rules.join(", "), false));
        let acts: Vec<String> = contest
            .acts
            .iter()
            .map(|act| format!("{} {}", act.rule, act.action))
            .collect();
        match &contest.fails {
            Some(why) => println!("    acts:  nothing ({why})"),
            None => println!("    acts:  {}", clean(&acts.join(", "), false)),
        }
    }
}

/// What `--by WHO` records as proposed_by: `cli:WHO`, or `cli` alone.
fn cli_by(by: Option<String>) -> String {
    by.map_or_else(|| "cli".to_string(), |who| format!("cli:{who}"))
}

fn read_rule_json() -> Result<Rule> {
    serde_json::from_reader(io::stdin().lock()).context("reading one rule as JSON from stdin")
}

/// Compiles rules with `first_seen_at` left at 0 and without touching any store, so previews are
/// side-effect free and cover mail that predates the rule. A `name` that matches no rule is an error.
/// What `rules check` warns about after the rules compiled.
fn rules_warnings(paths: &Paths, config: &Config) -> Result<Vec<String>> {
    let path = paths.rules_file();
    let accounts: Vec<&str> = config.accounts.iter().map(|a| a.name.as_str()).collect();
    let untrusting: Vec<&str> = config
        .accounts
        .iter()
        .filter(|a| !a.can_authenticate())
        .map(|a| a.name.as_str())
        .collect();
    let rules = postvak_core::rules::load(&path)?.rules;
    let warnings = postvak_core::rules::warnings(&path, &rules, &accounts);
    let synced = postvak_core::rules::account_folders(paths, &accounts);
    let authenticated = postvak_core::rules::authenticated_warnings(&rules, &untrusting, &synced);
    let watched = postvak_core::rules::watched_folder_warnings(&rules, &synced);
    let folders = postvak_core::rules::folder_warnings(paths, &rules, &accounts);
    Ok([warnings, authenticated, watched, folders].concat())
}

fn compiled_rules(paths: &Paths, name: Option<&str>) -> Result<Vec<CompiledRule>> {
    let file = postvak_core::rules::load(&paths.rules_file())?;
    let compiled: Vec<CompiledRule> = postvak_core::rules::compile(&file)?
        .into_iter()
        .filter(|r| name.is_none_or(|n| n == r.rule.name))
        .collect();
    if let Some(name) = name
        && compiled.is_empty()
    {
        bail!("no rule named '{name}'");
    }
    Ok(compiled)
}

/// Asks before a rule expunges existing mail, since the server copy is gone for good; refuses without a terminal.
fn confirm_expunges(
    rules: &[CompiledRule],
    paths: &Paths,
    accounts: &[&AccountConfig],
) -> Result<()> {
    use std::io::IsTerminal as _;
    let mut any = false;
    for acc in accounts {
        let store = Store::open_account(paths, &acc.name)?;
        let warning = postvak_core::actions::expunge_warning(
            rules,
            &store,
            acc,
            &acc.identity()?,
            time::now(),
        )?;
        if let Some(warning) = warning {
            any = true;
            eprintln!("{warning}");
        }
    }
    if !any {
        return Ok(());
    }
    if !io::stdin().is_terminal() {
        bail!(
            "refusing to delete without confirmation; check with --dry-run, then rerun with --yes"
        );
    }
    if prompt("Type yes to continue")? != "yes" {
        bail!("cancelled");
    }
    Ok(())
}

fn print_planned_actions(
    rules: &[CompiledRule],
    store: &Store,
    account: &AccountConfig,
    identity: &Identity,
) -> Result<()> {
    for p in postvak_core::actions::planned(rules, store, account, identity, time::now())? {
        println!(
            "{}\t{}/{}\t{}\t{}",
            clean(&p.rule, false),
            clean(&p.message.folder, false),
            p.message.uid,
            clean(&p.action.label(), false),
            clean(p.message.subject.as_deref().unwrap_or(""), false)
        );
    }
    Ok(())
}

fn cmd_trash(command: TrashCommand, config: &Config, paths: &Paths) -> Result<()> {
    match command {
        TrashCommand::List { account } => {
            for acc in config::select(&config.accounts, account.as_deref())? {
                for e in Trash::new(paths.trash_dir(&acc.name)).list()? {
                    let subject = e.headers().subject.unwrap_or_default();
                    println!(
                        "{}  {}  {}/{}  {}  {}",
                        acc.name,
                        time::date_time(e.saved_at),
                        clean(&e.folder, false),
                        e.uid,
                        clean(&subject, false),
                        clean(&e.path.display().to_string(), false)
                    );
                }
            }
            Ok(())
        }
        TrashCommand::Restore { file, account } => {
            let acc = config::single(&config.accounts, account.as_deref())?;
            let folder = LazyClient::new(paths).restore(&acc.name, std::path::absolute(&file)?)?;
            println!(
                "restored to {}; rules leave restored mail alone",
                clean(&folder, false)
            );
            Ok(())
        }
        TrashCommand::Purge { account } => {
            for acc in config::select(&config.accounts, account.as_deref())? {
                let removed = Trash::new(paths.trash_dir(&acc.name))
                    .purge(acc.trash_retention_days, time::now())?;
                println!("{}: removed {removed}", acc.name);
            }
            Ok(())
        }
    }
}

fn cmd_account_add(mut config: Config, paths: &Paths) -> Result<()> {
    let name = prompt("Account name (letters, digits, - _)")?;
    let host = prompt("IMAP host")?;
    let port: u16 = prompt("Port [993]")?.parse().unwrap_or(993);
    let tls = match prompt("Encryption: (i)mplicit TLS, (s)TARTTLS or (n)one [by port: STARTTLS on 143, else implicit]")?
        .to_ascii_lowercase()
        .chars()
        .next()
    {
        None => None,
        Some('i') => Some(Tls::Implicit),
        Some('s') => Some(Tls::Starttls),
        Some('n') => Some(Tls::None),
        Some(_) => bail!("answer i, s or n, or nothing for the port's default"),
    };
    let ca_file = {
        let answer = prompt("Extra trusted CA file (PEM, absolute path) [none]")?;
        if answer.is_empty() {
            None
        } else {
            Some(PathBuf::from(answer))
        }
    };
    let username = prompt("Username")?;
    let address = {
        let a = prompt(&format!("Email address [{username}]"))?;
        if a.is_empty() { None } else { Some(a) }
    };
    let storage = prompt("Password storage: (k)eyring or (c)ommand [k]")?;
    let password = if storage.starts_with('c') {
        PasswordSource::Command {
            command: prompt("Password command")?,
        }
    } else {
        PasswordSource::Keyring { keyring: true }
    };
    let account = AccountConfig {
        port,
        address,
        ca_file,
        tls,
        ..AccountConfig::new(&name, &host, &username, password)
    };
    account::check(&config, &account)?;
    let secret = match &account.password {
        PasswordSource::Command { .. } => None,
        PasswordSource::Keyring { .. } => {
            Some(Secret::new(rpassword::prompt_password("Password: ")?))
        }
    };
    let client = Client::connect_or_start(paths)?;
    println!("testing the login to {host}…");
    account::test_login(&client, paths, &config, &account, secret.as_ref())
        .context("the login failed, so nothing was saved")?;
    account::add(&mut config, paths, account, secret)?;
    println!("saved to {}", paths.config_file().display());
    account::await_started(&client, &name, account::START_TIMEOUT)?;
    if !sync_account(&client, &name) {
        bail!(
            "the first sync of '{name}' failed; check the login and run `postvak account add` again"
        );
    }
    if client.status()?.lacks_uidplus(&name) {
        println!("note: {}", postvak_core::help::NO_UIDPLUS);
    }
    Ok(())
}

fn prompt(label: &str) -> Result<String> {
    print!("{label}: ");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_reference_is_current() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/docs/src/cli.md");
        let generated = clap_markdown::help_markdown::<Cli>();
        if std::env::var_os("POSTVAK_BLESS").is_some() {
            std::fs::write(path, &generated).unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            generated,
            "docs/src/cli.md is stale; run POSTVAK_BLESS=1 cargo test"
        );
    }

    #[test]
    fn unsubscribe_line_says_what_happened_or_what_to_open() {
        use postvak_core::unsubscribe::Report;
        let line = |method, done| {
            unsubscribe_line(&Report {
                method,
                url: "https://list.example/u".into(),
                done,
            })
        };
        assert_eq!(
            line("one_click", true),
            "unsubscribed: the sender's server accepted it"
        );
        assert_eq!(
            line("one_click", false),
            "would unsubscribe by one-click POST to https://list.example/u"
        );
        assert_eq!(
            line("link", false),
            "open this page to unsubscribe: https://list.example/u"
        );
        assert!(line("mail", false).starts_with("send a mail to unsubscribe: "));
    }
}
