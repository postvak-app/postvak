//! The message list as a Slint model. It keeps the rows the app lists and makes a row's texts only when the window
//! shows that row, so a folder of thousands of threads costs only the rows on screen.
use std::cell::RefCell;
use std::collections::HashSet;

use chrono::Local;
use std::rc::Rc;

use slint::{Model, ModelNotify, ModelRc, ModelTracker, SharedString, VecModel};

use super::icons;
use super::list::{self, Row, RowKey};
use super::state::{App, View};
use super::ui::RowData;

/// What the rows are drawn from.
#[derive(Default, PartialEq)]
struct Shown {
    rows: Vec<Row>,
    /// The account names, in All inboxes, where each row is tagged with its account.
    tags: Option<Vec<String>>,
    recipient: bool,
    search: bool,
    cursor: usize,
    marked: HashSet<(usize, RowKey)>,
}

#[derive(Default)]
pub(crate) struct ListModel {
    shown: RefCell<Shown>,
    notify: ModelNotify,
}

impl ListModel {
    /// Takes the app's rows: a new list resets the model, a moved cursor or mark only changes the rows it touched.
    pub(crate) fn update(&self, app: &App) {
        let tags = (app.view == View::AllInboxes)
            .then(|| app.accounts.iter().map(|a| a.name.clone()).collect());
        let (recipient, search) = (app.shows_recipient(), app.search.is_some());
        let same_list = {
            let shown = self.shown.borrow();
            shown.rows == app.list.rows
                && shown.tags == tags
                && shown.recipient == recipient
                && shown.search == search
        };
        if !same_list {
            *self.shown.borrow_mut() = Shown {
                rows: app.list.rows.clone(),
                tags,
                recipient,
                search,
                cursor: app.list.cursor,
                marked: app.list.marked.clone(),
            };
            self.notify.reset();
            return;
        }
        let changed: Vec<usize> = {
            let shown = self.shown.borrow();
            let was = |i: usize, row: &Row| i == shown.cursor || shown.marked.contains(&row.id());
            let is =
                |i: usize, row: &Row| i == app.list.cursor || app.list.marked.contains(&row.id());
            let marked = |row: &Row, set: &HashSet<_>| set.contains(&row.id());
            shown
                .rows
                .iter()
                .enumerate()
                .filter(|(i, row)| {
                    was(*i, row) != is(*i, row)
                        || marked(row, &shown.marked) != marked(row, &app.list.marked)
                })
                .map(|(i, _)| i)
                .collect()
        };
        {
            let mut shown = self.shown.borrow_mut();
            shown.cursor = app.list.cursor;
            shown.marked.clone_from(&app.list.marked);
        }
        for i in changed {
            self.notify.row_changed(i);
        }
    }
}

impl Model for ListModel {
    type Data = RowData;

    fn row_count(&self) -> usize {
        self.shown.borrow().rows.len()
    }

    fn row_data(&self, index: usize) -> Option<RowData> {
        let shown = self.shown.borrow();
        let row = shown.rows.get(index)?;
        let account = shown
            .tags
            .as_ref()
            .and_then(|tags| tags.get(row.account))
            .map(String::as_str);
        let marked = shown.marked.contains(&row.id());
        let now = Local::now();
        let columns = list::columns(row, account, marked, shown.recipient, shown.search, &now);
        let mut name = list::row_text(row, account, shown.recipient, shown.search, &now);
        if marked {
            name = format!("✔ {name}");
        }
        let marker = if marked {
            icons::CHECK
        } else if row.flagged {
            icons::FLAG
        } else {
            ""
        };
        let [read, moves, file] = list::menu(row).map(|group| {
            let titles: Vec<SharedString> =
                group.into_iter().map(|(title, _)| title.into()).collect();
            ModelRc::from(Rc::new(VecModel::from(titles)))
        });
        Some(RowData {
            menu_read: read,
            menu_move: moves,
            menu_file: file,
            unread: row.unread,
            flagged: row.flagged,
            marker: marker.into(),
            member: row.member,
            tag: columns.account.unwrap_or_default().into(),
            who: columns.who.into(),
            subject: columns.subject.into(),
            date: columns.date.into(),
            selected: index == shown.cursor || marked,
            name: name.into(),
        })
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
