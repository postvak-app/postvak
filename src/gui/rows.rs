//! The message list and the HTML page's strips as Slint models. It keeps the rows the app lists and makes a row's texts only when the window
//! shows that row, so a folder of thousands of threads costs only the rows on screen.
use std::cell::RefCell;
use std::collections::HashSet;

use chrono::Local;
use std::rc::Rc;
use std::sync::Arc;

use slint::{
    Image, Model, ModelNotify, ModelRc, ModelTracker, Rgba8Pixel, SharedPixelBuffer, SharedString,
    VecModel,
};

use super::html::{self, HtmlState, Pixels};
use super::icons;
use super::list::{self, Row, RowKey};
use super::state::{App, View};
use super::ui::{RowData, StripData};

/// A model of `items`, for the window's small lists.
pub(crate) fn model<T: Clone + 'static>(items: impl IntoIterator<Item = T>) -> ModelRc<T> {
    ModelRc::from(Rc::new(VecModel::from(
        items.into_iter().collect::<Vec<T>>(),
    )))
}

/// `current` with `items` put into it in place, changing only the rows that differ, so Slint keeps the elements of
/// the others (and their hover and open menus); a new model when `current` is not one of ours yet.
pub(crate) fn synced<T: Clone + PartialEq + 'static>(
    current: ModelRc<T>,
    items: Vec<T>,
) -> ModelRc<T> {
    let Some(shown) = current.as_any().downcast_ref::<VecModel<T>>() else {
        return model(items);
    };
    if shown.row_count() != items.len() {
        shown.set_vec(items);
    } else {
        for (index, item) in items.into_iter().enumerate() {
            if shown.row_data(index).as_ref() != Some(&item) {
                shown.set_row_data(index, item);
            }
        }
    }
    current
}

/// Puts `items` into the window's model `current` (see `synced`); `set` installs it when it is new.
pub(crate) fn sync<T: Clone + PartialEq + 'static>(
    current: ModelRc<T>,
    items: Vec<T>,
    set: impl FnOnce(ModelRc<T>),
) {
    let updated = synced(current.clone(), items);
    if updated != current {
        set(updated);
    }
}

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
    /// Takes the app's rows. A list of another length or kind resets the model; otherwise only the rows that look
    /// different change, so Slint keeps the elements of the others and a row turning read can fade.
    pub(crate) fn update(&self, app: &App) {
        let tags = (app.view == View::AllInboxes)
            .then(|| app.accounts.iter().map(|a| a.name.clone()).collect());
        let (recipient, search) = (app.shows_recipient(), app.search.is_some());
        let changed: Option<Vec<usize>> = {
            let shown = self.shown.borrow();
            let same_kind = shown.rows.len() == app.list.rows.len()
                && shown.tags == tags
                && shown.recipient == recipient
                && shown.search == search;
            same_kind.then(|| {
                (0..shown.rows.len())
                    .filter(|&i| {
                        shown.drawn(i)
                            != drawn(&app.list.rows[i], i, app.list.cursor, &app.list.marked)
                    })
                    .collect()
            })
        };
        if changed.as_ref().is_some_and(Vec::is_empty) {
            return;
        }
        *self.shown.borrow_mut() = Shown {
            rows: app.list.rows.clone(),
            tags,
            recipient,
            search,
            cursor: app.list.cursor,
            marked: app.list.marked.clone(),
        };
        match changed {
            Some(rows) => rows.into_iter().for_each(|i| self.notify.row_changed(i)),
            None => self.notify.reset(),
        }
    }
}

impl Shown {
    fn drawn(&self, index: usize) -> (&Row, bool, bool) {
        drawn(&self.rows[index], index, self.cursor, &self.marked)
    }
}

/// What row `index` is drawn from: the row, whether it has the cursor and whether it is marked.
fn drawn<'a>(
    row: &'a Row,
    index: usize,
    cursor: usize,
    marked: &HashSet<(usize, RowKey)>,
) -> (&'a Row, bool, bool) {
    (row, index == cursor, marked.contains(&row.id()))
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
        let columns = list::columns(row, account, shown.recipient, shown.search, &now);
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

/// The painted strips of the HTML page as images, each converted once: a strip keeps its image until the render
/// thread replaces it.
#[derive(Default)]
pub(crate) struct PageModel {
    strips: RefCell<Vec<(u32, Arc<Pixels>, StripData)>>,
    notify: ModelNotify,
}

impl PageModel {
    pub(crate) fn update(&self, view: Option<&HtmlState>) {
        let scale = view.map_or(1.0, |v| v.sent.0.max(0.5));
        let strips = view.map(|v| &v.strips);
        let same = {
            let shown = self.strips.borrow();
            let wanted: Vec<_> = strips.into_iter().flatten().collect();
            shown.len() == wanted.len()
                && shown
                    .iter()
                    .zip(&wanted)
                    .all(|((n, a, _), (m, b))| n == *m && Arc::ptr_eq(a, b))
        };
        if same {
            return;
        }
        let old = std::mem::take(&mut *self.strips.borrow_mut());
        let new = strips
            .into_iter()
            .flatten()
            .map(|(&strip, pixels)| {
                let kept = old
                    .iter()
                    .find(|(n, p, _)| *n == strip && Arc::ptr_eq(p, pixels));
                let data = kept.map_or_else(
                    || strip_data(strip, pixels, scale),
                    |(_, _, data)| data.clone(),
                );
                (strip, pixels.clone(), data)
            })
            .collect();
        *self.strips.borrow_mut() = new;
        self.notify.reset();
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "strip sizes are far below f32's exact range"
)]
fn strip_data(strip: u32, pixels: &Pixels, scale: f32) -> StripData {
    let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        pixels.rgba(),
        pixels.width(),
        pixels.height(),
    );
    StripData {
        image: Image::from_rgba8_premultiplied(buffer),
        y: strip as f32 * html::STRIP as f32 / scale,
        width: pixels.width() as f32 / scale,
        height: pixels.height() as f32 / scale,
    }
}

impl Model for PageModel {
    type Data = StripData;

    fn row_count(&self) -> usize {
        self.strips.borrow().len()
    }

    fn row_data(&self, index: usize) -> Option<StripData> {
        self.strips
            .borrow()
            .get(index)
            .map(|(_, _, data)| data.clone())
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
