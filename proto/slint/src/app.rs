//! Everything behind the Slint window: the model, the keymap, the shown message and its HTML strips. `ui/mail.slint`
//! only draws and calls back; this is the only code that changes state, as `app.rs` is in the egui GUI.
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Local};
use postvak::gui::html::{self, Load, Pixels, Renderer, Reply, Request, STRIP};
use postvak::gui::list::{LIST_ROW, RowKey, columns, display_name, offset_showing};
use postvak::message::{HtmlBody, clean};
use postvak::rules::Action;
use slint::platform::Key;
use slint::{
    ComponentHandle, Image, ModelNotify, ModelRc, ModelTracker, Rgba8Pixel,
    SharedPixelBuffer, SharedString, Timer, VecModel,
};

use crate::model::Model;
use crate::{KeyHelp, MailWindow, RowData, StripData};

/// `app.rs`'s `READ_DELAY`: an opened message becomes read after its text was on screen this long.
pub const READ_DELAY: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Act {
    Next,
    Prev,
    Expand,
    Collapse,
    Mark,
    Archive,
    Help,
    Escape,
    Search,
}

#[derive(Clone, Copy)]
enum Binding {
    Text(&'static str),
    Key(Key),
}

/// The keymap: `App::keys` for the list pane. The help table is read from it.
const BINDINGS: [(Binding, Act, &str); 15] = [
    (Binding::Text("j"), Act::Next, "Next message"),
    (Binding::Key(Key::DownArrow), Act::Next, "Next message"),
    (Binding::Text("k"), Act::Prev, "Previous message"),
    (Binding::Key(Key::UpArrow), Act::Prev, "Previous message"),
    (Binding::Key(Key::RightArrow), Act::Expand, "Open thread"),
    (Binding::Key(Key::LeftArrow), Act::Collapse, "Close thread"),
    (Binding::Text("x"), Act::Mark, "Mark"),
    (Binding::Text("e"), Act::Archive, "Archive"),
    (Binding::Text("?"), Act::Help, "Keys"),
    (Binding::Key(Key::Escape), Act::Escape, "Close, clear marks"),
    (Binding::Text("/"), Act::Search, "Search"),
    // Shifted letters arrive as their capitals; these keep the table honest about what is bound.
    (Binding::Text("J"), Act::Next, "Next message"),
    (Binding::Text("K"), Act::Prev, "Previous message"),
    (Binding::Text("X"), Act::Mark, "Mark"),
    (Binding::Text("E"), Act::Archive, "Archive"),
];

impl Binding {
    fn text(self) -> SharedString {
        match self {
            Binding::Text(text) => text.into(),
            Binding::Key(key) => key.into(),
        }
    }

    fn name(self) -> String {
        match self {
            Binding::Text(text) => text.to_string(),
            Binding::Key(key) => format!("{key:?}"),
        }
    }
}

/// The act a key's text runs, from the keymap.
pub fn act_for(text: &str) -> Option<Act> {
    BINDINGS
        .iter()
        .find(|(binding, ..)| binding.text() == text)
        .map(|(_, act, _)| *act)
}

/// One line per act: its keys, from the keymap, and what it does.
pub fn key_help() -> Vec<KeyHelp> {
    let mut help: Vec<(Act, Vec<String>, &str)> = Vec::new();
    for (binding, act, what) in BINDINGS {
        if matches!(binding, Binding::Text(t) if t.chars().all(char::is_uppercase)) {
            continue;
        }
        match help.iter_mut().find(|(a, ..)| *a == act) {
            Some((_, keys, _)) => keys.push(binding.name()),
            None => help.push((act, vec![binding.name()], what)),
        }
    }
    help.into_iter()
        .map(|(_, keys, what)| KeyHelp {
            keys: keys.join(", ").into(),
            what: what.into(),
        })
        .collect()
}

/// The list's rows for Slint, built only for the rows it shows.
pub struct Rows {
    model: Rc<RefCell<Model>>,
    now: DateTime<Local>,
    notify: ModelNotify,
}

impl slint::Model for Rows {
    type Data = RowData;

    fn row_count(&self) -> usize {
        self.model.borrow().rows.len()
    }

