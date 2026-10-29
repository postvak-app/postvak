//! State into the window and the window's callbacks back into `UiAction`s. `present` sets the window's properties and
//! models from `&App`; `connect` hands each callback to `super::on`, which asks the app what it means and applies it.
//! Nothing here changes state.
use slint::language::{SortOrder, StandardListViewItem, TableColumn};
use slint::{ComponentHandle, Model, ModelRc, SharedString};

use postvak_core::config;

use super::draft::{self, ACTIONS, Condition, Draft, Field, Op};
use super::folders::{self, Entry, FolderEntry};
use super::html::{self, HtmlState};
use super::keys::{self, Key, Named, Press};
use super::rows::{ListModel, PageModel, model, sync, synced};
use super::rules::SheetSort;
use super::state::{self, App, Focus, UiAction, View};
use super::ui::{
    AccountFields, ActionData, BodyInfo, ConditionData, Dialog, Dot, FolderItem, FolderKind,
    KeyHelp, MailWindow, MenuAction, MenuChoice, MenuState, PageInfo, Pane, ProposalData, Remote,
    RuleData, RuleEditor, SelectedData, Sheet, StatusLine, Theme, ThemePreference, ToolButton,
};
use super::{account, body, icons, list, menu, rules, status, theme, toolbar};
use postvak_core::message::clean;

/// What a callback asks the app: the actions it means, in order.
pub(crate) type Ask = Box<dyn FnOnce(&App) -> Vec<UiAction>>;

/// An `Ask` for at most one action.
fn one(f: impl FnOnce(&App) -> Option<UiAction> + 'static) -> Ask {
    Box::new(move |app| f(app).into_iter().collect())
}

/// The form's fields as the window sent them.
pub(crate) fn account_fields(fields: AccountFields) -> account::Fields {
    account::Fields {
        name: fields.name.into(),
        host: fields.host.into(),
        port: fields.port.into(),
        username: fields.username.into(),
        password: fields.password.into(),
        address: fields.address.into(),
        starttls: fields.starttls,
    }
}

