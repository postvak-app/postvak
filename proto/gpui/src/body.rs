//! The body pane: header, then the text or the HTML page. The HTML is laid out and painted by postvak's own render
//! thread (Blitz), in strips of premultiplied RGBA that become GPUI images; the protocol is `App::html_width`,
//! `html_visible` and `receive_html` without egui.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use gpui::{
    Context, Entity, ImageSource, IntoElement, ParentElement, Render, RenderImage, ScrollHandle,
    Styled, Subscription, Task, Window, div, img, prelude::*, px,
};
use postvak::gui::html::{self, Load, Pixels, Renderer, Reply, Request, STRIP};
use postvak::gui::list::{RowKey, display_name};
use postvak::message::{HtmlBody, clean};
use postvak::store::Message;

use crate::list::{MessageList, Rows};
use crate::theme;

/// `app.rs`'s `READ_DELAY`: an opened message becomes read after its text was on screen this long.
pub const READ_DELAY: Duration = Duration::from_secs(1);

/// The message on screen.
pub struct Shown {
    pub account: usize,
    pub key: RowKey,
    pub message: Option<Message>,
    pub text: Option<String>,
    pub html: Option<HtmlBody>,
    pub armed: bool,
    shown_at: Instant,
    pub read_sent: bool,
    pub view: Option<HtmlView>,
    /// Set when the HTML could not be laid out; the text shows instead.
    pub note: Option<&'static str>,
}

/// `HtmlState` without egui: what is laid out, at which width and scale, and the strips painted so far.
pub struct HtmlView {
    pub generation: u64,
    sent: (f32, f32),
    wanted: Option<((f32, f32), Instant)>,
    /// Width and height in points, once laid out.
    pub page: Option<(f32, f32)>,
    pub strips: BTreeMap<u32, Strip>,
    requested: BTreeSet<u32>,
}

pub struct Strip {
    pub image: Arc<RenderImage>,
    pub width: u32,
    pub height: u32,
}

pub struct BodyPane {
    rows: Entity<gpui_kit::component::list::ListState<Rows>>,
    pub shown: Option<Shown>,
    renderer: Renderer,
    generation: u64,
    scroll: ScrollHandle,
    _wake: Task<()>,
    _list: Subscription,
}

impl BodyPane {
    pub fn new(list: &Entity<MessageList>, cx: &mut Context<Self>) -> BodyPane {
        let (wake, mut woken) = futures::channel::mpsc::unbounded::<()>();
        let renderer = Renderer::start(Arc::new(move || {
            let _ = wake.unbounded_send(());
        }));
        let _wake = cx.spawn(async move |this, cx| {
            while woken.next().await.is_some() {
                if this.update(cx, |body, cx| body.receive(cx)).is_err() {
                    break;
                }
            }
        });
        let rows = list.read(cx).list.clone();
        let _list = cx.observe(&rows, |body, _, cx| body.sync(cx));
        let mut body = BodyPane {
            rows,
            shown: None,
            renderer,
            generation: 0,
            scroll: ScrollHandle::new(),
            _wake,
            _list,
        };
        body.sync(cx);
        body
    }