    fn row_data(&self, index: usize) -> Option<RowData> {
        let model = self.model.borrow();
        let row = model.rows.get(index)?;
        let account = (model.shown.len() > 1).then(|| model.accounts[row.account].name.as_str());
        let c = columns(row, account, model.marked.contains(&row.id()), false, false, &self.now);
        let label = format!(
            "{}{}{}, {}, {}",
            if c.unread { "Unread, " } else { "" },
            if c.flagged { "flagged, " } else { "" },
            c.who,
            c.subject,
            c.date
        );
        let who = match c.account {
            Some(account) => format!("{account} · {}", c.who),
            None => c.who,
        };
        Some(RowData {
            who: who.into(),
            subject: c.subject.into(),
            date: c.date.into(),
            unread: c.unread,
            flagged: c.flagged,
            marked: c.marked,
            member: c.member,
            label: label.into(),
        })
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
}

/// The message on screen.
struct Shown {
    account: usize,
    key: RowKey,
    has_text: bool,
    html: Option<HtmlBody>,
    view: Option<HtmlView>,
    /// Set when the HTML could not be laid out; the text shows instead.
    note: Option<&'static str>,
    /// Marks the message read after `READ_DELAY`; dropped, and so stopped, with the message.
    read_timer: Option<Timer>,
}

/// `HtmlState` without egui: what is laid out, at which width and scale, and the strips painted so far.
struct HtmlView {
    generation: u64,
    sent: (f32, f32),
    /// Width and height in logical pixels, once laid out.
    page: Option<(f32, f32)>,
    strips: BTreeMap<u32, Image>,
    requested: BTreeSet<u32>,
}

pub struct App {
    pub model: Rc<RefCell<Model>>,
    rows: Rc<Rows>,
    strips: Rc<VecModel<StripData>>,
    ui: slint::Weak<MailWindow>,
    me: Weak<RefCell<App>>,
    renderer: Renderer,
    generation: u64,
    shown: Option<Shown>,
    /// Lays the page out again once a new width held still for `SETTLE`.
    settle: Timer,
}

thread_local! {
    /// The app of this thread's window, for the render thread's wake-ups.
    static APP: RefCell<Option<Rc<RefCell<App>>>> = const { RefCell::new(None) };
}

impl App {
    /// The app over `model`, wired to `ui`'s callbacks.
    pub fn new(ui: &MailWindow, model: Model) -> Rc<RefCell<App>> {
        let model = Rc::new(RefCell::new(model));
        let rows = Rc::new(Rows {
            model: model.clone(),
            now: Local::now(),
            notify: ModelNotify::default(),
        });
        let strips = Rc::new(VecModel::default());
        ui.set_rows(ModelRc::from(rows.clone()));
        ui.set_strips(ModelRc::from(strips.clone()));
        ui.set_key_help(ModelRc::from(Rc::new(VecModel::from(key_help()))));
        // Wakes the window's thread when a render reply is ready; does nothing without an event loop (tests poll).
        let renderer = Renderer::start(Arc::new(|| {
            let _ = slint::invoke_from_event_loop(|| {
                APP.with(|app| {
                    if let Some(app) = app.borrow().as_ref() {
                        app.borrow_mut().receive();
                    }
                });
            });
        }));
        let app = Rc::new_cyclic(|me| {
            RefCell::new(App {
                model,
                rows,
                strips,
                ui: ui.as_weak(),
                me: me.clone(),
                renderer,
                generation: 0,
                shown: None,
                settle: Timer::default(),
            })
        });
        APP.with(|slot| *slot.borrow_mut() = Some(app.clone()));
        let weak = Rc::downgrade(&app);
        ui.on_key(move |text| with(&weak, |app| app.key(&text)).unwrap_or(false));
        let weak = Rc::downgrade(&app);
        ui.on_select(move |index| {
            let index = usize::try_from(index).unwrap_or(0);
            with(&weak, |app| app.move_cursor(|m| m.select(index)));
        });
        let weak = Rc::downgrade(&app);
        ui.on_search_edited(move |query| {
            with(&weak, |app| app.change(|m| m.set_search(Some(query.to_string()))));
        });
        let weak = Rc::downgrade(&app);
        ui.on_body_scrolled(move || {
            with(&weak, App::visible);
        });
        let weak = Rc::downgrade(&app);
        ui.on_body_resized(move || {
            with(&weak, App::resized);
        });
        app.borrow_mut().sync_ui();
        app
    }

    fn ui(&self) -> Option<MailWindow> {
        self.ui.upgrade()
    }

    /// Runs a key's act; false when the key is not in the keymap.
    pub fn key(&mut self, text: &str) -> bool {
        let Some(act) = act_for(text) else {
            return false;
        };
        match act {
            Act::Next => self.move_cursor(|m| m.step(1)),
            Act::Prev => self.move_cursor(|m| m.step(-1)),
            Act::Expand => self.change(Model::expand),
            Act::Collapse => self.change(Model::collapse),
            Act::Mark => self.change(Model::toggle_mark),
            Act::Archive => self.change(|m| m.act(Action::Archive)),
            Act::Help => self.change(|m| m.help = !m.help),
            Act::Search => {
                if let Some(ui) = self.ui() {
                    ui.invoke_focus_search();
                }
            }
            Act::Escape => {
                let searching = self.model.borrow().search.is_some()
                    || self.ui().is_some_and(|ui| ui.get_search_focused());
                if searching {
                    self.change(|m| m.set_search(None));
                    if let Some(ui) = self.ui() {
                        ui.set_query(SharedString::new());
                        ui.invoke_focus_list();
                    }
                } else {
                    self.change(Model::escape);
                }
            }
        }
        true
    }

