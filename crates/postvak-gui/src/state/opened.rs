//! The opened message: reading it, marking it read, its attachments, Save as… and unsubscribing.
use super::*;

impl App {
    /// A click on the row already shown opens it; `shown_at` stays, so a message long on screen is read at once.
    pub(super) fn arm_shown_body(&mut self, index: usize) {
        let Some((account, key)) = self.list.rows.get(index).map(Row::id) else {
            return;
        };
        if let Some(body) = &mut self.body
            && body.account == account
            && body.key == key
        {
            body.armed = true;
        }
    }

    /// Whether the body shows the cursor's message, or nothing when there is no row.
    pub(super) fn body_follows_cursor(&self) -> bool {
        self.list.rows.get(self.list.cursor).map(Row::id)
            == self.body.as_ref().map(|b| (b.account, b.key.clone()))
    }

    /// Loads the cursor's message when the cursor moved to another one; nothing while a search query is typed.
    pub(super) fn sync_body(&mut self, now: f64, typing_search: bool) {
        if typing_search {
            self.body = None;
            return;
        }
        let armed = std::mem::take(&mut self.pending_arm);
        if self.body_follows_cursor() {
            return;
        }
        self.waiting_bodies.clear();
        let current = self.list.rows.get(self.list.cursor).map(Row::id);
        self.body = current.map(|(account, key)| BodyState::chosen(account, key, now, armed));
        self.read_body(now);
        self.prefetch_neighbours();
    }

    /// Reads the shown message again after its body arrived; it shows as it was until the read answers.
    pub(super) fn reload_body(&mut self, account: usize, key: RowKey, now: f64) {
        if self
            .body
            .as_ref()
            .is_some_and(|b| b.account == account && b.key == key)
        {
            self.read_body(now);
        }
    }

    /// Starts reading the shown message on the reader thread, and shows it when the read answers within `body_budget`.
    pub(super) fn read_body(&mut self, now: f64) {
        let Some(body) = &mut self.body else {
            return;
        };
        let account = &self.accounts[body.account];
        if account.store.is_err() {
            body.reading = None;
            return;
        }
        self.body_generation += 1;
        let generation = self.body_generation;
        body.reading = Some(generation);
        let request = reader::Request {
            generation,
            account: account.name.clone(),
            key: body.key.clone(),
        };
        let reader = self.reader.get_or_insert_with(|| {
            Reader::start(self.paths.clone(), self.read_body, self.wake.clone())
        });
        reader.send(request);
        if let Some(read) = reader.wait(generation, self.body_budget) {
            self.show_read(read, now);
        }
    }

    /// Shows the read the shown message waits for; answers it no longer waits for are dropped.
    pub(super) fn receive_read(&mut self, now: f64) {
        let Some(reader) = &self.reader else {
            return;
        };
        for (generation, read) in reader.replies() {
            if self.body.as_ref().and_then(|b| b.reading) == Some(generation) {
                self.show_read(read, now);
            }
        }
    }

    /// Fills the shown message from what the store holds; asks the sync thread for a missing body once. The read delay
    /// restarts when the text first appears.
    pub(super) fn show_read(&mut self, read: Read, now: f64) {
        let Some(body) = &mut self.body else {
            return;
        };
        body.reading = None;
        let Read {
            message,
            mut attachments,
            mut html,
            has_raw,
            provenance,
        } = read;
        body.provenance = provenance;
        let preview = (message.is_some() && !has_raw)
            .then(|| {
                self.previews
                    .iter()
                    .find(|(key, _)| key.0 == body.account && key.1 == body.key)
            })
            .flatten();
        body.attachments_pending = preview.is_some_and(|(_, p)| p.whole);
        body.on_demand = preview.is_some_and(|(_, p)| !p.whole);
        if let Some((_, preview)) = preview {
            attachments = preview.attachments.clone();
            html = preview.html.clone();
        }
        let after_download = if has_raw {
            body.after_download.take()
        } else {
            None
        };
        let text = body_text(message.as_ref());
        if body.text.is_none() && text.is_some() {
            body.shown_at = now;
        }
        if body.brand.is_none() {
            body.brand = message.as_ref().and_then(|m| self.logos.brand(&m.headers));
            if let Some(brand) = &body.brand {
                self.logos.want(brand, &self.wake);
            }
        }
        body.links = body::links(text.as_deref());
        body.text = text;
        body.attachments = attachments;
        // The whole message brings the page its preview showed: keep the laid-out view and where it was scrolled.
        let same_page = html.as_ref().map(|h| &h.html) == body.html.as_ref().map(|h| &h.html);
        if !same_page {
            body.text_note = too_large(html.as_ref());
            body.html_view = None;
        }
        body.html = html;
        // An indexed message has its text but not its raw bytes, which the HTML view and the attachments need.
        let missing = message.is_some() && !has_raw;
        body.message = message;
        let (account, key) = (body.account, body.key.clone());
        if missing {
            self.request_body(account, &key);
        }
        if let Some(action) = after_download {
            self.apply_action(action);
        }
    }

