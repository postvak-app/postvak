//! State into the window and the window's callbacks back into `UiAction`s. `present` sets the window's properties and
//! models from `&App`; `connect` hands each callback to `super::on`, which asks the app what it means and applies it.
//! Nothing here changes state.
use slint::language::{SortOrder, StandardListViewItem, TableColumn};
use slint::{ComponentHandle, Model, ModelRc, SharedString};

use crate::config;

use super::folders::{self, Entry, FolderEntry};
use super::html::{self, HtmlState};
use super::keys::{self, Key, Named, Press};
use super::rows::{ListModel, PageModel, model, sync, synced};
use super::rules::SheetSort;
use super::state::{App, Focus, UiAction, View};
use super::ui::{
    BodyInfo, Dot, FolderItem, FolderKind, KeyHelp, MailWindow, MenuAction, MenuChoice, MenuState,
    PageInfo, Pane, ProposalData, Remote, RuleData, Sheet, StatusLine, Theme, ThemePreference,
    ToolButton,
};
use super::{body, icons, list, menu, rules, status, theme, toolbar};
use crate::message::clean;

/// What a callback asks the app: the actions it means, in order.
pub(crate) type Ask = Box<dyn FnOnce(&App) -> Vec<UiAction>>;

/// An `Ask` for at most one action.
fn one(f: impl FnOnce(&App) -> Option<UiAction> + 'static) -> Ask {
    Box::new(move |app| f(app).into_iter().collect())
}

/// What never changes while the window is open.
pub(crate) fn init(window: &MailWindow) {
    window.set_list_min_width(list::MIN_WIDTH);
    window.set_close_icon(icons::CLOSE.into());
    window.set_search_icon(icons::SEARCH.into());
    window.set_sync_icon(icons::SYNC.into());
    window.set_save_icon(icons::SAVE.into());
    window.set_source_icon(icons::SOURCE.into());
    window.set_search_hint(toolbar::SEARCH_HINT.into());
    window.set_sync_hint(status::sync_key().into());
    window.set_version(format!("v{}", status::VERSION).into());
    window.set_description(env!("CARGO_PKG_DESCRIPTION").into());
    window.set_license(env!("CARGO_PKG_LICENSE").into());
    window.set_repository(env!("CARGO_PKG_REPOSITORY").into());
    window.set_homepage(env!("CARGO_PKG_HOMEPAGE").into());
    let help = keys::HELP.map(|(keys, what)| KeyHelp {
        keys: keys.into(),
        what: what.into(),
    });
    window.set_keys(model(help));
}

/// Shows `app` in `window`. It runs after every callback and tick, so the models are updated in place.
pub(crate) fn present(app: &App, window: &MailWindow) {
    window
        .global::<Theme>()
        .set_preference(preference(app.theme));
    window.set_config_changed(app.config_changed);
    window.set_focused_pane(pane(app.focus));
    window.set_mail_view(app.view.lists_mail());
    let buttons = toolbar::buttons(app).map(|b| ToolButton {
        icon: b.icon.into(),
        name: b.name.into(),
        hint: b.hint.into(),
    });
    sync(window.get_tool_buttons(), buttons.into(), |m| {
        window.set_tool_buttons(m)
    });
    window.set_tool_buttons_enabled(toolbar::enabled(app));

    let folders = folders::entries(app).iter().map(folder_item).collect();
    sync(window.get_folders(), folders, |m| window.set_folders(m));
    let pinned = folders::pinned(app).iter().map(folder_row).collect();
    sync(window.get_pinned(), pinned, |m| window.set_pinned(m));

    let lines = status::lines(app).into_iter().map(status_line).collect();
    sync(window.get_status_lines(), lines, |m| {
        window.set_status_lines(m)
    });
    let update = app
        .newer_release()
        .map(status::update_text)
        .unwrap_or_default();
    window.set_update(update.into());

    present_list(app, window);
    present_body(app, window);
    present_sheet(app, window);

    let enabled = |item: menu::Item| item.action(app).is_some();
    window.set_menu(MenuState {
        save_eml: enabled(menu::Item::SaveEml),
        view_source: enabled(menu::Item::ViewSource),
        search: enabled(menu::Item::Search),
        message: toolbar::enabled(app),
        text_or_html: enabled(menu::Item::TextOrHtml),
        mark_folder_read: enabled(menu::Item::MarkFolderRead),
    });
    window.set_about_open(app.about_open);

    window.set_help_open(app.help_open);
    window.set_history_open(app.history_open);
    let history = status::history(app)
        .into_iter()
        .map(SharedString::from)
        .collect();
    sync(window.get_history(), history, |m| window.set_history(m));
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
    // Until the list has reported its height there is nothing to scroll within.
    if app.list.follow_cursor && app.list.viewport.1 > 0.0 {
        let (offset, height) = app.list.viewport;
        let offset = list::offset_showing(app.list.cursor, theme::LIST_ROW, offset, height);
        window.set_list_y(-offset);
    }
    window.set_move_open(app.move_picker.is_some());
    let targets = app
        .move_targets()
        .into_iter()
        .map(|name| SharedString::from(clean(name, false)));
    sync(window.get_move_targets(), targets.collect(), |m| {
        window.set_move_targets(m)
    });
}