    /// `sync_body`: loads the cursor's message when the cursor moved to another one.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let (current, armed) = self.rows.update(cx, |rows, _| {
            let model = &mut rows.delegate_mut().model;
            (model.shown(), std::mem::take(&mut model.armed))
        });
        let same = current.as_ref()
            == self
                .shown
                .as_ref()
                .map(|s| (s.account, s.key.clone()))
                .as_ref();
        if same {
            if armed && let Some(shown) = &mut self.shown {
                shown.armed = true;
                self.start_read_timer(cx);
            }
            return;
        }
        self.shown = current.map(|(account, key)| self.load(account, key, armed, cx));
        self.scroll.set_offset(gpui::point(px(0.), px(0.)));
        if armed {
            self.start_read_timer(cx);
        }
        cx.notify();
    }

    fn load(&self, account: usize, key: RowKey, armed: bool, cx: &Context<Self>) -> Shown {
        let store = &self.rows.read(cx).delegate().model.accounts[account].store;
        let message = store.message(&key.0, key.1).ok().flatten();
        let raw = store.raw(&key.0, key.1).ok().flatten();
        let html = raw.as_deref().and_then(html::html_of);
        let note = html
            .as_ref()
            .filter(|h| h.html.len() > html::MAX_HTML)
            .map(|_| html::TOO_LARGE);
        Shown {
            account,
            text: message
                .as_ref()
                .and_then(|m| m.body_text.as_deref())
                .map(|t| clean(t, true)),
            message,
            html,
            armed,
            shown_at: cx.background_executor().now(),
            read_sent: false,
            view: None,
            note,
            key,
        }
    }

    fn start_read_timer(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(READ_DELAY).await;
            this.update(cx, |body, cx| body.mark_read_if_due(cx)).ok();
        })
        .detach();
    }

    /// `mark_read_after_delay`, run by the timer instead of every frame.
    fn mark_read_if_due(&mut self, cx: &mut Context<Self>) {
        let now = cx.background_executor().now();
        let Some(shown) = &mut self.shown else { return };
        let downloaded = shown
            .message
            .as_ref()
            .is_some_and(|m| m.body_text.is_some());
        if !shown.armed || shown.read_sent || !downloaded || now - shown.shown_at < READ_DELAY {
            return;
        }
        let (account, key) = (shown.account, shown.key.clone());
        let seen = self.rows.read(cx).delegate().model.is_seen(account, &key);
        if seen {
            return;
        }
        shown.read_sent = true;
        self.rows.update(cx, |rows, cx| {
            rows.delegate_mut().model.mark_read(account, key);
            cx.notify();
        });
    }

    /// `receive_html`: the render thread's replies for the shown layout; older ones are dropped.
    fn receive(&mut self, cx: &mut Context<Self>) {
        for reply in self.renderer.replies() {
            let Some(shown) = self.shown.as_mut() else {
                continue;
            };
            let Some(view) = shown
                .view
                .as_mut()
                .filter(|v| v.generation == reply.generation())
            else {
                continue;
            };
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
                    if let Some(painted) = strip_image(&image) {
                        view.strips.insert(strip, painted);
                    }
                }
                Reply::Link { .. } | Reply::Selected { .. } => {}
            }
        }
        cx.notify();
    }

    /// `html_width`: lays the page out at `width`, or again once a new width or scale held still for `SETTLE`.
    fn layout(&mut self, scale: f32, width: f32, cx: &mut Context<Self>) {
        // The executor's clock, which tests advance by hand.
        let now = cx.background_executor().now();
        let Some(shown) = self
            .shown
            .as_mut()
            .filter(|s| s.html.is_some() && s.note.is_none())
        else {
            return;
        };
        let fresh = match &mut shown.view {
            None => true,
            Some(view) if view.sent == (scale, width) => false,
            Some(view) => match view.wanted {
                Some((wanted, since)) if wanted == (scale, width) => {
                    (now - since).as_secs_f64() >= html::SETTLE
                }
                _ => {
                    view.wanted = Some(((scale, width), now));
                    cx.spawn(async move |this, cx| {
                        cx.background_executor()
                            .timer(Duration::from_secs_f64(html::SETTLE))
                            .await;
                        this.update(cx, |_, cx| cx.notify()).ok();
                    })
                    .detach();
                    false
                }
            },
        };
        let Some(source) = shown.html.as_ref().filter(|_| fresh) else {
            return;
        };
        self.generation += 1;
        let load = Load {
            generation: self.generation,
            html: source.html.clone(),
            inline: source.inline.clone(),
            scale,
            width,
        };
        shown.view = Some(HtmlView {
            generation: self.generation,
            sent: (scale, width),
            wanted: None,
            page: None,
            strips: BTreeMap::new(),
            requested: BTreeSet::new(),
        });
        self.renderer.send(Request::Load(load));
    }

    /// `html_visible`: asks for the strips around what is on screen and lets go of those far from it.
    fn visible(&mut self, window: &mut Window) {
        let Some(view) = self.shown.as_mut().and_then(|s| s.view.as_mut()) else {
            return;
        };
        let Some((_, height)) = view.page else { return };
        let top = f32::from(-self.scroll.offset().y);
        let bottom = top + f32::from(self.scroll.bounds().size.height).max(1.0);
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
        for strip in far {
            if let Some(gone) = view.strips.remove(&strip) {
                window.drop_image(gone.image).ok();
            }
            view.requested.remove(&strip);
        }
    }
}

/// A painted strip as a GPUI image: GPUI's `RenderImage` is BGRA, Blitz paints RGBA.
fn strip_image(pixels: &Pixels) -> Option<Strip> {
    let mut bgra = pixels.rgba().to_vec();
    for pixel in bgra.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    let buffer = image::RgbaImage::from_raw(pixels.width(), pixels.height(), bgra)?;
    let frame = image::Frame::new(buffer);
    Some(Strip {
        image: Arc::new(RenderImage::new(smallvec::smallvec![frame])),
        width: pixels.width(),
        height: pixels.height(),
    })
}