    /// Asks the sync thread for a body once per message; while the account's last fetch runs, it waits its turn.
    pub(super) fn request_body(&mut self, account: usize, key: &RowKey) {
        let wanted = (account, key.clone());
        if self.requested.contains(&wanted) || self.waiting_bodies.contains(&wanted) {
            return;
        }
        if self.fetching_body.contains_key(&account) {
            self.waiting_bodies.push(wanted);
        } else {
            self.fetch_body(wanted);
        }
    }

    fn fetch_body(&mut self, (account, key): (usize, RowKey)) {
        self.requested.insert((account, key.clone()));
        self.fetching_body.insert(account, key.clone());
        let (folder, uid) = key;
        let command = Command::FetchBody {
            folder,
            uid,
            preview: true,
        };
        self.send(account, command);
    }

    /// Starts the account's next waiting fetch, the shown message's first.
    pub(super) fn body_fetch_ended(&mut self, account: usize) {
        self.fetching_body.remove(&account);
        let shown = self.body.as_ref().map(|b| (b.account, b.key.clone()));
        let next = self
            .waiting_bodies
            .iter()
            .position(|wanted| Some(wanted) == shown.as_ref())
            .or_else(|| self.waiting_bodies.iter().position(|(a, _)| *a == account));
        if let Some(next) = next {
            let wanted = self.waiting_bodies.remove(next);
            self.fetch_body(wanted);
        }
    }

    /// Asks for the missing bodies of the rows beside the cursor, each from its own account, so stepping to them
    /// shows text at once.
    pub(super) fn prefetch_neighbours(&mut self) {
        let cursor = self.list.cursor;
        let missing: Vec<(usize, RowKey)> = [cursor.checked_sub(1), Some(cursor + 1)]
            .into_iter()
            .flatten()
            .filter_map(|index| self.list.rows.get(index))
            .filter(|row| match &self.accounts[row.account].store {
                Ok(store) => matches!(store.has_raw(&row.folder, row.uid), Ok(false)),
                Err(_) => false,
            })
            .map(Row::id)
            .collect();
        for (account, key) in missing {
            self.request_body(account, &key);
        }
    }

    /// Marks the shown message read once the user opened it and its text has been on screen for `READ_DELAY`.
    pub(super) fn mark_read_after_delay(&mut self, now: f64) {
        // The selection hides the message.
        if self.selected_rows().len() > 1 {
            return;
        }
        let Some(body) = &mut self.body else { return };
        if !body.armed {
            return;
        }
        // The delay counts from when the text appears (`reload_body`), not from "Loading…".
        if body.message.as_ref().is_some_and(|m| m.body_text.is_none()) {
            return;
        }
        let account = &self.accounts[body.account];
        let last_edit = account.pending.get(&body.key).and_then(|edits| {
            edits.iter().rev().find_map(|edit| match edit {
                Optimistic::Seen(seen) => Some(*seen),
                _ => None,
            })
        });
        let seen = last_edit.unwrap_or_else(|| body.message.as_ref().is_none_or(Message::is_seen));
        if seen || body.read_sent {
            return;
        }
        let waited = now - body.shown_at;
        if waited < READ_DELAY {
            let at = body.shown_at + READ_DELAY;
            self.request_tick(at);
            return;
        }
        let (index, (folder, uid)) = (body.account, body.key.clone());
        if let Some(body) = &mut self.body {
            body.read_sent = true;
        }
        self.send_apply(
            index,
            folder,
            vec![uid],
            Action::MarkRead,
            Optimistic::Seen(true),
        );
        self.rebuild_rows();
    }

    /// The shown message's raw bytes from the store, or `None` while it is not downloaded.
    pub(super) fn shown_raw(&self) -> Option<Vec<u8>> {
        let body = self.body.as_ref()?;
        let store = self.accounts[body.account].store.as_ref().ok()?;
        store.raw(&body.key.0, body.key.1).ok().flatten()
    }

    /// Whether the shown message is one too large to download unasked, and is not stored whole yet.
    pub(super) fn shown_on_demand(&self) -> bool {
        let Some(body) = &self.body else { return false };
        body.on_demand
            && match &self.accounts[body.account].store {
                Ok(store) => matches!(store.has_raw(&body.key.0, body.key.1), Ok(false)),
                Err(_) => false,
            }
    }

