//! The body pane: header, then the text or the HTML page. The HTML is laid out and painted by postvak's own render
//! thread (Blitz), in strips of premultiplied RGBA that become GPUI images; the protocol is `App::html_width`,
//! `html_visible` and `receive_html` without egui.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use gpui::{
    Context, Entity, ImageSource, IntoElement, ParentElement, Render, RenderImage, ScrollHandle,
    SharedString, Styled, Subscription, Task, Window, div, img, prelude::*, px,
    swap_rgba_pa_to_bgra,
};
use postvak::gui::html::{self, Load, Pixels, Renderer, Reply, Request, STRIP};
use postvak::gui::list::{RowKey, display_name};
use postvak::message::{HtmlBody, clean};

use crate::model::Model;
use crate::theme;

/// `app.rs`'s `READ_DELAY`: an opened message becomes read after its text was on screen this long.
const READ_DELAY: Duration = Duration::from_secs(1);

/// The message on screen, with what the header and text show computed once.
struct Shown {
    account: usize,
    key: RowKey,
    subject: SharedString,
    from: SharedString,
    text: Option<SharedString>,
    html: Option<HtmlBody>,
    view: Option<HtmlView>,
    /// Set when the HTML could not be laid out; the text shows instead.
    note: Option<&'static str>,
    /// Marks the message read after `READ_DELAY`; dropped, and so cancelled, with the message.
    read_timer: Option<Task<()>>,
}

/// `HtmlState` without egui: what is laid out, at which width and scale, and the strips painted so far.
struct HtmlView {
    generation: u64,
    sent: (f32, f32),
    wanted: Option<((f32, f32), Instant)>,
    /// Width and height in points, once laid out.
    page: Option<(f32, f32)>,
    strips: BTreeMap<u32, Arc<RenderImage>>,
    requested: BTreeSet<u32>,
}

pub struct BodyPane {
    model: Entity<Model>,
    shown: Option<Shown>,
    renderer: Renderer,
    generation: u64,
    scroll: ScrollHandle,
    /// Redraws once a new width held still for `SETTLE`; replaced while the width keeps changing.
    settle: Option<Task<()>>,
    _wake: Task<()>,
    _model: Subscription,
}

impl BodyPane {
    pub fn new(model: Entity<Model>, cx: &mut Context<Self>) -> BodyPane {
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
        let _model = cx.observe(&model, |body, _, cx| body.sync(cx));
        let mut body = BodyPane {
            model,
            shown: None,
            renderer,
            generation: 0,
            scroll: ScrollHandle::new(),
            settle: None,
            _wake,
            _model,
        };
        body.sync(cx);
        body
    }

    /// `sync_body`: loads the cursor's message when the cursor moved to another one, and starts the read timer once
    /// the user chose it.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let model = self.model.read(cx);
        let current = model.cursor_row();
        let armed = current.is_some() && model.armed_for == current;
        let shown = self.shown.as_ref().map(|s| (s.account, s.key.clone()));
        if current != shown {
            self.shown = current.map(|(account, key)| self.load(account, key, cx));
            self.scroll.set_offset(gpui::point(px(0.), px(0.)));
            cx.notify();
        }
        if armed && let Some(shown) = self.shown.as_mut().filter(|s| s.read_timer.is_none()) {
            shown.read_timer = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(READ_DELAY).await;
                this.update(cx, |body, cx| body.mark_read(cx)).ok();
            }));
        }
    }

    fn load(&self, account: usize, key: RowKey, cx: &Context<Self>) -> Shown {
        let store = &self.model.read(cx).accounts[account].store;
        let message = store.message(&key.0, key.1).ok().flatten();
        let raw = store.raw(&key.0, key.1).ok().flatten();
        let html = raw.as_deref().and_then(html::html_of);
        let note = html
            .as_ref()
            .filter(|h| h.html.len() > html::MAX_HTML)
            .map(|_| html::TOO_LARGE);
        let m = message.as_ref();
        let subject = m.and_then(|m| m.subject.as_deref()).unwrap_or_default();
        let from = m
            .and_then(|m| m.from_addr.as_deref())
            .map(display_name)
            .unwrap_or_default();
        Shown {
            account,
            key,
            subject: clean(subject, false).into(),
            from: clean(&from, false).into(),
            text: m
                .and_then(|m| m.body_text.as_deref())
                .map(|t| clean(t, true).into()),
            html,
            view: None,
            note,
            read_timer: None,
        }
    }

    /// `mark_read_after_delay`'s send, run by the read timer once the text has been on screen.
    /// How many strips of the shown page are painted, for tests that wait for them.
    #[cfg(test)]
    pub fn painted_strips(&self) -> usize {
        let view = self.shown.as_ref().and_then(|s| s.view.as_ref());
        view.map_or(0, |v| v.strips.len())
    }

    fn mark_read(&mut self, cx: &mut Context<Self>) {
        let Some(shown) = self.shown.as_ref().filter(|s| s.text.is_some()) else {
            return;
        };
        let (account, key) = (shown.account, shown.key.clone());
        self.model.update(cx, |model, cx| {
            if !model.is_seen(account, &key) {
                model.mark_read(account, key);
                cx.notify();
            }
        });
    }

    /// `receive_html`: the render thread's replies for the shown layout; older ones are dropped.
    fn receive(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
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
                    if let Some(image) = strip_image(image) {
                        view.strips.insert(strip, image);
                    }
                }
                Reply::Link { .. } | Reply::Selected { .. } => {}
            }
        }
        if changed {
            cx.notify();
        }
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
                    self.settle = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor()
                            .timer(Duration::from_secs_f64(html::SETTLE))
                            .await;
                        this.update(cx, |_, cx| cx.notify()).ok();
                    }));
                    false
                }
            },
        };
        if !fresh {
            return;
        }
        let Some(source) = &shown.html else { return };
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
        for strip in html::far_strips(view.strips.keys().copied(), top, bottom, scale) {
            if let Some(gone) = view.strips.remove(&strip) {
                window.drop_image(gone).ok();
            }
            view.requested.remove(&strip);
        }
    }
}