    /// Runs `f` on the model, then shows what changed: any row may have.
    pub fn change(&mut self, f: impl FnOnce(&mut Model)) {
        f(&mut self.model.borrow_mut());
        self.rows.notify.reset();
        self.sync_ui();
    }

    /// Runs `f`, which only moves the cursor: Slint compares each shown row with the cursor itself, so no row is
    /// built again.
    pub fn move_cursor(&mut self, f: impl FnOnce(&mut Model)) {
        f(&mut self.model.borrow_mut());
        self.sync_ui();
    }

    /// The cursor, its row scrolled into view, the help, and the message under the cursor.
    fn sync_ui(&mut self) {
        let Some(ui) = self.ui() else { return };
        let (cursor, help) = {
            let model = self.model.borrow();
            (model.cursor, model.help)
        };
        ui.set_cursor(i32::try_from(cursor).unwrap_or(i32::MAX));
        ui.set_help(help);
        let offset = -ui.get_list_y();
        let showing = offset_showing(cursor, LIST_ROW, offset, ui.get_list_height());
        if showing != offset {
            ui.set_list_y(-showing);
        }
        self.sync_body();
    }

    /// `sync_body`: loads the cursor's message when the cursor moved to another one, and starts the read timer once
    /// the user chose it.
    fn sync_body(&mut self) {
        let (current, armed) = {
            let model = self.model.borrow();
            let current = model.cursor_row();
            (current.clone(), current.is_some() && model.armed_for == current)
        };
        let shown = self.shown.as_ref().map(|s| (s.account, s.key.clone()));
        if current != shown {
            self.shown = current.map(|(account, key)| self.load(account, key));
            if let Some(ui) = self.ui() {
                ui.set_body_y(0.0);
            }
            self.show_body();
            self.layout();
        }
        let me = self.me.clone();
        if armed && let Some(shown) = self.shown.as_mut().filter(|s| s.read_timer.is_none()) {
            let timer = Timer::default();
            timer.start(slint::TimerMode::SingleShot, READ_DELAY, move || {
                if let Some(app) = me.upgrade() {
                    app.borrow_mut().mark_read();
                }
            });
            shown.read_timer = Some(timer);
        }
    }

    fn load(&self, account: usize, key: RowKey) -> Shown {
        let model = self.model.borrow();
        let store = &model.accounts[account].store;
        let message = store.message(&key.0, key.1).ok().flatten();
        let raw = store.raw(&key.0, key.1).ok().flatten();
        let html = raw.as_deref().and_then(html::html_of);
        let note = html
            .as_ref()
            .filter(|h| h.html.len() > html::MAX_HTML)
            .map(|_| html::TOO_LARGE);
        let m = message.as_ref();
        if let Some(ui) = self.ui() {
            let subject = m.and_then(|m| m.subject.as_deref()).unwrap_or_default();
            let from = m.and_then(|m| m.from_addr.as_deref()).map(display_name).unwrap_or_default();
            let text = m.and_then(|m| m.body_text.as_deref()).map(|t| clean(t, true));
            ui.set_subject(clean(subject, false).into());
            ui.set_from(clean(&from, false).into());
            ui.set_text(text.unwrap_or_else(|| "Loading…".into()).into());
        }
        Shown {
            account,
            key,
            has_text: m.is_some_and(|m| m.body_text.is_some()),
            html,
            view: None,
            note,
            read_timer: None,
        }
    }

