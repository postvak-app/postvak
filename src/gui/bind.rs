//! State into the window and the window's callbacks back into `UiAction`s. `present` sets the window's properties and
//! models from `&App`; `connect` hands each callback to `super::on`, which asks the app what it means and applies it.
//! Nothing here changes state.
use std::rc::Rc;

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::config;

use super::folders::{self, Entry, FolderEntry};
use super::keys::{self, Key, Named, Press};
use super::rows::ListModel;
use super::state::{App, Focus, UiAction};
use super::ui::{
    BodyInfo, Dot, FolderItem, FolderKind, KeyHelp, MailWindow, MenuChoice, Pane, StatusLine,
    Theme, ThemePreference, ToolButton,
};
use super::{body, icons, list, status, toolbar};
use crate::message::clean;

/// What a callback asks the app: the actions it means, in order.
pub(crate) type Ask = Box<dyn FnOnce(&App) -> Vec<UiAction>>;

/// An `Ask` for at most one action.
fn one(f: impl FnOnce(&App) -> Option<UiAction> + 'static) -> Ask {
    Box::new(move |app| f(app).into_iter().collect())
}

/// Shows `app` in `window`.
pub(crate) fn present(app: &App, window: &MailWindow) {
    window
        .global::<Theme>()
        .set_preference(preference(app.theme));
    window.set_config_changed(app.config_changed);
    window.set_focused_pane(pane(app.focus));
    window.set_mail_view(app.view.lists_mail());
    window.set_list_min_width(list::MIN_WIDTH);
    window.set_close_icon(icons::CLOSE.into());
    window.set_search_icon(icons::SEARCH.into());
    window.set_sync_icon(icons::SYNC.into());

    let buttons = toolbar::buttons(app).map(|b| ToolButton {
        icon: b.icon.into(),
        name: b.name.into(),
        hint: b.hint.into(),
    });
    window.set_tool_buttons(model(buttons));
    window.set_tool_buttons_enabled(toolbar::enabled(app));
    window.set_search_hint(toolbar::SEARCH_HINT.into());

    window.set_folders(model(folders::entries(app).iter().map(folder_item)));
    window.set_pinned(model(folders::pinned(app).iter().map(folder_row)));

    window.set_status_lines(model(status::lines(app).into_iter().map(status_line)));
    window.set_sync_hint(status::sync_key().into());
    window.set_version(format!("v{}", status::VERSION).into());
    let update = app
        .newer_release()
        .map(status::update_text)
        .unwrap_or_default();
    window.set_update(update.into());

    present_list(app, window);
    present_body(app, window);

    window.set_help_open(app.help_open);
    let help = keys::HELP.map(|(keys, what)| KeyHelp {
        keys: keys.into(),
        what: what.into(),
    });
    window.set_keys(model(help));
    window.set_history_open(app.history_open);
    window.set_history(model(
        status::history(app).into_iter().map(SharedString::from),
    ));
}

fn present_list(app: &App, window: &MailWindow) {
    window.set_list_title(list::title(&app.view).unwrap_or_default().into());
    window.set_list_counts(list::counts(&app.list.rows).into());
    window.set_searching(app.search.is_some());
    window.set_search_hint_text(list::search_hint(&app.view).into());
    window.set_results(list::results(app.list.rows.len()).into());
    let rows = window.get_rows();
    match rows.as_any().downcast_ref::<ListModel>() {
        Some(model) => model.update(app),
        None => {
            let model = ListModel::default();
            model.update(app);
            window.set_rows(ModelRc::new(model));
        }
    }
    if app.list.follow_cursor {
        let (offset, height) = app.list.viewport;
        let offset = list::offset_showing(app.list.cursor, list::LIST_ROW, offset, height);
        window.set_list_y(-offset);
    }
    window.set_move_open(app.move_picker.is_some());
    let targets = app
        .move_targets()
        .into_iter()
        .map(|name| SharedString::from(clean(name, false)));
    window.set_move_targets(model(targets));
}

fn present_body(app: &App, window: &MailWindow) {
    let pane = body::pane(app);
    let text = |value: Option<String>| SharedString::from(value.unwrap_or_default());
    window.set_body_info(BodyInfo {
        empty: pane.empty.unwrap_or_default().into(),
        subject: text(pane.subject),
        initials: pane.initials.into(),
        name: pane.name.into(),
        address: text(pane.address),
        short_date: pane.short_date.into(),
        full_date: pane.full_date.into(),
        to: text(pane.to),
        cc: text(pane.cc),
        downloaded: pane.downloaded,
        missing: pane.missing.into(),
        has_html: pane.has_html,
        shows_html: pane.shows_html,
        text_note: pane.text_note.unwrap_or_default().into(),
        attachments: model(pane.attachments.into_iter().map(SharedString::from)),
        saved: text(pane.saved),
        links: model(pane.links.into_iter().map(SharedString::from)),
    });
    window.set_save_icon(icons::SAVE.into());
    window.set_source_icon(icons::SOURCE.into());
    // Messages can be megabytes: set the text only when it changed.
    let body_text = app
        .body
        .as_ref()
        .and_then(|b| b.text.as_deref())
        .unwrap_or_default();
    if window.get_body_text() != body_text {
        window.set_body_text(body_text.into());
    }
    let source = app.body.as_ref().and_then(|b| b.source.as_ref());
    window.set_source_open(source.is_some());
    let source_text = source.map_or("", |s| s.text.as_str());
    if window.get_source_text() != source_text {
        window.set_source_text(source_text.into());
        let rows = source.map(|s| {
            s.rows
                .iter()
                .map(|r| SharedString::from(&s.text[r.clone()]))
        });
        window.set_source_rows(model(rows.into_iter().flatten()));
        // Monospace: a row is as wide as its characters, at about 0.6 em each.
        let widest = source
            .into_iter()
            .flat_map(|s| s.rows.iter().map(|r| s.text[r.clone()].chars().count()))
            .max();
        #[allow(
            clippy::cast_precision_loss,
            reason = "a row is at most SOURCE_ROW characters"
        )]
        window.set_source_widest(widest.unwrap_or(0) as f32 * 8.5);
    }
}