/// What never changes while the window is open.
pub(crate) fn init(window: &MailWindow) {
    window.set_list_min_width(list::MIN_WIDTH);
    init_editor(window);
    window.set_close_icon(icons::CLOSE.into());
    window.set_search_icon(icons::SEARCH.into());
    window.set_filter_icon(icons::FILTER.into());
    window.set_rule_icon(icons::RULES.into());
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
        label: b.label.into(),
    });
    sync(window.get_tool_buttons(), buttons.into(), |m| {
        window.set_tool_buttons(m)
    });
    window.set_tool_buttons_enabled(toolbar::enabled(app));

    let folders = folders::entries(app).iter().map(folder_item).collect();
    sync_folders(window.get_folders(), folders, |m| window.set_folders(m));
    let pinned = folders::pinned(app).iter().map(folder_row).collect();
    sync_folders(window.get_pinned(), pinned, |m| window.set_pinned(m));

    let line = status_line(status::summary(app));
    if window.get_status_line() != line {
        window.set_status_line(line);
    }
    window.set_status_detail(status::detail(app).into());
    let update = app
        .newer_release()
        .map(status::update_text)
        .unwrap_or_default();
    window.set_update(update.into());

    present_list(app, window);
    present_body(app, window);
    present_sheet(app, window);
    present_editor(app, window);

    let enabled = |item: MenuAction| menu::action(item, app).is_some();
    window.set_menu(MenuState {
        save_eml: enabled(MenuAction::SaveEml),
        view_source: enabled(MenuAction::ViewSource),
        search: enabled(MenuAction::Search),
        message: toolbar::enabled(app),
        text_or_html: enabled(MenuAction::TextOrHtml),
        original_colours: enabled(MenuAction::OriginalColours),
        shows_original_colours: app.body.as_ref().is_some_and(|b| b.original_colours),
        mark_folder_read: enabled(MenuAction::MarkFolderRead),
        open_at_login: enabled(MenuAction::OpenAtLogin),
        opens_at_login: app.open_at_login.checked(),
    });
    window.set_about_open(app.about_open);
    let apply_confirm = app.apply_confirm.as_ref().map(|c| c.warning.as_str());
    window.set_apply_confirm(apply_confirm.unwrap_or_default().into());
    let form = app.account_form.as_ref();
    window.set_account_open(form.is_some());
    let error = form
        .and_then(|form| form.error.as_deref())
        .unwrap_or_default();
    window.set_account_error(error.into());
    window.set_account_saving(form.is_some_and(|form| form.saving.is_some()));
    let testing = form.is_some_and(|form| form.testing);
    window.set_account_progress(account::progress(testing).into());

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
    window.set_filter_label(list::filter_label(app.filter).unwrap_or_default().into());
    let filter = list::FILTERS.iter().position(|(_, f)| *f == app.filter);
    window.set_list_filter(
        filter
            .and_then(|i| i32::try_from(i).ok())
            .unwrap_or_default(),
    );
    window.set_searching(app.search.is_some());
    window.set_search_hint_text(list::search_hint(&app.view).into());
    window.set_results(list::results(app).into());
    let empty = if app.list.rows.is_empty() {
        list::empty(&app.view, app.search.as_deref(), app.filter)
    } else {
        String::new()
    };
    window.set_list_empty(empty.into());
    window.set_marked_count(i32::try_from(app.list.marked.len()).unwrap_or(i32::MAX));
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
        let above = match list::divider(&app.list.rows) {
            Some(first) if first <= app.list.cursor => theme::ROW,
            _ => 0.0,
        };
        let offset = list::offset_showing(app.list.cursor, theme::LIST_ROW, above, offset, height);
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
    // Drawn once per logo: the shown image is kept while the record is the same. A logo the window cannot draw shows
    // the initials.
    let logo_key = pane
        .logo
        .as_ref()
        .map(|(key, _)| key.as_str())
        .unwrap_or_default();
    let logo = if shown.logo_key == logo_key {
        shown.has_logo.then(|| shown.logo.clone())
    } else {
        pane.logo
            .as_ref()
            .and_then(|(_, svg)| slint::Image::load_from_svg_data(svg.as_bytes()).ok())
    };
    let info = BodyInfo {
        empty: pane.empty.unwrap_or_default().into(),
        subject: text(pane.subject),
        initials: pane.initials.into(),
        tint: i32::try_from(pane.tint).unwrap_or_default(),
        has_logo: logo.is_some(),
        logo: logo.unwrap_or_default(),
        logo_key: logo_key.into(),
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
        adapts: pane.adapts,
        original_colours: pane.original_colours,
        attachments: strings(shown.attachments.clone(), pane.attachments),
        attachments_pending: pane.attachments_pending,
        saved: text(pane.saved),
        unsubscribe: pane.unsubscribe,
        links: strings(shown.links.clone(), pane.links),
    };
    if info != shown {
        window.set_body_info(info);
    }
    let (title, selected) = body::selection(app).unwrap_or_default();
    window.set_selection_title(title.into());
    let selected = selected.into_iter().map(|s| SelectedData {
        who: s.who.into(),
        subject: s.subject.into(),
        date: s.date.into(),
        unread: s.unread,
    });
    sync(window.get_selection(), selected.collect(), |m| {
        window.set_selection(m)
    });
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
        dark: view.is_some_and(|v| v.dark),
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

/// The accounts the editor offers: every account, then each by name, then the draft's own when no longer listed.
fn draft_accounts(app: &App) -> Vec<String> {
    let mut names: Vec<String> = app.accounts.iter().map(|a| a.name.clone()).collect();
    if let Some(own) = app.rule_draft.as_ref().and_then(|d| d.account.as_ref())
        && !names.contains(own)
    {
        names.push(own.clone());
    }
    names
}