/// A painted strip as a GPUI image: Blitz paints premultiplied RGBA, GPUI's `RenderImage` is straight BGRA.
fn strip_image(pixels: Pixels) -> Option<Arc<RenderImage>> {
    let (width, height) = (pixels.width(), pixels.height());
    let mut bgra = pixels.into_rgba();
    for pixel in bgra.as_chunks_mut::<4>().0 {
        swap_rgba_pa_to_bgra(pixel);
    }
    let buffer = image::RgbaImage::from_raw(width, height, bgra)?;
    let frame = image::Frame::new(buffer);
    Some(Arc::new(RenderImage::new(smallvec::smallvec![frame])))
}

impl Render for BodyPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The scroll area's width from the last frame; a resize settles before a new layout anyway.
        let width = f32::from(self.scroll.bounds().size.width) - 2.0 * f32::from(theme::INSET);
        if width > 0.0 {
            self.layout(window.scale_factor(), width.max(html::MIN_WIDTH), cx);
        } else if self.shown.as_ref().is_some_and(|s| s.html.is_some()) {
            // Not laid out yet: draw once more, after which the scroll area has its size.
            cx.defer_in(window, |_, _, cx| cx.notify());
        }
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
        let header = div()
            .p(theme::INSET)
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .font_family(theme::SERIF_FONT)
                    .text_xl()
                    .text_color(theme::heading())
                    .child(shown.subject.clone()),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme::muted())
                    .child(shown.from.clone()),
            );
        let page = shown.view.as_ref().and_then(|v| Some((v, v.page?)));
        let content = match page {
            Some((view, (page_w, page_h))) => {
                let scale = view.sent.0;
                div()
                    .relative()
                    .mx(theme::INSET)
                    .w(px(page_w))
                    .h(px(page_h))
                    .children(view.strips.iter().map(|(n, strip)| {
                        let size = strip.size(0);
                        img(ImageSource::Render(strip.clone()))
                            .absolute()
                            .left_0()
                            .top(px((n * STRIP) as f32 / scale))
                            .w(px(i32::from(size.width) as f32 / scale))
                            .h(px(i32::from(size.height) as f32 / scale))
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

    use crate::{Mail, test_model};

    use super::*;

    fn painted(mail: &Entity<Mail>, cx: &mut VisualTestContext) -> Option<(usize, i32, i32)> {
        mail.read_with(cx, |mail, cx| {
            let view = mail.body.read(cx).shown.as_ref()?.view.as_ref()?;
            let size = view.strips.get(&0)?.size(0);
            Some((view.strips.len(), size.width.into(), size.height.into()))
        })
    }

    #[gpui::test]
    fn the_newsletter_paints_and_becomes_read_a_second_after_it_is_opened(cx: &mut TestAppContext) {
        // The render thread is a real thread; its replies arrive while the test waits.
        cx.executor().allow_parking();
        cx.update(crate::init);
        let (_dir, model, commands) = test_model(true);
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
            count >= 1 && width > 300 && height <= STRIP as i32,
            "{count} strips, {width}×{height}"
        );
        // Shown at start, not chosen: it stays unread however long it is on screen.
        cx.executor().advance_clock(READ_DELAY * 2);
        cx.run_until_parked();
        assert!(commands.try_iter().next().is_none());
        // Away and back: chosen now, read a second later and not before.
        cx.simulate_keystrokes("j k");
        cx.executor().advance_clock(READ_DELAY / 2);
        cx.run_until_parked();
        assert!(commands.try_iter().next().is_none());
        cx.executor().advance_clock(READ_DELAY);
        cx.run_until_parked();
        let sent: Vec<_> = commands.try_iter().collect();
        let [(account, Command::Apply { uids, action, .. })] = sent.as_slice() else {
            panic!("expected one Apply, got {sent:?}");
        };
        assert_eq!(
            (account.as_str(), uids.as_slice(), action),
            ("work", &[97][..], &Action::MarkRead)
        );
    }
}