/// Hands the window's callbacks to `on`, which runs a function of the app and applies the action it returns.
pub(crate) fn connect(window: &MailWindow, on: fn(Ask) -> bool) {
    let act = move |action: UiAction| on(Box::new(move |_| vec![action]));
    window.on_key(move |text, command, alt| {
        let press = press(&text, command, alt);
        on(one(move |app| {
            let typing = app.search_focused || app.move_picker.is_some();
            keys::action(&press?, app.focus, typing)
        }))
    });
    window.on_tool(move |i| {
        on(one(move |app| {
            let enabled = toolbar::enabled(app);
            let button = toolbar::buttons(app)
                .into_iter()
                .nth(usize::try_from(i).ok()?)?;
            enabled.then_some(button.action)
        }));
    });
    window.on_search(move || {
        act(UiAction::StartSearch);
    });
    window.on_folder_clicked(move |i| {
        on(one(move |app| {
            let view = folder_at(app, i)?.view?;
            Some(UiAction::SelectView(view))
        }));
    });
    window.on_caret_clicked(move |i| {
        on(one(move |app| {
            let (_, account, path) = folder_at(app, i)?.branch?;
            Some(UiAction::ToggleFolder(account, path))
        }));
    });
    window.on_folder_menu(move |i, j| {
        on(one(move |app| {
            let (_, enabled, action) = folder_at(app, i)?
                .menu()
                .into_iter()
                .nth(usize::try_from(j).ok()?)?;
            enabled.then_some(action)
        }));
    });
    window.on_pinned_clicked(move |i| {
        on(one(move |app| {
            let pinned = folders::pinned(app)
                .into_iter()
                .nth(usize::try_from(i).ok()?)?;
            Some(UiAction::SelectView(pinned.view?))
        }));
    });
    window.on_sync(move || {
        act(UiAction::SyncNow);
    });
    window.on_toggle_history(move || {
        act(UiAction::ToggleHistory);
    });
    window.on_toggle_help(move || {
        act(UiAction::ToggleHelp);
    });
    window.on_set_theme(move |theme| {
        act(UiAction::SetTheme(theme_of(theme)));
    });
    window.on_row_clicked(move |i| {
        act(UiAction::SelectRow(usize::try_from(i).unwrap_or_default()));
    });
    window.on_row_menu(move |i, j| {
        on(Box::new(move |app| {
            let (Ok(i), Ok(j)) = (usize::try_from(i), usize::try_from(j)) else {
                return Vec::new();
            };
            let Some(row) = app.list.rows.get(i) else {
                return Vec::new();
            };
            let Some((_, action)) = list::menu(row).into_iter().flatten().nth(j) else {
                return Vec::new();
            };
            // The menu acts on the marked rows when it was opened on one of them, else on its own row.
            if app.list.marked.contains(&row.id()) {
                vec![action]
            } else {
                vec![UiAction::SelectRow(i), action]
            }
        }));
    });
    window.on_search_edited(move |text| {
        act(UiAction::SearchFor(text.into()));
    });
    window.on_search_accepted(move || {
        act(UiAction::SelectRow(0));
    });
    window.on_search_focus(move |focused| {
        act(UiAction::SearchFocused(focused));
    });
    window.on_list_scrolled(move |offset, height| {
        act(UiAction::ListViewport { offset, height });
    });
    window.on_move_filter(move |text| {
        act(UiAction::MoveFilter(text.into()));
    });
    window.on_move_accepted(move || {
        on(one(|app| {
            Some(UiAction::MoveTo(app.move_targets().first()?.to_string()))
        }));
    });
    window.on_move_chosen(move |i| {
        on(one(move |app| {
            let target = app
                .move_targets()
                .into_iter()
                .nth(usize::try_from(i).ok()?)?;
            Some(UiAction::MoveTo(target.to_string()))
        }));
    });
    window.on_escape(move || {
        act(UiAction::Escape);
    });
    window.on_toggle_html(move || {
        act(UiAction::ToggleHtml);
    });
    window.on_save_eml(move || {
        act(UiAction::SaveEml);
    });
    window.on_open_source(move || {
        act(UiAction::OpenSource);
    });
    window.on_save_attachment(move |i| {
        on(one(move |app| {
            let body = app.body.as_ref()?;
            let attachment = body.attachments.get(usize::try_from(i).ok()?)?;
            Some(UiAction::SaveAttachment(attachment.index))
        }));
    });
    window.on_open_link(move |url| {
        on(one(move |_| {
            body::has_target(&url).then(|| UiAction::OpenUrl(url.into()))
        }));
    });
    window.on_open_update(move || {
        on(one(|app| {
            let version = app.newer_release()?;
            Some(UiAction::OpenUrl(crate::update::release_url(version)))
        }));
    });
}