/// Whether the open rule can run on existing mail: saved as it is, and on.
fn draft_applies(app: &App) -> bool {
    app.rule_draft
        .as_ref()
        .is_some_and(|d| d.original.is_some() && d.enabled && !app.draft_edited())
}

/// The editor's fixed choices, which never change while the window is open.
fn init_editor(window: &MailWindow) {
    let editor = window.global::<RuleEditor>();
    let labels = |items: Vec<&str>| model(items.into_iter().map(SharedString::from));
    editor.set_fields(labels(Field::ALL.map(Field::label).to_vec()));
    editor.set_ops(labels(Op::ALL.map(Op::label).to_vec()));
    editor.set_action_choices(labels(ACTIONS.iter().map(draft::action_name).collect()));
}

fn present_editor(app: &App, window: &MailWindow) {
    let editor = window.global::<RuleEditor>();
    // The prompt can show over the reader, when its Rule button would replace an edited rule.
    editor.set_unsaved(app.leave_draft.is_some());
    let draft = app.rule_draft.as_ref().filter(|_| app.view == View::Rules);
    let Some(draft) = draft else {
        editor.set_open(false);
        return;
    };
    let accounts = draft_accounts(app);
    let account = draft
        .account
        .as_ref()
        .and_then(|own| accounts.iter().position(|name| name == own))
        .map_or(0, |i| i + 1);
    let mut choices = vec!["Every account".to_string()];
    choices.extend(accounts.iter().map(|name| clean(name, false)));
    let conditions: Vec<ConditionData> = draft
        .conditions
        .iter()
        .map(|c| ConditionData {
            field: index_of(&Field::ALL, &c.field),
            header: clean(&c.header, false).into(),
            op: index_of(&Op::ALL, &c.op),
            value: clean(&c.value, false).into(),
            text: c.field.is_text(),
            flag: c.field.is_flag(),
            named: c.field == Field::Header,
        })
        .collect();
    let actions: Vec<ActionData> = draft
        .actions
        .iter()
        .map(|action| {
            let value = match action {
                postvak_core::rules::Action::Move(v) | postvak_core::rules::Action::Tag(v) => {
                    Some(v)
                }
                _ => None,
            };
            ActionData {
                name: draft::action_name(action).into(),
                label: clean(&draft::action_label(action), false).into(),
                value: value.map(|v| clean(v, false)).unwrap_or_default().into(),
                takes_value: value.is_some(),
                note: if *action == postvak_core::rules::Action::Delete {
                    "keeps a .eml backup".into()
                } else {
                    SharedString::new()
                },
            }
        })
        .collect();
    let kept: Vec<SharedString> = draft::match_labels(&draft.kept)
        .iter()
        .map(|label| clean(label, false).into())
        .collect();
    editor.set_open(true);
    editor.set_name(clean(&draft.name, false).into());
    editor.set_enabled(draft.enabled);
    editor.set_account(i32::try_from(account).unwrap_or(0));
    let choices = choices.into_iter().map(SharedString::from).collect();
    sync(editor.get_accounts(), choices, |m| editor.set_accounts(m));
    editor.set_folder(clean(draft.folder.as_deref().unwrap_or_default(), false).into());
    sync(editor.get_kept(), kept, |m| editor.set_kept(m));
    sync(editor.get_conditions(), conditions, |m| {
        editor.set_conditions(m)
    });
    sync(editor.get_actions(), actions, |m| editor.set_actions(m));
    editor.set_renaming(draft.renames());
    editor.set_error(draft.error.as_deref().unwrap_or_default().into());
    editor.set_tested(draft.tested.as_deref().unwrap_or_default().into());
    editor.set_confirm_remove(draft.confirm_remove);
    editor.set_applies(draft_applies(app));
    editor.set_saved(draft.original.is_some());
}