    /// Asks the daemon for the whole shown message, and runs `action` once it is stored.
    pub(super) fn download_whole(&mut self, action: UiAction) {
        let Some(body) = &mut self.body else { return };
        let (account, (folder, uid)) = (body.account, body.key.clone());
        body.after_download = Some(action);
        body.saved = Some("Downloading the message…".into());
        let command = Command::FetchBody {
            folder,
            uid,
            preview: false,
        };
        self.send(account, command);
    }

    /// Asks the daemon for attachment `index` of the shown message on its own; it is saved when it comes.
    pub(super) fn fetch_attachment(&mut self, index: usize, save_as: bool) {
        let Some(body) = &mut self.body else { return };
        let asked = |f: &Fetching| {
            f.account == body.account
                && f.key == body.key
                && f.index == index
                && f.save_as == save_as
        };
        // A second click while it downloads would save it twice.
        if self.fetching.iter().any(asked) {
            return;
        }
        let name = body
            .attachments
            .iter()
            .find(|a| a.index == index)
            .and_then(|a| a.name.as_deref())
            .map(|name| clean(name, false))
            .unwrap_or_else(|| format!("attachment {index}"));
        body.saved = Some(format!("Downloading {name}…"));
        let (account, key) = (body.account, body.key.clone());
        self.fetching.push(Fetching {
            account,
            key: key.clone(),
            index,
            save_as,
        });
        let (folder, uid) = key;
        self.send(account, Command::FetchAttachment { folder, uid, index });
    }

    /// Saves an attachment that came on its own as each asker wanted it: Save into Downloads, and Save as… through the
    /// dialog while its message still shows.
    pub(super) fn attachment_fetched(
        &mut self,
        account: usize,
        key: RowKey,
        index: usize,
        name: Option<String>,
        contents: Vec<u8>,
    ) {
        let (wanted, rest): (Vec<Fetching>, Vec<Fetching>) = std::mem::take(&mut self.fetching)
            .into_iter()
            .partition(|f| f.account == account && f.key == key && f.index == index);
        self.fetching = rest;
        let shown = self
            .body
            .as_ref()
            .is_some_and(|b| b.account == account && b.key == key);
        for fetch in wanted {
            if fetch.save_as {
                if shown {
                    let file = message::attachment_file_name(name.as_deref(), index);
                    self.save_as(Some(Ok((file, contents.clone()))));
                }
                continue;
            }
            let saved = message::save_fetched_attachment(
                name.as_deref(),
                index,
                &contents,
                &self.downloads,
            );
            if shown && let Some(body) = &mut self.body {
                body.saved = Some(saved_note(Some(saved)));
            }
        }
    }

    pub(super) fn save_attachment(&mut self, index: usize) {
        if self.shown_on_demand() {
            self.fetch_attachment(index, false);
            return;
        }
        let raw = self.shown_raw();
        let note =
            saved_note(raw.map(|raw| message::save_attachment(&raw, index, &self.downloads)));
        if let Some(body) = &mut self.body {
            body.saved = Some(note);
        }
    }

    pub(super) fn save_eml(&mut self) {
        let raw = self.shown_raw();
        let Some(body) = &mut self.body else { return };
        let subject = body.message.as_ref().and_then(|m| m.subject.as_deref());
        body.saved =
            Some(saved_note(raw.map(|raw| {
                message::save_eml(&raw, subject, &self.downloads)
            })));
    }

    /// Opens the save dialog for `file` (its suggested name and contents), starting in Downloads. One at a time.
    pub(super) fn save_as(&mut self, file: Option<std::io::Result<(String, Vec<u8>)>>) {
        if self.saving_as.is_some() {
            return;
        }
        let Some(body) = &mut self.body else { return };
        let (name, bytes) = match file {
            Some(Ok(file)) => file,
            failed => {
                body.saved = Some(saved_note(failed.map(|f| f.map(|_| PathBuf::new()))));
                return;
            }
        };
        let (account, key) = (body.account, body.key.clone());
        let answer = (self.ask_save_path)(&name, &self.downloads, self.wake.clone());
        self.saving_as = Some(SavingAs {
            account,
            key,
            bytes,
            answer,
        });
    }