fn model<T: Clone + 'static>(items: impl IntoIterator<Item = T>) -> ModelRc<T> {
    ModelRc::from(Rc::new(VecModel::from(
        items.into_iter().collect::<Vec<T>>(),
    )))
}

/// The folder entry the pane shows at `index`.
fn folder_at(app: &App, index: i32) -> Option<FolderEntry> {
    match folders::entries(app)
        .into_iter()
        .nth(usize::try_from(index).ok()?)?
    {
        Entry::Folder(folder) => Some(folder),
        _ => None,
    }
}

fn folder_item(entry: &Entry) -> FolderItem {
    match entry {
        Entry::Folder(folder) => folder_row(folder),
        Entry::Account { name, busy, error } => FolderItem {
            kind: FolderKind::Account,
            label: name.into(),
            busy: *busy,
            error: error.as_deref().unwrap_or_default().into(),
            ..FolderItem::default()
        },
        Entry::Separator => FolderItem {
            kind: FolderKind::Separator,
            ..FolderItem::default()
        },
    }
}

fn folder_row(folder: &FolderEntry) -> FolderItem {
    let menu = folder.menu();
    // Open and Collapse or Expand above the separator; marking read and syncing below it.
    let head = menu
        .iter()
        .take_while(|(_, _, action)| {
            matches!(action, UiAction::SelectView(_) | UiAction::ToggleFolder(..))
        })
        .count();
    let choices = |items: &[(String, bool, UiAction)]| {
        model(items.iter().map(|(title, enabled, _)| MenuChoice {
            title: title.into(),
            enabled: *enabled,
        }))
    };
    FolderItem {
        kind: FolderKind::Folder,
        icon: folder.icon.into(),
        label: folder.label.as_str().into(),
        count: folder.count.as_deref().unwrap_or_default().into(),
        name: folder.name.as_str().into(),
        depth: i32::try_from(folder.depth).unwrap_or(i32::MAX),
        selected: folder.selected,
        caret_name: folder.caret_name().unwrap_or_default().into(),
        menu_head: choices(&menu[..head]),
        menu_tail: choices(&menu[head..]),
        ..FolderItem::default()
    }
}

fn status_line(line: status::Line) -> StatusLine {
    StatusLine {
        text: line.text.into(),
        dot: match line.dot {
            None => Dot::None,
            Some(status::Dot::Failed) => Dot::Failed,
            Some(status::Dot::UpToDate) => Dot::UpToDate,
            Some(status::Dot::Offline) => Dot::Offline,
        },
        failed: line.failed,
        progress: line.progress.unwrap_or(-1.0),
    }
}

/// A key event's text as the keymap reads it; None for a key it has no use for.
fn press(text: &str, command: bool, alt: bool) -> Option<Press> {
    use slint::platform::Key as Slint;
    let mut chars = text.chars();
    let (Some(first), None) = (chars.next(), chars.next()) else {
        return None;
    };
    let named = [
        (Slint::Backspace, Named::Backspace),
        (Slint::Delete, Named::Delete),
        (Slint::DownArrow, Named::Down),
        (Slint::Escape, Named::Escape),
        (Slint::LeftArrow, Named::Left),
        (Slint::RightArrow, Named::Right),
        (Slint::Tab, Named::Tab),
        (Slint::UpArrow, Named::Up),
    ];
    let key = match named.into_iter().find(|(key, _)| char::from(*key) == first) {
        Some((_, named)) => Key::Named(named),
        // Other keys without text arrive as characters in Unicode's private use area.
        None if ('\u{E000}'..='\u{F8FF}').contains(&first) || first.is_control() => return None,
        None => Key::Text(text.into()),
    };
    Some(Press { key, command, alt })
}

fn preference(theme: config::Theme) -> ThemePreference {
    match theme {
        config::Theme::Dark => ThemePreference::Dark,
        config::Theme::Light => ThemePreference::Light,
        config::Theme::System => ThemePreference::System,
    }
}

fn theme_of(preference: ThemePreference) -> config::Theme {
    match preference {
        ThemePreference::Dark => config::Theme::Dark,
        ThemePreference::Light => config::Theme::Light,
        ThemePreference::System => config::Theme::System,
    }
}

fn pane(focus: Focus) -> Pane {
    match focus {
        Focus::Body => Pane::Body,
        Focus::Folders => Pane::Folders,
        Focus::List => Pane::List,
    }
}