fn index_of<T: PartialEq>(all: &[T], item: &T) -> i32 {
    all.iter()
        .position(|x| x == item)
        .and_then(|i| i32::try_from(i).ok())
        .unwrap_or(0)
}

/// An `Ask` that changes a copy of the open rule and hands it back when it differs.
fn edit_draft(change: impl FnOnce(&App, &mut Draft) + 'static) -> Ask {
    one(move |app| {
        let mut draft = app.rule_draft.clone()?;
        change(app, &mut draft);
        (Some(&draft) != app.rule_draft.as_ref()).then(|| UiAction::EditDraft(Box::new(draft)))
    })
}

/// Removes the `index`th of `items`, as the window's callbacks number them, when there is one.
fn remove_nth<T>(items: &mut Vec<T>, index: i32) {
    if let Ok(index) = usize::try_from(index)
        && index < items.len()
    {
        items.remove(index);
    }
}

/// The open rule's row `index`, when there is one.
fn condition(draft: &mut Draft, index: i32) -> Option<&mut Condition> {
    draft.conditions.get_mut(usize::try_from(index).ok()?)
}

fn connect_editor(window: &MailWindow, on: fn(Ask) -> bool) {
    let editor = window.global::<RuleEditor>();
    let act = move |action: UiAction| on(Box::new(move |_| vec![action]));
    editor.on_name_edited(move |text| {
        on(edit_draft(move |_, d| d.name = text.into()));
    });
    editor.on_enabled_toggled(move |enabled| {
        on(edit_draft(move |_, d| d.enabled = enabled));
    });
    editor.on_account_chosen(move |i| {
        on(edit_draft(move |app, d| {
            d.account = usize::try_from(i - 1)
                .ok()
                .and_then(|i| draft_accounts(app).into_iter().nth(i));
        }));
    });
    editor.on_folder_edited(move |text| {
        on(edit_draft(move |_, d| {
            d.folder = (!text.trim().is_empty()).then(|| text.into());
        }));
    });
    editor.on_field_chosen(move |row, i| {
        on(edit_draft(move |_, d| {
            if let (Some(field), Some(c)) = (nth(Field::ALL, i), condition(d, row)) {
                *c = c.with_field(field);
            }
        }));
    });
    editor.on_op_chosen(move |row, i| {
        on(edit_draft(move |_, d| {
            if let (Some(op), Some(c)) = (nth(Op::ALL, i), condition(d, row)) {
                c.op = op;
            }
        }));
    });
    editor.on_header_edited(move |row, text| {
        on(edit_draft(move |_, d| {
            if let Some(c) = condition(d, row) {
                c.header = text.into();
            }
        }));
    });
    editor.on_value_edited(move |row, text| {
        on(edit_draft(move |_, d| {
            if let Some(c) = condition(d, row) {
                c.value = text.into();
            }
        }));
    });
    editor.on_add_condition(move || {
        on(edit_draft(|_, d| {
            let unused = Field::ALL
                .into_iter()
                .find(|f| d.conditions.iter().all(|c| c.field != *f));
            d.conditions
                .push(Condition::new(unused.unwrap_or(Field::From)));
        }));
    });
    editor.on_remove_condition(move |row| {
        on(edit_draft(move |_, d| remove_nth(&mut d.conditions, row)));
    });
    editor.on_action_chosen(move |i| {
        on(edit_draft(move |_, d| {
            d.actions.extend(nth(ACTIONS, i));
        }));
    });
    editor.on_action_value_edited(move |i, text| {
        on(edit_draft(move |_, d| {
            let action = usize::try_from(i).ok().and_then(|i| d.actions.get_mut(i));
            if let Some(
                postvak_core::rules::Action::Move(value) | postvak_core::rules::Action::Tag(value),
            ) = action
            {
                *value = text.into();
            }
        }));
    });
    editor.on_remove_action(move |i| {
        on(edit_draft(move |_, d| remove_nth(&mut d.actions, i)));
    });
    editor.on_cancel_remove(move || {
        on(edit_draft(|_, d| d.confirm_remove = false));
    });
    editor.on_save(move || {
        act(UiAction::SaveDraft);
    });
    editor.on_test(move || {
        act(UiAction::TestDraft);
    });
    editor.on_apply(move || {
        on(one(|app| {
            let name = app.rule_draft.as_ref()?.original.clone()?;
            draft_applies(app).then_some(UiAction::ApplyRule(name))
        }));
    });
    editor.on_remove(move |confirmed| {
        act(UiAction::RemoveRule { confirmed });
    });
    editor.on_unsaved_save(move || {
        act(UiAction::UnsavedSave);
    });
    editor.on_unsaved_discard(move || {
        act(UiAction::UnsavedDiscard);
    });
    editor.on_unsaved_cancel(move || {
        act(UiAction::UnsavedCancel);
    });
}

