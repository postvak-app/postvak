//! The message list: GPUI Kit's virtual `List` over the model's rows, with Postvak's single keys as GPUI actions in
//! the `MessageList` key context.
use chrono::{DateTime, Local};
use gpui::{
    App, Context, Entity, FontWeight, IntoElement, KeyBinding, ParentElement, Render,
    ScrollStrategy, SharedString, Styled, Subscription, Window, actions, div, prelude::*,
};
use gpui_kit::component::IndexPath;
use gpui_kit::component::list::{List, ListDelegate, ListEvent, ListItem, ListState};
use postvak::gui::list::columns;
use postvak::rules::Action;

use crate::model::Model;
use crate::theme;

actions!(
    postvak,
    [
        Next, Prev, Expand, Collapse, ToggleMark, Archive, ToggleHelp
    ]
);

pub const CONTEXT: &str = "MessageList";

/// The keys of `App::keys` for the list pane. Up, Down and Esc are GPUI Kit's own list bindings, which reach the same
/// model through the delegate.
pub fn bindings() -> Vec<KeyBinding> {
    let context = Some(CONTEXT);
    vec![
        KeyBinding::new("j", Next, context),
        KeyBinding::new("k", Prev, context),
        KeyBinding::new("right", Expand, context),
        KeyBinding::new("left", Collapse, context),
        KeyBinding::new("x", ToggleMark, context),
        KeyBinding::new("e", Archive, context),
        KeyBinding::new("?", ToggleHelp, context),
        KeyBinding::new("shift-/", ToggleHelp, context),
    ]
}

/// What each action does, for the help table; its keys come from the keymap.
pub const HELP: [(&str, &dyn gpui::Action); 7] = [
    ("Next message", &Next),
    ("Previous message", &Prev),
    ("Open thread", &Expand),
    ("Close thread", &Collapse),
    ("Mark", &ToggleMark),
    ("Archive", &Archive),
    ("Keys", &ToggleHelp),
];

pub struct Rows {
    pub model: Model,
    now: DateTime<Local>,
}

impl ListDelegate for Rows {
    type Item = ListItem;

    fn items_count(&self, _: usize, _: &App) -> usize {
        self.model.rows.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) -> Option<ListItem> {
        let row = self.model.rows.get(ix.row)?;
        let account =
            (self.model.shown.len() > 1).then(|| self.model.accounts[row.account].name.as_str());
        let marked = self.model.marked.contains(&row.id());
        let c = columns(row, account, marked, false, false, &self.now);
        let selected = ix.row == self.model.cursor;
        let (fg, muted) = if selected {
            (theme::on_accent(), theme::on_accent())
        } else {
            (theme::text(), theme::muted())
        };
        let marker = if c.marked {
            "✓"
        } else if c.flagged {
            "⚑"
        } else {
            ""
        };
        let label = format!(
            "{}{}{}, {}, {}",
            if c.unread { "Unread, " } else { "" },
            if c.flagged { "flagged, " } else { "" },
            c.who,
            c.subject,
            c.date
        );
        let top = div()
            .flex()
            .items_center()
            .gap(theme::PAD)
            .child(
                div()
                    .w_3()
                    .text_color(theme::accent())
                    .child(if c.unread { "•" } else { "" }),
            )
            .child(div().w_5().text_color(theme::accent()).child(marker))
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .when(c.member, |d| d.pl(theme::INSET))
                    .when(c.unread, |d| d.font_weight(FontWeight::SEMIBOLD))
                    .when_some(c.account.clone(), |d, account| {
                        d.child(format!("{account} · "))
                    })
                    .child(c.who.clone()),
            )
            .child(
                div()
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(muted)
                    .child(c.date.clone()),
            );
        let bottom = div()
            .pl(gpui::px(44.))
            .overflow_hidden()
            .text_ellipsis()
            .whitespace_nowrap()
            .text_color(muted)
            .child(c.subject.clone());
        Some(
            ListItem::new(("row", ix.row))
                .accessibility_label(SharedString::from(label))
                .p_0()
                .child(
                    div()
                        .h(theme::LIST_ROW)
                        .px(theme::INSET)
                        .flex()
                        .flex_col()
                        .justify_center()
                        .border_b_1()
                        .border_color(theme::border())
                        .text_color(fg)
                        .text_sm()
                        .child(top)
                        .child(bottom),
                ),
        )
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) {
        // Esc in the Kit list clears its selection; the model keeps its cursor and `MessageList` puts it back.
        if let Some(ix) = ix {
            self.model.select(ix.row);
        }
    }

    fn cancel(&mut self, _: &mut Window, cx: &mut Context<ListState<Self>>) {
        self.model.escape();
        cx.notify();
    }
}

pub struct MessageList {
    pub list: Entity<ListState<Rows>>,
    _cancel: Subscription,
}

impl MessageList {
    pub fn new(model: Model, window: &mut Window, cx: &mut Context<Self>) -> MessageList {
        let rows = Rows {
            model,
            now: Local::now(),
        };
        let list = cx.new(|cx| ListState::new(rows, window, cx));
        let _cancel = cx.subscribe_in(&list, window, |this, _, event, window, cx| {
            if matches!(event, ListEvent::Cancel) {
                this.change(window, cx, |_| {});
            }
        });
        let mut this = MessageList { list, _cancel };
        this.change(window, cx, |_| {});
        this
    }

