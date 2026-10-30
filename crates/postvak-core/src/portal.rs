//! XDG desktop portals (`org.freedesktop.portal.*`) on the session bus: the desktop's own dialogs and permissions,
//! which also reach out of a Flatpak sandbox. A request answers later with a `Response` signal, after the user chose.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{DynamicType, OwnedValue, Value};

const DESTINATION: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";

/// Makes each request's token, and so its answer's path, this process's own.
static REQUESTS: AtomicU32 = AtomicU32::new(0);

/// A portal's answer: its results, or None when the user cancelled.
pub type Answer = Option<HashMap<String, OwnedValue>>;

/// Calls `method` on the portal `interface` (`org.freedesktop.portal.<interface>`) and waits for the user's answer.
/// `body` gets the request's `handle_token` to put in its options. Blocks until the user answers, so not on the
/// window's thread.
pub fn request<B>(interface: &str, method: &str, body: impl FnOnce(&str) -> B) -> Result<Answer>
where
    B: Serialize + DynamicType,
{
    let bus = Connection::session().context("no session bus for the desktop portal")?;
    let sender = bus
        .unique_name()
        .context("the session bus gave no name")?
        .to_string();
    let token = format!(
        "postvak{}_{}",
        std::process::id(),
        REQUESTS.fetch_add(1, Ordering::Relaxed)
    );
    // Listening before the call, on the path the portal will answer on, is the portal's way to never miss the answer.
    let request = Proxy::new(
        &bus,
        DESTINATION,
        request_path(&sender, &token),
        "org.freedesktop.portal.Request",
    )?;
    let mut answers = request.receive_signal("Response")?;
    bus.call_method(
        Some(DESTINATION),
        PATH,
        Some(format!("org.freedesktop.portal.{interface}").as_str()),
        method,
        &body(&token),
    )
    .with_context(|| format!("asking the {interface} portal"))?;
    let answer = answers
        .next()
        .with_context(|| format!("the {interface} portal did not answer"))?;
    let (code, results): (u32, HashMap<String, OwnedValue>) = answer.body().deserialize()?;
    answered(code, results)
}

/// The path a portal request with `token` answers on: the sender's unique name without its `:`, dots as underscores.
fn request_path(sender: &str, token: &str) -> String {
    let sender = sender.trim_start_matches(':').replace('.', "_");
    format!("{PATH}/request/{sender}/{token}")
}

fn answered(code: u32, results: HashMap<String, OwnedValue>) -> Result<Answer> {
    match code {
        0 => Ok(Some(results)),
        1 => Ok(None),
        _ => bail!("the desktop portal refused the request"),
    }
}

/// The desktop's save dialog (`FileChooser.SaveFile`), starting in `folder` with `name` filled in; the path chosen, or
/// None when cancelled. In a Flatpak the path is the document portal's, which only the chosen file is visible through.
pub fn save_file(
    title: &str,
    name: &str,
    folder: Option<&std::path::Path>,
) -> Result<Option<PathBuf>> {
    let answer = request("FileChooser", "SaveFile", |token| {
        let mut options: HashMap<&str, Value> = HashMap::from([
            ("handle_token", Value::from(token.to_string())),
            ("modal", Value::from(true)),
            ("current_name", Value::from(name.to_string())),
        ]);
        if let Some(folder) = folder {
            options.insert("current_folder", Value::from(path_bytes(folder)));
        }
        ("", title.to_string(), options)
    })?;
    let Some(results) = answer else {
        return Ok(None);
    };
    let uri = results
        .get("uris")
        .and_then(|uris| <Vec<String>>::try_from(uris.try_clone().ok()?).ok())
        .and_then(|uris| uris.into_iter().next())
        .context("the save dialog chose no file")?;
    file_path(&uri).map(Some)
}

/// Runs `exe` with `args` in a new sandbox of this Flatpak app (`org.freedesktop.portal.Flatpak.Spawn`), with
/// `envs` added and stdout and stderr to `output`. Returns once it started: the new sandbox is not this process's
/// child and lives on after this one ends.
pub fn spawn_sandboxed(
    exe: &std::path::Path,
    args: &[&str],
    envs: HashMap<String, String>,
    output: &std::fs::File,
) -> Result<()> {
    use std::os::fd::AsFd;
    use zbus::zvariant::Fd;

    let null = std::fs::File::open("/dev/null").context("opening /dev/null")?;
    let fds: HashMap<u32, Fd> = HashMap::from([
        (0, Fd::from(null.as_fd())),
        (1, Fd::from(output.as_fd())),
        (2, Fd::from(output.as_fd())),
    ]);
    let options: HashMap<&str, Value> = HashMap::new();
    let flags = 0u32;
    let bus = Connection::session().context("no session bus for the Flatpak portal")?;
    bus.call_method(
        Some("org.freedesktop.portal.Flatpak"),
        "/org/freedesktop/portal/Flatpak",
        Some("org.freedesktop.portal.Flatpak"),
        "Spawn",
        &(
            path_bytes(std::path::Path::new("/")),
            spawn_argv(exe, args),
            fds,
            envs,
            flags,
            options,
        ),
    )
    .with_context(|| format!("starting {} in its own sandbox", exe.display()))?;
    Ok(())
}

