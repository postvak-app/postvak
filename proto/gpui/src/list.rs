//! The message list: GPUI Kit's virtual `List` over the model's rows, with Postvak's single keys as GPUI actions in
//! the `MessageList` key context.
use chrono::{DateTime, Local};
use gpui::{
    App, Context, Entity, Focusable, FontWeight, IntoElement, KeyBinding, ParentElement, Render,
    ScrollStrategy, Styled, Window, actions, div, prelude::*,
};
use gpui_kit::component::IndexPath;
use gpui_kit::component::list::{List, ListDelegate, ListItem, ListState};
use postvak::gui::list::columns;
use postvak::rules::Action;

use crate::model::Model;
use crate::theme;

actions!(
    postvak,
    [
        Next, Prev, Expand, Collapse, ToggleMark, Archive, ToggleHelp, Escape
    ]
);

const CONTEXT: &str = "MessageList";

/// The keys of `App::keys` for the list pane. Up and Down are GPUI Kit's own list bindings, which reach the same model
/// through the delegate. Esc is bound in the Kit list's own context, after Kit's, so it replaces Kit's Cancel there,
/// which would clear the selection.
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
        KeyBinding::new("escape", Escape, Some("List")),
    ]
}

/// What each action does, for the help table; its keys come from the keymap.
const HELP: [(&str, &dyn gpui::Action); 8] = [
    ("Next message", &Next),
    ("Previous message", &Prev),
    ("Open thread", &Expand),
    ("Close thread", &Collapse),
    ("Mark", &ToggleMark),
    ("Archive", &Archive),
    ("Keys", &ToggleHelp),
    ("Close, clear marks", &Escape),
];

pub struct Rows {
    model: Entity<Model>,
    now: DateTime<Local>,
}

impl ListDelegate for Rows {
    type Item = ListItem;

    fn items_count(&self, _: usize, cx: &App) -> usize {
        self.model.read(cx).rows.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<ListItem> {
        let model = self.model.read(cx);
        let row = model.rows.get(ix.row)?;
        let account = (model.shown.len() > 1).then(|| model.accounts[row.account].name.as_str());
        let marked = model.marked.contains(&row.id());
        let c = columns(row, account, marked, false, false, &self.now);
        let (fg, muted) = if ix.row == model.cursor {
            (theme::on_accent(), theme::on_accent())
        } else {
            (theme::text(), theme::muted())
        };
        let marker = match (c.marked, c.flagged) {
            (true, _) => "✓",
            (_, true) => "⚑",
            _ => "",
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
                    .w(theme::DOT)
                    .text_color(theme::accent())
                    .child(if c.unread { "•" } else { "" }),
            )
            .child(
                div()
                    .w(theme::MARKER)
                    .text_color(theme::accent())
                    .child(marker),
            )
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .when(c.member, |d| d.pl(theme::INSET))
                    .when(c.unread, |d| d.font_weight(FontWeight::SEMIBOLD))
                    .when_some(c.account, |d, account| d.child(format!("{account} · ")))
                    .child(c.who),
            )
            .child(
                div()
                    .font_family(theme::MONO_FONT)
                    .text_xs()
                    .text_color(muted)
                    .child(c.date),
            );
        // The subject starts under the sender: past the dot, the marker and their gaps.
        let bottom = div()
            .pl(theme::DOT + theme::MARKER + theme::PAD * 2.)
            .overflow_hidden()
            .text_ellipsis()
            .whitespace_nowrap()
            .text_color(muted)
            .child(c.subject);
        Some(
            ListItem::new(("row", ix.row))
                .accessibility_label(label)
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

    /// A click, or Kit's Up and Down.
    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) {
        if let Some(ix) = ix {
            self.model.update(cx, |model, cx| {
                model.select(ix.row);
                cx.notify();
            });
        }
    }
}

pub struct MessageList {
    model: Entity<Model>,
    list: Entity<ListState<Rows>>,
}

impl MessageList {
    pub fn new(model: Entity<Model>, window: &mut Window, cx: &mut Context<Self>) -> MessageList {
        let rows = Rows {
            model: model.clone(),
            now: Local::now(),
        };
        let list = cx.new(|cx| ListState::new(rows, window, cx));
        let mut this = MessageList { model, list };
        this.change(window, cx, |_| {});
        this
    }

    pub fn focus(this: &Entity<MessageList>, window: &mut Window, cx: &mut App) {
        let list = this.read(cx).list.clone();
        list.update(cx, |list, cx| list.focus(window, cx));
    }