    /// Runs `f` on the model, then puts the Kit list's selection on the model's cursor and scrolls to it.
    pub fn change(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Model),
    ) {
        self.list.update(cx, |list, cx| {
            f(&mut list.delegate_mut().model);
            let cursor = IndexPath::new(list.delegate().model.cursor);
            list.set_selected_index(Some(cursor), window, cx);
            list.scroll_to_item(cursor, ScrollStrategy::Nearest, window, cx);
            cx.notify();
        });
        cx.notify();
    }

    fn next(&mut self, _: &Next, window: &mut Window, cx: &mut Context<Self>) {
        self.change(window, cx, |m| m.step(1));
    }

    fn prev(&mut self, _: &Prev, window: &mut Window, cx: &mut Context<Self>) {
        self.change(window, cx, |m| m.step(-1));
    }

    fn expand(&mut self, _: &Expand, window: &mut Window, cx: &mut Context<Self>) {
        self.change(window, cx, Model::expand);
    }

    fn collapse(&mut self, _: &Collapse, window: &mut Window, cx: &mut Context<Self>) {
        self.change(window, cx, Model::collapse);
    }

    fn toggle_mark(&mut self, _: &ToggleMark, window: &mut Window, cx: &mut Context<Self>) {
        self.change(window, cx, Model::toggle_mark);
    }

    fn archive(&mut self, _: &Archive, window: &mut Window, cx: &mut Context<Self>) {
        self.change(window, cx, |m| m.act(Action::Archive));
    }

    fn toggle_help(&mut self, _: &ToggleHelp, window: &mut Window, cx: &mut Context<Self>) {
        self.change(window, cx, |m| m.help = !m.help);
    }

    /// The help table, each action's keys read from the keymap.
    fn help(&self, window: &Window) -> impl IntoElement {
        let rows = HELP.iter().map(|(what, action)| {
            let keys: Vec<String> = window
                .bindings_for_action(*action)
                .iter()
                .map(|binding| {
                    binding
                        .keystrokes()
                        .iter()
                        .map(|k| k.unparse())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect();
            div()
                .flex()
                .gap(theme::INSET)
                .child(
                    div()
                        .w_32()
                        .font_family(theme::MONO_FONT)
                        .child(keys.join(", ")),
                )
                .child(*what)
        });
        div()
            .absolute()
            .top(theme::INSET)
            .right(theme::INSET)
            .p(theme::INSET)
            .bg(theme::background())
            .border_1()
            .border_color(theme::border())
            .text_color(theme::text())
            .text_sm()
            .child(
                div()
                    .font_family(theme::SERIF_FONT)
                    .text_color(theme::heading())
                    .text_lg()
                    .child("Keys"),
            )
            .children(rows)
    }
}

impl Render for MessageList {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let help = self.list.read(cx).delegate().model.help;
        div()
            .key_context(CONTEXT)
            .on_action(cx.listener(Self::next))
            .on_action(cx.listener(Self::prev))
            .on_action(cx.listener(Self::expand))
            .on_action(cx.listener(Self::collapse))
            .on_action(cx.listener(Self::toggle_mark))
            .on_action(cx.listener(Self::archive))
            .on_action(cx.listener(Self::toggle_help))
            .relative()
            .size_full()
            .bg(theme::background())
            .font_family(theme::TEXT_FONT)
            .child(List::new(&self.list))
            .when(help, |d| d.child(self.help(window)))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use gpui::{TestAppContext, VisualTestContext};
    use postvak::sync::Command;

    use super::*;

    #[gpui::test]
    fn keys_move_the_cursor_open_a_thread_and_archive_it(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.bind_keys(bindings());
        });
        let dir = tempfile::tempdir().unwrap();
        let home = postvak::gui::fixture::mailbox(dir.path(), "light").unwrap();
        let model = Model::open(&home, vec![1]).unwrap();
        let (view, cx) = cx.add_window_view(|window, cx| MessageList::new(model, window, cx));
        cx.update(|window, cx| {
            let rows = view.read(cx).list.clone();
            rows.update(cx, |rows, cx| rows.focus(window, cx));
        });
        let state = |cx: &mut VisualTestContext| {
            view.read_with(cx, |v, cx| {
                let m = &v.list.read(cx).delegate().model;
                (
                    m.cursor,
                    m.rows.len(),
                    m.rows.get(m.cursor).map(|r| r.count),
                )
            })
        };
        cx.simulate_keystrokes("j");
        // Row 1 is the offsite thread: two messages.
        assert_eq!(state(cx), (1, 5, Some(2)));
        cx.simulate_keystrokes("right");
        assert_eq!(state(cx).1, 7);
        cx.simulate_keystrokes("left e");
        let sent: Vec<_> = view.read_with(cx, |v, cx| {
            v.list
                .read(cx)
                .delegate()
                .model
                .commands
                .try_iter()
                .collect()
        });
        let [(_, Command::Apply { uids, .. })] = sent.as_slice() else {
            panic!("expected one Apply, got {sent:?}");
        };
        assert_eq!(uids.as_slice(), &[93, 94]);
        assert_eq!(state(cx).1, 4);
        cx.simulate_keystrokes("k down down");
        assert_eq!(state(cx).0, 2, "Up and Down are the Kit list's own keys");
        let help = |cx: &mut VisualTestContext| {
            view.read_with(cx, |v, cx| v.list.read(cx).delegate().model.help)
        };
        cx.simulate_keystrokes("?");
        assert!(help(cx), "? opens the key table");
        let keys = cx.update(|window, _| {
            window
                .bindings_for_action(&Next)
                .iter()
                .map(|b| {
                    b.keystrokes()
                        .iter()
                        .map(|k| k.unparse())
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        });
        assert_eq!(keys, [["j"]], "the table reads its keys from the keymap");
        cx.simulate_keystrokes("escape");
        assert!(!help(cx));
    }
}