impl Render for BodyPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let scale = window.scale_factor();
        let width = f32::from(window.viewport_size().width - theme::LIST_WIDTH)
            - 2.0 * f32::from(theme::INSET);
        self.layout(scale, width.max(html::MIN_WIDTH), cx);
        self.visible(window);
        let pane = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::background())
            .text_color(theme::text())
            .font_family(theme::TEXT_FONT);
        let Some(shown) = &self.shown else {
            return pane;
        };
        let m = shown.message.as_ref();
        let subject = m.and_then(|m| m.subject.clone()).unwrap_or_default();
        let from = m
            .and_then(|m| m.from_addr.as_deref())
            .map(display_name)
            .unwrap_or_default();
        let header = div()
            .p(theme::INSET)
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .font_family(theme::SERIF_FONT)
                    .text_xl()
                    .text_color(theme::heading())
                    .child(clean(&subject, false)),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme::muted())
                    .child(clean(&from, false)),
            );
        let content = match shown
            .view
            .as_ref()
            .and_then(|v| v.page.map(|page| (v, page)))
        {
            Some((view, (page_w, page_h))) => {
                let scale = view.sent.0;
                div()
                    .relative()
                    .w(px(page_w))
                    .h(px(page_h))
                    .children(view.strips.iter().map(|(n, strip)| {
                        img(ImageSource::Render(strip.image.clone()))
                            .absolute()
                            .left_0()
                            .top(px((n * STRIP) as f32 / scale))
                            .w(px(strip.width as f32 / scale))
                            .h(px(strip.height as f32 / scale))
                    }))
                    .into_any_element()
            }
            None if shown.view.is_some() => {
                div().p(theme::INSET).child("Rendering…").into_any_element()
            }
            None => div()
                .p(theme::INSET)
                .whitespace_normal()
                .when_some(shown.note, |d, note| {
                    d.child(div().text_color(theme::muted()).child(note))
                })
                .child(shown.text.clone().unwrap_or_else(|| "Loading…".into()))
                .into_any_element(),
        };
        pane.child(header).child(
            div()
                .id("body")
                .flex_1()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .child(content),
        )
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use gpui::{TestAppContext, VisualTestContext};
    use postvak::rules::Action;
    use postvak::sync::Command;

    use crate::Mail;
    use crate::model::Model;

    use super::*;

    fn painted(mail: &Entity<Mail>, cx: &mut VisualTestContext) -> Option<(usize, u32, u32)> {
        mail.read_with(cx, |mail, cx| {
            let view = mail.body.read(cx).shown.as_ref()?.view.as_ref()?;
            let strip = view.strips.get(&0)?;
            Some((view.strips.len(), strip.width, strip.height))
        })
    }

    fn sent(mail: &Entity<Mail>, cx: &mut VisualTestContext) -> Vec<(String, Command)> {
        mail.read_with(cx, |mail, cx| {
            let rows = mail.list.read(cx).list.read(cx);
            rows.delegate().model.commands.try_iter().collect()
        })
    }

    #[gpui::test]
    fn the_newsletter_paints_and_becomes_read_a_second_after_it_is_opened(cx: &mut TestAppContext) {
        // The render thread is a real thread; its replies arrive while the test waits.
        cx.executor().allow_parking();
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.bind_keys(crate::list::bindings());
        });
        let dir = tempfile::tempdir().unwrap();
        let home = postvak::gui::fixture::mailbox(dir.path(), "light").unwrap();
        crate::add_newsletter(&home).unwrap();
        let model = Model::open(&home, vec![1]).unwrap();
        let (mail, cx) = cx.add_window_view(|window, cx| Mail::new(model, window, cx));
        let mut strips = None;
        for _ in 0..2000 {
            cx.run_until_parked();
            strips = painted(&mail, cx);
            if strips.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let (count, width, height) = strips.expect("no strip within 10 s");
        assert!(
            count >= 1 && width > 300 && height <= STRIP,
            "{count} strips, {width}×{height}"
        );
        // Shown at start, not chosen: it stays unread however long it is on screen.
        cx.executor().advance_clock(READ_DELAY * 2);
        cx.run_until_parked();
        assert!(sent(&mail, cx).is_empty());
        // Away and back: chosen now, read a second later and not before.
        cx.simulate_keystrokes("j k");
        cx.executor().advance_clock(READ_DELAY / 2);
        cx.run_until_parked();
        assert!(sent(&mail, cx).is_empty());
        cx.executor().advance_clock(READ_DELAY);
        cx.run_until_parked();
        let sent = sent(&mail, cx);
        let [(account, Command::Apply { uids, action, .. })] = sent.as_slice() else {
            panic!("expected one Apply, got {sent:?}");
        };
        assert_eq!(
            (account.as_str(), uids.as_slice(), action),
            ("work", &[97][..], &Action::MarkRead)
        );
    }
}