/// The desktop's settings (`org.freedesktop.portal.Settings`) in each of `namespaces`, by namespace and key.
pub type Settings = HashMap<String, HashMap<String, OwnedValue>>;

/// Reads the settings in `namespaces`; a namespace the desktop does not have is left out.
pub fn settings(bus: &Connection, namespaces: &[&str]) -> Result<Settings> {
    let reply = bus
        .call_method(
            Some(DESTINATION),
            PATH,
            Some("org.freedesktop.portal.Settings"),
            "ReadAll",
            &(namespaces,),
        )
        .context("reading the desktop's settings")?;
    Ok(reply.body().deserialize()?)
}

/// Calls `changed` with the settings in `namespaces` now, then again each time one of them changes. Blocks while the
/// session bus lasts, so on a thread of its own.
pub fn follow_settings(namespaces: &[&str], mut changed: impl FnMut(&Settings)) -> Result<()> {
    let bus = Connection::session().context("no session bus for the desktop's settings")?;
    let portal = Proxy::new(&bus, DESTINATION, PATH, "org.freedesktop.portal.Settings")?;
    // Listening before the first read, so a change in between is not missed.
    let changes = portal.receive_signal("SettingChanged")?;
    changed(&settings(&bus, namespaces)?);
    for change in changes {
        let (namespace, _key, _value): (String, String, OwnedValue) =
            change.body().deserialize()?;
        if namespaces.contains(&namespace.as_str()) {
            changed(&settings(&bus, namespaces)?);
        }
    }
    Ok(())
}

/// `exe` and `args` as `Spawn` takes them: each with a closing NUL.
fn spawn_argv(exe: &std::path::Path, args: &[&str]) -> Vec<Vec<u8>> {
    std::iter::once(path_bytes(exe))
        .chain(args.iter().map(|arg| path_bytes(std::path::Path::new(arg))))
        .collect()
}

/// A path as the portals take it: its bytes with a closing NUL.
fn path_bytes(path: &std::path::Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    let mut bytes = path.as_os_str().as_bytes().to_vec();
    bytes.push(0);
    bytes
}

/// The local path behind a `file://` URI the portal chose.
fn file_path(uri: &str) -> Result<PathBuf> {
    url::Url::parse(uri)
        .ok()
        .filter(|url| url.scheme() == "file")
        .and_then(|url| url.to_file_path().ok())
        .with_context(|| format!("the save dialog chose {uri}, which is not a local file"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_answer_comes_on_the_senders_request_path() {
        assert_eq!(
            request_path(":1.42", "postvak7_0"),
            "/org/freedesktop/portal/desktop/request/1_42/postvak7_0"
        );
    }

    #[test]
    fn a_cancelled_request_is_none_and_a_refused_one_an_error() {
        assert!(answered(0, HashMap::new()).unwrap().is_some());
        assert!(answered(1, HashMap::new()).unwrap().is_none());
        assert!(answered(2, HashMap::new()).is_err());
    }

    #[test]
    fn the_chosen_file_is_a_local_path_with_its_escapes_undone() {
        assert_eq!(
            file_path("file:///home/me/My%20Mail/report.pdf").unwrap(),
            PathBuf::from("/home/me/My Mail/report.pdf")
        );
        assert!(file_path("https://example.com/report.pdf").is_err());
    }

    #[test]
    fn a_path_goes_to_the_portal_with_a_closing_nul() {
        assert_eq!(path_bytes(std::path::Path::new("/home/me")), b"/home/me\0");
    }

    #[test]
    fn the_sandbox_runs_the_binary_with_its_arguments() {
        assert_eq!(
            spawn_argv(
                std::path::Path::new("/app/bin/postvak"),
                &["run", "--idle-exit", "60"]
            ),
            [
                &b"/app/bin/postvak\0"[..],
                b"run\0",
                b"--idle-exit\0",
                b"60\0"
            ]
        );
    }
}