    /// Runs `f` on the model, then puts the Kit list's selection on the model's cursor and scrolls to it.
    fn change(&mut self, window: &mut Window, cx: &mut Context<Self>, f: impl FnOnce(&mut Model)) {
        let cursor = self.model.update(cx, |model, cx| {
            f(model);
            cx.notify();
            IndexPath::new(model.cursor)
        });
        self.list.update(cx, |list, cx| {
            list.set_selected_index(Some(cursor), window, cx);
            list.scroll_to_item(cursor, ScrollStrategy::Nearest, window, cx);
            cx.notify();
        });
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

    /// The help table is drawn here, so these two also redraw this view.
    fn toggle_help(&mut self, _: &ToggleHelp, window: &mut Window, cx: &mut Context<Self>) {
        self.change(window, cx, |m| m.help = !m.help);
        cx.notify();
    }

    fn escape(&mut self, _: &Escape, window: &mut Window, cx: &mut Context<Self>) {
        self.change(window, cx, Model::escape);
        cx.notify();
    }

    /// The keys that run `action` while the list has focus, from the keymap.
    fn keys(&self, action: &dyn gpui::Action, window: &Window, cx: &App) -> String {
        let focus = self.list.read(cx).focus_handle(cx);
        let keys: Vec<String> = window
            .bindings_for_action_in(action, &focus)
            .iter()
            .map(|binding| {
                let strokes: Vec<String> =
                    binding.keystrokes().iter().map(|k| k.unparse()).collect();
                strokes.join(" ")
            })
            .collect();
        keys.join(", ")
    }

    /// The help table, each action's keys read from the keymap.
    fn help(&self, window: &Window, cx: &App) -> impl IntoElement {
        let rows = HELP.iter().map(|(what, action)| {
            let keys = self.keys(*action, window, cx);
            div()
                .flex()
                .gap(theme::INSET)
                .child(div().w_32().font_family(theme::MONO_FONT).child(keys))
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
        let help = self.model.read(cx).help;
        div()
            .key_context(CONTEXT)
            .on_action(cx.listener(Self::next))
            .on_action(cx.listener(Self::prev))
            .on_action(cx.listener(Self::expand))
            .on_action(cx.listener(Self::collapse))
            .on_action(cx.listener(Self::toggle_mark))
            .on_action(cx.listener(Self::archive))
            .on_action(cx.listener(Self::toggle_help))
            .on_action(cx.listener(Self::escape))
            .relative()
            .size_full()
            .bg(theme::background())
            .font_family(theme::TEXT_FONT)
            .child(List::new(&self.list))
            .when(help, |d| d.child(self.help(window, cx)))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use gpui::{TestAppContext, VisualTestContext};
    use postvak::sync::Command;

    use crate::{Mail, test_model};

    use super::*;

    #[gpui::test]
    fn keys_move_the_cursor_open_a_thread_and_archive_it(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let (_dir, model, commands) = test_model(false);
        let (mail, cx) = cx.add_window_view(|window, cx| Mail::new(model, window, cx));
        let model = mail.read_with(cx, |mail, _| mail.model.clone());
        let state = |cx: &mut VisualTestContext| {
            model.read_with(cx, |m, _| {
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
        let sent: Vec<_> = commands.try_iter().collect();
        let [(_, Command::Apply { uids, .. })] = sent.as_slice() else {
            panic!("expected one Apply, got {sent:?}");
        };
        assert_eq!(uids.as_slice(), &[93, 94]);
        assert_eq!(state(cx).1, 4);
        cx.simulate_keystrokes("k down down");
        assert_eq!(state(cx).0, 2, "Up and Down are the Kit list's own keys");
        let help = |cx: &mut VisualTestContext| model.read_with(cx, |m, _| m.help);
        cx.simulate_keystrokes("?");
        assert!(help(cx), "? opens the key table");
        let list = mail.read_with(cx, |mail, _| mail.list.clone());
        let keys = cx.update(|window, cx| {
            let list = list.read(cx);
            [list.keys(&Next, window, cx), list.keys(&Escape, window, cx)]
        });
        assert_eq!(
            keys,
            ["j", "escape"],
            "the table reads its keys from the keymap"
        );
        cx.simulate_keystrokes("x escape");
        assert!(!help(cx));
        assert_eq!(
            model.read_with(cx, |m, _| m.marked.len()),
            1,
            "Esc closed the help first"
        );
        cx.simulate_keystrokes("escape");
        assert_eq!(
            model.read_with(cx, |m, _| (m.marked.len(), m.cursor)),
            (0, 2),
            "the cursor stays"
        );
    }
}