    /// Writes the file once the dialog picked where; a cancelled dialog saves nothing and says nothing.
    pub(super) fn receive_save_as(&mut self) {
        let Some(saving) = &self.saving_as else {
            return;
        };
        let note = match worker::poll(&saving.answer) {
            None => return,
            Some(Ok(Ok(None))) => None,
            Some(Ok(Ok(Some(path)))) => Some(saved_note(Some(
                message::write_chosen(&path, &saving.bytes).map(|()| path),
            ))),
            Some(Ok(Err(e))) => Some(format!("Could not save: {e:#}")),
            Some(Err(Stopped)) => Some("Could not save: the save dialog stopped".into()),
        };
        if let Some(note) = note
            && let Some(body) = &mut self.body
            && body.account == saving.account
            && body.key == saving.key
        {
            body.saved = Some(note);
        }
        self.saving_as = None;
    }

    pub(super) fn unsubscribe(&mut self) {
        let Some(body) = &mut self.body else { return };
        let method = body
            .message
            .as_ref()
            .and_then(|m| unsubscribe::method(&m.headers));
        match method {
            Some(Method::OneClick(url)) => {
                let post = self.post_unsubscribe;
                let answer = worker::oneshot("unsubscribe", self.wake.clone(), move || post(&url));
                body.saved = Some("Unsubscribing…".into());
                self.unsubscribing = Some(Unsubscribing {
                    account: body.account,
                    key: body.key.clone(),
                    answer,
                });
            }
            Some(Method::Link(url) | Method::Mail(url)) => {
                self.apply_action(UiAction::OpenUrl(url))
            }
            None => {}
        }
    }

    /// Shows or hides Why?: each condition of the rule that last acted on the shown message, checked against it now.
    pub(super) fn toggle_why(&mut self) {
        let Some(body) = &self.body else { return };
        if body.why.is_some() {
            if let Some(body) = &mut self.body {
                body.why = None;
            }
            return;
        }
        let found = (|| {
            let entry = body.provenance.first()?;
            let rule = self
                .rules
                .file
                .rules
                .iter()
                .find(|r| r.name == entry.rule_name)?;
            let message = body.message.as_ref()?;
            let account = &self.accounts[body.account].name;
            let config = Config::load(&self.paths.config_file()).ok()?;
            let identity = config.account(account)?.identity().ok()?;
            let ctx = rule_file::coverage::context(account, &identity, postvak_core::time::now());
            let lines = rule_file::engine::explain(rule, &self.rules.file.lists, message, &ctx)
                .into_iter()
                .map(|(single, met)| (draft::match_labels(&single).join(", "), met))
                .collect();
            Some(lines)
        })();
        if let Some(body) = &mut self.body {
            body.why = Some(found.unwrap_or_default());
        }
    }

    /// Takes the answer to a one-click unsubscribe; the note goes on its message while that still shows.
    pub(super) fn receive_unsubscribe(&mut self) {
        let Some(Unsubscribing {
            account,
            key,
            answer,
        }) = &self.unsubscribing
        else {
            return;
        };
        let (note, accepted) = match worker::poll(answer) {
            None => return,
            Some(Ok(Ok(()))) => (
                "Unsubscribed: the sender's server accepted it.".to_string(),
                true,
            ),
            Some(Ok(Err(e))) => (format!("Could not unsubscribe: {e}"), false),
            Some(Err(Stopped)) => (
                "Could not unsubscribe: the request stopped".to_string(),
                false,
            ),
        };
        if let Some(body) = &mut self.body
            && body.account == *account
            && body.key == *key
        {
            body.saved = Some(note.clone());
            body.unsubscribed = accepted;
        }
        self.push_history(note);
        self.unsubscribing = None;
    }

    /// Only the shown message, where a move of its row would take its whole thread.
    pub(super) fn move_back(&mut self) {
        let Some(target) = body::pane(self).provenance.and_then(|p| p.move_back) else {
            return;
        };
        let Some(body) = &self.body else { return };
        let (account, (folder, uid)) = (body.account, body.key.clone());
        self.pending_arm = true;
        self.send_apply(
            account,
            folder,
            vec![uid],
            Action::Move(target),
            Optimistic::Hidden,
        );
        self.rebuild_rows();
    }

    /// Only the shown message, where Delete would take its whole thread.
    pub(super) fn trash_unsubscribed(&mut self) {
        let Some(body) = &self.body else { return };
        if !body.unsubscribed {
            return;
        }
        let (account, (folder, uid)) = (body.account, body.key.clone());
        self.pending_arm = true;
        self.send_apply(
            account,
            folder,
            vec![uid],
            Action::Trash,
            Optimistic::Hidden,
        );
        self.rebuild_rows();
    }

    pub(super) fn open_source(&mut self) {
        let raw = self.shown_raw();
        let Some(body) = &mut self.body else { return };
        match raw {
            Some(raw) => body.source = Some(body::Source::new(&raw)),
            None => {
                body.saved = Some("Could not show the source: the message is not downloaded".into())
            }
        }
    }
}