fn present_body(app: &App, window: &MailWindow) {
    let pane = body::pane(app);
    let text = |value: Option<String>| SharedString::from(value.unwrap_or_default());
    let shown = window.get_body_info();
    let strings = |current, items: Vec<String>| {
        synced(current, items.into_iter().map(SharedString::from).collect())
    };
    let info = BodyInfo {
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
        attachments: strings(shown.attachments.clone(), pane.attachments),
        saved: text(pane.saved),
        links: strings(shown.links.clone(), pane.links),
    };
    if info != shown {
        window.set_body_info(info);
    }
    // Messages can be megabytes: set the text only when it changed.
    let body_text = app
        .body
        .as_ref()
        .and_then(|b| b.text.as_deref())
        .unwrap_or_default();
    if window.get_body_text() != body_text {
        window.set_body_text(body_text.into());
    }
    present_page(app, window);
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

fn present_page(app: &App, window: &MailWindow) {
    let view = app
        .body
        .as_ref()
        .filter(|b| b.shows_html())
        .and_then(|b| b.html_view.as_ref());
    let remote = match view {
        Some(v) if v.fetching.is_some() => Remote::Loading,
        Some(v) if !v.remote.is_empty() => Remote::NotLoaded,
        _ => Remote::None,
    };
    let (width, height) = view.and_then(|v| v.page).unwrap_or_default();
    let link = view
        .and_then(|v| v.hover.as_deref())
        .map(|href| clean(href, false));
    window.set_page(PageInfo {
        ready: view.is_some_and(|v| v.page.is_some()),
        width,
        height,
        remote,
        link: link.unwrap_or_default().into(),
    });
    let selection = view
        .and_then(|v| v.selection.as_deref())
        .unwrap_or_default();
    window.set_page_selection(selection.into());
    let strips = window.get_strips();
    match strips.as_any().downcast_ref::<PageModel>() {
        Some(model) => model.update(view),
        None => {
            let model = PageModel::default();
            model.update(view);
            window.set_strips(ModelRc::new(model));
        }
    }
}

fn present_sheet(app: &App, window: &MailWindow) {
    match app.view {
        View::Rules => {
            window.set_sheet(Sheet::Rules);
            window.set_rules_banner(rules::banner(&app.rules).unwrap_or_default().into());
            let proposals = rules::proposals(&app.rules)
                .into_iter()
                .map(|p| ProposalData {
                    title: p.title.into(),
                    toml: p.toml.into(),
                });
            sync(window.get_proposals(), proposals.collect(), |m| {
                window.set_proposals(m)
            });
            let lines = rules::rule_lines(&app.rules).into_iter().map(|r| RuleData {
                label: r.label.into(),
                enabled: r.enabled,
                scope: r.scope.into(),
                by: r.by.into(),
            });
            sync(window.get_rules(), lines.collect(), |m| window.set_rules(m));
        }
        View::Activity => {
            window.set_sheet(Sheet::Activity);
            let rows = rules::activity_cells(app);
            present_table(window, &rules::ACTIVITY_COLUMNS, &rows, app.sheet_sort);
            window.set_sheet_empty(rules::NO_ACTIVITY.into());
        }
        View::Trash => {
            window.set_sheet(Sheet::Backups);
            let rows = rules::trash_cells(app);
            present_table(window, &rules::TRASH_COLUMNS, &rows, app.sheet_sort);
            window.set_sheet_empty(rules::NO_TRASH.into());
        }
        View::AllInboxes | View::Folder { .. } => {}
    }
}

/// Puts a table's columns and rows, in `sort`'s order, into the window. Unchanged rows keep the chosen row, and
/// unchanged sorting keeps the widths the user dragged the columns to.
fn present_table<const N: usize>(
    window: &MailWindow,
    columns: &[rules::Column; N],
    rows: &[[String; N]],
    sort: Option<SheetSort>,
) {
    let sort_order = |column: usize| match sort {
        Some(SheetSort {
            column: c,
            ascending,
        }) if c == column => {
            if ascending {
                SortOrder::Ascending
            } else {
                SortOrder::Descending
            }
        }
        _ => SortOrder::Unsorted,
    };
    let shown = window.get_sheet_columns();
    let same_columns = shown.row_count() == N
        && shown
            .iter()
            .zip(columns)
            .enumerate()
            .all(|(i, (shown, (title, ..)))| {
                shown.title == *title && shown.sort_order == sort_order(i)
            });
    if !same_columns {
        let columns = columns
            .iter()
            .enumerate()
            .map(|(i, &(title, min_width, stretch))| {
                let mut column = TableColumn::default();
                column.title = title.into();
                column.min_width = min_width;
                column.horizontal_stretch = stretch;
                column.sort_order = sort_order(i);
                column
            });
        window.set_sheet_columns(model(columns));
    }
    let order = rules::sheet_order(rows, sort);
    let cells = window.get_sheet_cells();
    let same_rows = cells.row_count() == order.len()
        && cells.iter().zip(&order).all(|(shown, &i)| {
            shown
                .iter()
                .map(|item| item.text)
                .eq(rows[i].iter().map(SharedString::from))
        });
    if !same_rows {
        let item = |text: &String| StandardListViewItem::from(text.as_str());
        window.set_sheet_cells(model(
            order.iter().map(|&i| model(rows[i].iter().map(item))),
        ));
        window.set_sheet_row(-1);
    }
}

/// Hands the window's callbacks to `on`, which runs a function of the app and applies the action it returns.
pub(crate) fn connect(window: &MailWindow, on: fn(Ask) -> bool) {
    let act = move |action: UiAction| on(Box::new(move |_| vec![action]));
    window.on_key(move |text, command, alt| {
        let press = press(&text, command, alt);
        on(one(move |app| {
            keys::action(&press?, app.focus, app.typing())
        }))
    });
    window.on_tool(move |i| {
        on(one(move |app| {
            let enabled = toolbar::enabled(app);
            let button = nth(toolbar::buttons(app), i)?;
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
            let (_, enabled, action) = nth(folder_at(app, i)?.menu().into_iter().flatten(), j)?;
            enabled.then_some(action)
        }));
    });
    window.on_pinned_clicked(move |i| {
        on(one(move |app| {
            let pinned = nth(folders::pinned(app), i)?;
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
            let target = nth(app.move_targets(), i)?;
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
            let attachment = nth(&body.attachments, i)?;
            Some(UiAction::SaveAttachment(attachment.index))
        }));
    });
    window.on_open_link(move |url| {
        on(one(move |_| {
            body::has_target(&url).then(|| UiAction::OpenUrl(url.into()))
        }));
    });
    let weak = window.as_weak();
    window.on_page_width(move |width| {
        let scale = weak.upgrade().map_or(1.0, |w| w.window().scale_factor());
        act(UiAction::HtmlWidth {
            scale,
            width: width.max(html::MIN_WIDTH),
        });
    });
    window.on_page_visible(move |top, bottom| {
        act(UiAction::HtmlVisible { top, bottom });
    });
    window.on_page_hover(move |x, y| {
        on(one(move |app| {
            let at = Some([x, y]);
            (html_view(app)?.hovered_at != at).then_some(UiAction::HtmlHover(at))
        }));
    });
    window.on_page_leave(move || {
        on(one(|app| {
            html_view(app)?
                .hovered_at
                .is_some()
                .then_some(UiAction::HtmlHover(None))
        }));
    });
    window.on_page_select(move |from_x, from_y, to_x, to_y| {
        on(one(move |app| {
            let at = ([from_x, from_y], [to_x, to_y]);
            (html_view(app)?.selected_at != Some(at)).then_some(UiAction::HtmlSelect {
                from: at.0,
                to: at.1,
            })
        }));
    });
    window.on_page_clicked(move |x, y| {
        on(Box::new(move |app| {
            let Some(view) = html_view(app) else {
                return Vec::new();
            };
            let mut actions = Vec::new();
            // A click clears the selection.
            if view.selection.is_some() {
                actions.push(UiAction::HtmlSelect {
                    from: [x, y],
                    to: [x, y],
                });
            }
            if let Some(url) = view.hover.as_deref().and_then(html::openable) {
                actions.push(UiAction::OpenUrl(url.to_string()));
            }
            actions
        }));
    });
    window.on_page_open_link(move || {
        on(one(|app| {
            let url = html::openable(html_view(app)?.hover.as_deref()?)?;
            Some(UiAction::OpenUrl(url.to_string()))
        }));
    });
    window.on_load_remote(move || {
        act(UiAction::LoadRemote);
    });
    window.on_menu_item(move |action| {
        on(one(move |app| menu_item(action).action(app)));
    });
    window.on_open_rules_file(move || {
        act(UiAction::OpenRulesFile);
    });
    window.on_approve_rule(move |i| {
        on(one(move |app| {
            Some(UiAction::ApproveRule(proposal(app, i)?))
        }));
    });
    window.on_reject_rule(move |i| {
        on(one(move |app| {
            Some(UiAction::RejectRule(proposal(app, i)?))
        }));
    });
    window.on_set_enabled(move |i, enabled| {
        on(one(move |app| {
            let rule = nth(rules::rule_lines(&app.rules), i)?;
            Some(UiAction::SetRuleEnabled(rule.name, enabled))
        }));
    });
    window.on_restore(move |i| {
        on(one(move |app| {
            let order = rules::sheet_order(&rules::trash_cells(app), app.sheet_sort);
            let row = &app.trash[*nth(&order, i)?];
            Some(UiAction::Restore(row.account, row.entry.path.clone()))
        }));
    });
    window.on_sort_sheet(move |column, ascending| {
        if let Ok(column) = usize::try_from(column) {
            act(UiAction::SortSheet(SheetSort { column, ascending }));
        }
    });
    window.on_open_update(move || {
        on(one(|app| {
            let version = app.newer_release()?;
            Some(UiAction::OpenUrl(crate::update::release_url(version)))
        }));
    });
}

/// The `index`th of `items`, as the window's callbacks number them.
fn nth<T>(items: impl IntoIterator<Item = T>, index: i32) -> Option<T> {
    items.into_iter().nth(usize::try_from(index).ok()?)
}

/// The name of the proposal the Rules view shows at `index`.
fn proposal(app: &App, index: i32) -> Option<String> {
    Some(nth(rules::proposals(&app.rules), index)?.name)
}

fn menu_item(action: MenuAction) -> menu::Item {
    use menu::Item;
    match action {
        MenuAction::About => Item::About,
        MenuAction::AddToSelection => Item::AddToSelection,
        MenuAction::Archive => Item::Archive,
        MenuAction::Delete => Item::Delete,
        MenuAction::Documentation => Item::Documentation,
        MenuAction::Flag => Item::Flag,
        MenuAction::History => Item::History,
        MenuAction::Keys => Item::Keys,
        MenuAction::MarkFolderRead => Item::MarkFolderRead,
        MenuAction::Move => Item::Move,
        MenuAction::ReadOrUnread => Item::ReadOrUnread,
        MenuAction::SaveEml => Item::SaveEml,
        MenuAction::Search => Item::Search,
        MenuAction::SyncNow => Item::SyncNow,
        MenuAction::TextOrHtml => Item::TextOrHtml,
        MenuAction::ThemeDark => Item::Theme(config::Theme::Dark),
        MenuAction::ThemeLight => Item::Theme(config::Theme::Light),
        MenuAction::ThemeSystem => Item::Theme(config::Theme::System),
        MenuAction::ViewSource => Item::ViewSource,
    }
}

/// The shown message's HTML view.
fn html_view(app: &App) -> Option<&HtmlState> {
    app.body.as_ref()?.html_view.as_ref()
}

/// The folder entry the pane shows at `index`.
fn folder_at(app: &App, index: i32) -> Option<FolderEntry> {
    match nth(folders::entries(app), index)? {
        Entry::Folder(folder) => Some(folder),
        _ => None,
    }
}

fn folder_item(entry: &Entry) -> FolderItem {
    match entry {
        Entry::Folder(folder) => folder_row(folder),
        Entry::Account {
            name,
            busy,
            error,
            gap,
        } => FolderItem {
            kind: FolderKind::Account,
            label: name.into(),
            busy: *busy,
            gap: *gap,
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
    let [head, tail] = folder.menu();
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
        heading: folder.view == Some(View::AllInboxes),
        caret_name: folder.caret_name().unwrap_or_default().into(),
        menu_head: choices(&head),
        menu_tail: choices(&tail),
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
