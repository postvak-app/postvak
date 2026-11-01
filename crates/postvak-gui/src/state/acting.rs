//! Acting on the chosen messages, and undoing it.
use super::*;

impl App {
    /// Sends `action` for the marked rows, or the cursor row, each to its own account, and shows its effect before the
    /// server confirms it.
    pub(super) fn act(&mut self, action: Action) {
        let Some(optimistic) = Optimistic::of(&action) else {
            return;
        };
        let targets = self.targets();
        if targets.is_empty() {
            return;
        }
        if matches!(action, Action::MarkRead | Action::MarkUnread)
            && let Some(body) = &mut self.body
            && targets.iter().any(|(account, folder, uids)| {
                *account == body.account && *folder == body.key.0 && uids.contains(&body.key.1)
            })
        {
            body.read_sent = true;
        }
        self.pending_arm |= optimistic == Optimistic::Hidden;
        let count = targets.iter().map(|(_, _, uids)| uids.len()).sum();
        let id = self.take(undo::done_text(&action, count), 0);
        for (account, folder, uids) in targets {
            let action = match action {
                Action::Archive if self.is_archive(account, &folder) => {
                    Action::Move("INBOX".into())
                }
                Action::Junk if self.is_junk(account, &folder) => Action::NotJunk,
                _ => action.clone(),
            };
            if self.send_apply(account, folder, uids, action, optimistic) {
                if let Some(last) = self.accounts[account].apply_queue.back_mut() {
                    *last = Some(id);
                }
                if let Some(taken) = self.taken.iter_mut().find(|taken| taken.id == id) {
                    taken.waiting += 1;
                }
            }
        }
        if let Some(taken) = self
            .taken
            .iter()
            .find(|taken| taken.id == id && taken.waiting > 0)
        {
            self.toast = Some(Toast::new(taken.done.clone(), true));
        }
        self.list.marked.clear();
        self.rebuild_rows();
    }

    pub(super) fn mark_folder_read(&mut self, account: usize, folder: String) {
        let uids = match &self.accounts[account].store {
            Ok(store) => store.unread_uids(&folder),
            Err(_) => return,
        };
        match uids {
            Ok(uids) if uids.is_empty() => {}
            Ok(uids) => {
                self.send_apply(
                    account,
                    folder,
                    uids,
                    Action::MarkRead,
                    Optimistic::Seen(true),
                );
                self.rebuild_rows();
            }
            Err(e) => self.note_error(Some(account), e.to_string()),
        }
    }

    /// Sends one `Apply` and records its optimistic edit, which the matching `ActionDone` clears; false when the daemon
    /// is gone.
    pub(crate) fn send_apply(
        &mut self,
        account: usize,
        folder: String,
        uids: Vec<u32>,
        action: Action,
        optimistic: Optimistic,
    ) -> bool {
        let keys: Vec<RowKey> = uids.iter().map(|&uid| (folder.clone(), uid)).collect();
        if self.send(
            account,
            Command::Apply {
                folder,
                uids,
                action,
                by: "gui".into(),
            },
        ) {
            let state = &mut self.accounts[account];
            state.queued += 1;
            state.apply_queue.push_back(None);
            for key in keys {
                state.pending.entry(key).or_default().push(optimistic);
            }
            state.reload_folders();
            true
        } else {
            self.note_error(Some(account), DAEMON_LOST.into());
            false
        }
    }

    /// Our own `ActionDone` for `account`: its action joins the gesture whose `Apply` it answers, and an undo asked while
    /// that gesture waited goes out once it is complete.
    pub(super) fn action_done(&mut self, account: usize, action: Option<i64>) {
        let Some(Some(id)) = self.accounts[account].apply_queue.pop_front() else {
            return;
        };
        let Some(taken) = self.taken.iter_mut().find(|taken| taken.id == id) else {
            return;
        };
        taken.waiting = taken.waiting.saturating_sub(1);
        taken.actions.extend(action.map(|action| (account, action)));
        if taken.waiting == 0 && taken.state == UndoState::Asked {
            self.send_undo(id);
        }
    }

    /// Records a gesture of this window, which `z` can undo; its id.
    pub(super) fn take(&mut self, done: String, waiting: usize) -> u64 {
        let id = self.next_taken;
        self.next_taken += 1;
        self.taken.push(Taken {
            id,
            done,
            actions: Vec::new(),
            waiting,
            state: UndoState::Open,
        });
        // Only the newest are within reach of `z`.
        if self.taken.len() > MAX_TAKEN {
            self.taken.remove(0);
        }
        id
    }

    /// Undoes the newest gesture not undone yet; one still waiting for the daemon is undone once it answered.
    pub(super) fn undo_newest(&mut self) {
        let Some(taken) = self
            .taken
            .iter_mut()
            .rev()
            .find(|taken| taken.state == UndoState::Open && !taken.done.is_empty())
        else {
            self.toast = Some(Toast::new("Nothing to undo".into(), false));
            return;
        };
        if taken.waiting > 0 {
            taken.state = UndoState::Asked;
            self.toast = Some(Toast::new("Undoing…".into(), false));
        } else {
            let id = taken.id;
            self.send_undo(id);
        }
    }

    /// Sends an `Undo` for each action of gesture `id`.
    pub(super) fn send_undo(&mut self, id: u64) {
        let Some(actions) = self
            .taken
            .iter()
            .find(|taken| taken.id == id)
            .map(|taken| taken.actions.clone())
        else {
            return;
        };
        let mut left = 0;
        for (account, action) in actions {
            let command = Command::Undo {
                action,
                by: "gui".into(),
            };
            if self.send(account, command) {
                left += 1;
            } else {
                self.note_error(Some(account), DAEMON_LOST.into());
            }
        }
        let Some(taken) = self.taken.iter_mut().find(|taken| taken.id == id) else {
            return;
        };
        if left == 0 {
            taken.state = UndoState::Undone;
            self.toast = Some(Toast::new("Nothing to undo".into(), false));
        } else {
            taken.state = UndoState::Undoing {
                left,
                reports: Vec::new(),
            };
            self.toast = Some(Toast::new("Undoing…".into(), false));
        }
    }

    /// An answer to one of this window's undos: once every account answered, the toast says what came back.
    pub(super) fn undo_answered(
        &mut self,
        account: usize,
        report: Option<postvak_core::actions::UndoReport>,
    ) {
        let undoing = self.taken.iter_mut().find(|taken| {
            matches!(taken.state, UndoState::Undoing { .. })
                && taken.actions.iter().any(|(acc, action)| {
                    *acc == account
                        && report
                            .as_ref()
                            .is_none_or(|report| report.action == *action)
                })
        });
        let Some(taken) = undoing else { return };
        let UndoState::Undoing { left, reports } = &mut taken.state else {
            return;
        };
        *left = left.saturating_sub(1);
        reports.extend(report);
        if *left == 0 {
            let text = undo::undone_text(reports);
            taken.state = UndoState::Undone;
            self.toast = Some(Toast::new(text, false));
        }
    }

    /// Ends the toast once its time is up and the pointer is not on it.
    pub(super) fn expire_toast(&mut self, now: f64) {
        let Some(toast) = &mut self.toast else { return };
        if toast.hovered {
            return;
        }
        let until = *toast.until.get_or_insert(now + undo::TOAST_SECONDS);
        if now >= until {
            self.toast = None;
        } else {
            self.request_tick(until);
        }
    }
}