fn present_sheet(app: &App, window: &MailWindow) {
    match app.view {
        View::Rules => {
            window.set_sheet(Sheet::Rules);
            window.set_rules_banner(rules::banner(&app.rules).unwrap_or_default().into());
            let offers: Vec<SharedString> = app
                .rules
                .offers
                .iter()
                .map(|(account, id)| rules::offer_label(account, id).into())
                .collect();
            sync(window.get_rules_offers(), offers, |m| {
                window.set_rules_offers(m)
            });
            let proposals: Vec<ProposalData> = app
                .rules
                .proposals
                .iter()
                .map(|p| ProposalData {
                    label: p.label.as_str().into(),
                    by: p.by.as_str().into(),
                    summary: p.summary.as_str().into(),
                    toml: p.toml.as_str().into(),
                    marks: p.marks.as_str().into(),
                    preview: app
                        .rules
                        .previews
                        .get(&p.name)
                        .map_or_else(SharedString::new, Into::into),
                })
                .collect();
            let lines: Vec<RuleData> = rules::rule_order(&app.rules.lines, app.sheet_sort)
                .into_iter()
                .map(|i| (i, &app.rules.lines[i]))
                .map(|(i, r)| RuleData {
                    number: (i + 1).to_string().into(),
                    label: r.label.as_str().into(),
                    enabled: r.enabled,
                    account: r.account.as_str().into(),
                    folder: r.folder.as_str().into(),
                    by: r.by.as_str().into(),
                    conditions: r.conditions.as_str().into(),
                    actions: r.actions.as_str().into(),
                    selected: app
                        .rule_draft
                        .as_ref()
                        .is_some_and(|d| d.original.as_ref() == Some(&r.name)),
                })
                .collect();
            window.set_rules_counts(rules::counts(lines.len(), proposals.len()).into());
            let sort = app.sheet_sort;
            window.set_rules_sort(
                sort.and_then(|s| i32::try_from(s.column).ok())
                    .unwrap_or(-1),
            );
            window.set_rules_ascending(sort.is_none_or(|s| s.ascending));
            sync(window.get_proposals(), proposals, |m| {
                window.set_proposals(m)
            });
            sync(window.get_rules(), lines, |m| window.set_rules(m));
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
            window.set_purge_hint(rules::purge_hint(app).into());
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
    window.on_clear_selection(move || {
        act(UiAction::ClearSelection);
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
    window.on_folder_menu(move |i, group, j| {
        on(one(move |app| {
            let (_, enabled, action) = nth(nth(folder_at(app, i)?.menu(), group)?, j)?;
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
    window.on_row_clicked(move |i, extend, toggle| {
        let row = usize::try_from(i).unwrap_or_default();
        act(match (extend, toggle) {
            (true, _) => UiAction::ExtendSelection(row),
            (false, true) => UiAction::ToggleRowMark(row),
            (false, false) => UiAction::SelectRow(row),
        });
    });
    window.on_row_menu(move |i, group, j| {
        on(Box::new(move |app| {
            let Some((i, row)) = usize::try_from(i)
                .ok()
                .and_then(|i| Some((i, app.list.rows.get(i)?)))
            else {
                return Vec::new();
            };
            let Some((_, action)) = nth(
                nth(list::menu(row, app.shows_junk()), group).unwrap_or_default(),
                j,
            ) else {
                return Vec::new();
            };
            vec![UiAction::TargetRow(i), action]
        }));
    });
    window.on_drag_data(|row| slint::DataTransfer::from(SharedString::from(row.to_string())));
    window.on_drag_image(|count, subject, fill, ink| {
        let label = list::drag_label(usize::try_from(count).unwrap_or(1), &subject);
        drag_image(&label, fill, ink)
    });
    window.on_dragged_row(|data| {
        data.plain_text()
            .ok()
            .and_then(|text| text.parse().ok())
            .unwrap_or(-1)
    });
    window.on_folder_dropped(move |i, row| {
        on(Box::new(move |app| {
            let (Ok(row), Some(view)) =
                (usize::try_from(row), folder_at(app, i).and_then(|f| f.view))
            else {
                return Vec::new();
            };
            vec![UiAction::TargetRow(row), UiAction::DropOn(view)]
        }));
    });
    window.on_list_filter_clicked(move |i| {
        let filter = usize::try_from(i).ok().and_then(|i| list::FILTERS.get(i));
        if let Some((_, filter)) = filter {
            act(UiAction::SetFilter(*filter));
        }
    });
    window.on_search_edited(move |text| {
        act(UiAction::SearchFor(text.into()));
    });
    window.on_search_accepted(move || {
        act(UiAction::SearchAccepted);
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
    window.on_close_dialog(move |dialog| {
        act(UiAction::Close(dialog_of(dialog)));
    });
    window.on_toggle_html(move || {
        act(UiAction::ToggleHtml);
    });
    window.on_toggle_original_colours(move || {
        act(UiAction::ToggleOriginalColours);
    });
    window.on_save_eml(move || {
        act(UiAction::SaveEml);
    });
    window.on_unsubscribe(move || {
        act(UiAction::Unsubscribe);
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
    window.on_save_attachment_as(move |i| {
        on(one(move |app| {
            let body = app.body.as_ref()?;
            let attachment = nth(&body.attachments, i)?;
            Some(UiAction::SaveAttachmentAs(attachment.index))
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
    window.on_save_account(move |fields| {
        act(UiAction::SaveAccount(account_fields(fields)));
    });
    window.on_menu_item(move |action| {
        on(one(move |app| menu::action(action, app)));
    });
    window.on_open_rules_file(move || {
        act(UiAction::OpenRulesFile);
    });
    window.on_confirm_apply_rule(move || {
        act(UiAction::ConfirmApplyRule);
    });
    window.on_approve_rule(move |i| {
        on(one(move |app| {
            Some(UiAction::ApproveRule(proposal(app, i)?))
        }));
    });
    window.on_trust_authserv(move |i| {
        on(one(move |app| {
            let (account, id) = nth(&app.rules.offers, i)?.clone();
            Some(UiAction::TrustAuthserv { account, id })
        }));
    });
    window.on_reject_rule(move |i| {
        on(one(move |app| {
            Some(UiAction::RejectRule(proposal(app, i)?))
        }));
    });
    window.on_rule_from(move || {
        on(one(|app| {
            let message = app.body.as_ref()?.message.as_ref()?;
            Some(UiAction::RuleFrom(message.from_addr.clone()?))
        }));
    });
    window.on_new_rule(move || {
        act(UiAction::NewRule);
    });
    window.on_edit_rule(move |i| {
        on(one(move |app| {
            Some(UiAction::EditRule(rule_line(app, i)?.name.clone()))
        }));
    });
    connect_editor(window, on);
    window.on_preview_rule(move |i| {
        on(one(move |app| {
            Some(UiAction::PreviewRule(proposal(app, i)?))
        }));
    });
    window.on_set_enabled(move |i, enabled| {
        on(one(move |app| {
            let rule = rule_line(app, i)?;
            Some(UiAction::SetRuleEnabled(rule.name.clone(), enabled))
        }));
    });
    window.on_purge_trash(move || {
        act(UiAction::PurgeTrash);
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

/// The rule the Rules table shows at `index`, in its sorted order.
fn rule_line(app: &App, index: i32) -> Option<&rules::RuleLine> {
    let order = rules::rule_order(&app.rules.lines, app.sheet_sort);
    app.rules.lines.get(*nth(&order, index)?)
}

/// The name of the proposal the Rules view shows at `index`.
fn proposal(app: &App, index: i32) -> Option<String> {
    Some(nth(&app.rules.proposals, index)?.name.clone())
}

/// A pill with `label`, drawn from SVG with the window's own fonts.
fn drag_image(label: &str, fill: slint::Color, ink: slint::Color) -> slint::Image {
    let hex = |c: slint::Color| format!("#{:02x}{:02x}{:02x}", c.red(), c.green(), c.blue());
    // ponytail: width from an average glyph width, not measured; a long subject may get a little extra padding.
    let width = 24 + 7 * label.chars().count();
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="28"><rect width="{width}" height="28" rx="14" fill="{fill}"/><text x="{x}" y="18.5" text-anchor="middle" font-family="Hanken Grotesk" font-weight="600" font-size="13" fill="{ink}">{text}</text></svg>"#,
        x = width / 2,
        fill = hex(fill),
        ink = hex(ink),
        text = postvak_core::notify::escape_markup(label),
    );
    slint::Image::load_from_svg_data(svg.as_bytes()).unwrap_or_default()
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
            note,
            error,
            gap,
        } => FolderItem {
            kind: FolderKind::Account,
            label: name.into(),
            busy: *busy,
            note: note.unwrap_or_default().into(),
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

/// `sync` for folder rows. Their menus are models, which compare by identity, so a row keeps the shown menus while
/// they offer the same choices; otherwise every row would differ and the window draw again on every tick.
fn sync_folders(
    current: ModelRc<FolderItem>,
    mut items: Vec<FolderItem>,
    set: impl FnOnce(ModelRc<FolderItem>),
) {
    for (item, shown) in items.iter_mut().zip(current.iter()) {
        if item.menu_head.iter().eq(shown.menu_head.iter()) {
            item.menu_head = shown.menu_head;
        }
        if item.menu_tail.iter().eq(shown.menu_tail.iter()) {
            item.menu_tail = shown.menu_tail;
        }
    }
    sync(current, items, set);
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
        indent: i32::try_from(folder.indent()).unwrap_or_default(),
        path: folder.path.as_deref().unwrap_or_default().into(),
        selected: folder.selected,
        caret: folder.caret().unwrap_or_default().into(),
        caret_name: folder.caret_name().unwrap_or_default().into(),
        droppable: matches!(folder.view, Some(View::Folder { .. })),
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

fn dialog_of(dialog: Dialog) -> state::Dialog {
    match dialog {
        Dialog::Help => state::Dialog::Help,
        Dialog::Unsaved => state::Dialog::Unsaved,
        Dialog::ApplyConfirm => state::Dialog::ApplyConfirm,
        Dialog::Rule => state::Dialog::Rule,
        Dialog::Account => state::Dialog::Account,
        Dialog::About => state::Dialog::About,
        Dialog::Source => state::Dialog::Source,
        Dialog::Move => state::Dialog::Move,
        Dialog::History => state::Dialog::History,
    }
}

fn pane(focus: Focus) -> Pane {
    match focus {
        Focus::Body => Pane::Body,
        Focus::Folders => Pane::Folders,
        Focus::List => Pane::List,
    }
}