    /// The page or the text, whichever the shown message has laid out.
    fn show_body(&self) {
        let Some(ui) = self.ui() else { return };
        let page = self.shown.as_ref().and_then(|s| s.view.as_ref()).and_then(|v| v.page);
        ui.set_show_page(page.is_some());
        if let Some((width, height)) = page {
            ui.set_page_width(width);
            ui.set_page_height(height);
        }
        let strips: Vec<StripData> = self
            .shown
            .as_ref()
            .and_then(|s| s.view.as_ref())
            .map(|view| {
                let scale = view.sent.0;
                view.strips
                    .iter()
                    .map(|(n, image)| {
                        let size = image.size();
                        StripData {
                            image: image.clone(),
                            y: (n * STRIP) as f32 / scale,
                            width: size.width as f32 / scale,
                            height: size.height as f32 / scale,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.strips.set_vec(strips);
    }

    /// `mark_read_after_delay`'s send, run by the read timer once the text has been on screen.
    fn mark_read(&mut self) {
        let Some(shown) = self.shown.as_ref().filter(|s| s.has_text) else {
            return;
        };
        let (account, key) = (shown.account, shown.key.clone());
        if !self.model.borrow().is_seen(account, &key) {
            self.change(|m| m.mark_read(account, key));
        }
    }

    /// The width changed: a new layout once it held still for `SETTLE`.
    fn resized(&mut self) {
        let me = self.me.clone();
        self.settle.start(
            slint::TimerMode::SingleShot,
            Duration::from_secs_f64(html::SETTLE),
            move || {
                if let Some(app) = me.upgrade() {
                    app.borrow_mut().layout();
                }
            },
        );
    }

    /// `html_width`: lays the shown page out at the body's width, unless it already is.
    fn layout(&mut self) {
        let Some(ui) = self.ui() else { return };
        let scale = ui.window().scale_factor();
        let width = (ui.get_body_width() - 32.0).max(html::MIN_WIDTH);
        let Some(shown) = self.shown.as_mut().filter(|s| s.html.is_some() && s.note.is_none()) else {
            return;
        };
        if shown.view.as_ref().is_some_and(|v| v.sent == (scale, width)) {
            return;
        }
        let Some(source) = &shown.html else { return };
        self.generation += 1;
        self.renderer.send(Request::Load(Load {
            generation: self.generation,
            html: source.html.clone(),
            inline: source.inline.clone(),
            scale,
            width,
        }));
        // The old page stays on screen until the new one is laid out.
        let old = shown.view.take();
        shown.view = Some(HtmlView {
            generation: self.generation,
            sent: (scale, width),
            page: old.as_ref().and_then(|v| v.page),
            strips: old.map(|v| v.strips).unwrap_or_default(),
            requested: BTreeSet::new(),
        });
    }

    /// `html_visible`: asks for the strips around what is on screen and lets go of those far from it.
    fn visible(&mut self) {
        let Some(ui) = self.ui() else { return };
        let Some(view) = self.shown.as_mut().and_then(|s| s.view.as_mut()) else {
            return;
        };
        let Some((_, height)) = view.page else { return };
        let top = -ui.get_body_y();
        let bottom = top + ui.get_body_height().max(1.0);
        let scale = view.sent.0;
        for strip in html::wanted_strips(top, bottom, height, scale) {
            if view.requested.insert(strip) {
                self.renderer.send(Request::Paint {
                    generation: view.generation,
                    strip,
                });
            }
        }
        let far = html::far_strips(view.strips.keys().copied(), top, bottom, scale);
        if !far.is_empty() {
            for strip in far {
                view.strips.remove(&strip);
                view.requested.remove(&strip);
            }
            self.show_body();
        }
    }

    /// `receive_html`: the render thread's replies for the shown layout; older ones are dropped.
    pub fn receive(&mut self) {
        let mut changed = false;
        for reply in self.renderer.replies() {
            let Some(shown) = self.shown.as_mut() else { continue };
            let Some(view) = shown
                .view
                .as_mut()
                .filter(|v| v.generation == reply.generation())
            else {
                continue;
            };
            changed = true;
            match reply {
                Reply::Failed { .. } => {
                    shown.note = Some(html::FAILED);
                    shown.view = None;
                }
                Reply::Laid { height, width, .. } => {
                    if height > html::MAX_HEIGHT {
                        shown.note = Some(html::TOO_LARGE);
                        shown.view = None;
                        continue;
                    }
                    view.page = Some((width, height));
                    view.strips.clear();
                    view.requested.clear();
                }
                Reply::Strip { strip, image, .. } => {
                    view.strips.insert(strip, strip_image(&image));
                }
                Reply::Link { .. } | Reply::Selected { .. } => {}
            }
        }
        if changed {
            self.show_body();
            self.visible();
        }
    }

    /// How many strips of the shown page are painted, for tests that wait for them.
    #[cfg(test)]
    pub fn painted_strips(&self) -> usize {
        let view = self.shown.as_ref().and_then(|s| s.view.as_ref());
        // Zero until every strip asked for is in, so a test sees the whole visible page.
        view.filter(|v| v.requested.iter().all(|s| v.strips.contains_key(s)))
            .map_or(0, |v| v.strips.len())
    }
}

/// Runs `f` on the app behind `weak`, if it is still there.
fn with<R>(weak: &Weak<RefCell<App>>, f: impl FnOnce(&mut App) -> R) -> Option<R> {
    weak.upgrade().map(|app| f(&mut app.borrow_mut()))
}

/// A painted strip as a Slint image: Slint takes Blitz's premultiplied RGBA as it is, so this is one copy.
pub fn strip_image(pixels: &Pixels) -> Image {
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(pixels.width(), pixels.height());
    buffer.make_mut_bytes().copy_from_slice(pixels.rgba());
    Image::from_rgba8_premultiplied(buffer)
}
