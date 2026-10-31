//! The opened message's HTML view: its layout, styling, remote content, painting and pointer.
use super::*;

impl App {
    /// The shown message's sender domain (the registrable part), while the account's provider vouches for the sender.
    pub(crate) fn vouched_sender(&self) -> Option<String> {
        let body = self.body.as_ref()?;
        let message = body.message.as_ref()?;
        let from = message.from_addr.as_deref()?;
        let identity = self.accounts.get(body.account)?.identity.as_ref()?;
        if !identity.authenticated(&message.headers, from) {
            return None;
        }
        let [address] = message::bare_addresses(from).try_into().ok()?;
        Some(message::auth::org_domain(&message::auth::address_domain(
            &address,
        )?))
    }

    /// The shown message's sender domain, when its remote content loads without asking.
    pub(crate) fn remembered_sender(&self) -> Option<String> {
        self.vouched_sender()
            .filter(|domain| self.remote_content.contains(domain))
    }

    pub(super) fn save_remote_content(&mut self) {
        match config::save_remote_content(&self.paths.config_file(), &self.remote_content) {
            // Absorbs this write, so the poll does not take it for an outside edit.
            Ok(()) => {
                self.config_watch.changed();
            }
            Err(e) => self.note_error(None, format!("could not save remote content settings: {e}")),
        }
    }

    /// Takes the render thread's replies for the shown layout; replies for an older one are dropped.
    pub(super) fn receive_html(&mut self) {
        let remembered = self.remembered_sender().is_some();
        let Some(renderer) = &self.renderer else {
            return;
        };
        for reply in renderer.replies() {
            let Some(body) = self.body.as_mut() else {
                continue;
            };
            let Some(view) = body
                .html_view
                .as_mut()
                .filter(|v| v.generation == reply.generation())
            else {
                continue;
            };
            match reply {
                Reply::Failed { .. } => {
                    body.text_note = Some(html::FAILED);
                    body.html_view = None;
                }
                Reply::Laid {
                    height,
                    remote,
                    width,
                    ..
                } => {
                    if height > html::MAX_HEIGHT || width > html::MAX_WIDTH {
                        body.text_note = Some(html::TOO_LARGE);
                        body.html_view = None;
                        continue;
                    }
                    view.page = Some((width, height));
                    /* Once the user loaded remote content, what it asks for in turn (images in a stylesheet) follows; from a
                    remembered sender it loads unasked. */
                    let follow = (remembered || !view.fetched.is_empty())
                        && view.fetched.len() < html::MAX_REMOTE;
                    view.remote = remote;
                    if follow && !view.remote.is_empty() && view.fetching.is_none() {
                        self.load_remote();
                        continue;
                    }
                    view.strips.clear();
                    view.requested.clear();
                    view.hover = None;
                    view.hovered_at = None;
                    view.selected_at = None;
                    view.selection = None;
                }
                Reply::Link { href, .. } => view.hover = href,
                Reply::Selected { text, .. } => {
                    view.selection = text;
                    // Paint the strips again with the highlight; the old ones stay on screen until then.
                    view.requested.clear();
                }
                Reply::Strip { image, strip, .. } => {
                    view.strips.insert(strip, Arc::new(image));
                }
            }
        }
    }

    /// Starts the HTML layout of the shown message, or a new one once the panel width or scale held still for
    /// `html::SETTLE`; until then the old layout stays on screen.
    pub(super) fn html_width(&mut self, scale: f32, width: f32, now: f64) {
        let Some(body) = self.body.as_mut().filter(|b| b.shows_html()) else {
            return;
        };
        let mut settle_at = None;
        let fresh = match &mut body.html_view {
            None => true,
            Some(view) if view.sent == (scale, width) => {
                view.wanted = None;
                false
            }
            Some(view) => match view.wanted {
                Some((wanted, since)) if wanted == (scale, width) => {
                    if now - since < html::SETTLE {
                        settle_at = Some(since + html::SETTLE);
                    }
                    now - since >= html::SETTLE
                }
                _ => {
                    view.wanted = Some(((scale, width), now));
                    settle_at = Some(now + html::SETTLE);
                    false
                }
            },
        };
        if fresh {
            self.lay_out(scale, width);
        } else if let Some(at) = settle_at {
            self.request_tick(at);
        }
    }

    /// Sends the shown message's HTML to the render thread, at `width` points and `scale`.
    pub(super) fn lay_out(&mut self, scale: f32, width: f32) {
        let dark = self.html_dark_page();
        let Some(body) = self.body.as_mut() else {
            return;
        };
        let Some(source) = body.html.as_ref() else {
            return;
        };
        self.html_generation += 1;
        let generation = self.html_generation;
        let fetched = body
            .html_view
            .as_ref()
            .map(|view| view.fetched.clone())
            .unwrap_or_default();
        let load = html::Load {
            dark,
            fetched,
            generation,
            html: source.html.clone(),
            inline: source.inline.clone(),
            scale,
            width,
        };
        match &mut body.html_view {
            Some(view) => {
                view.generation = generation;
                view.sent = (scale, width);
                view.wanted = None;
                view.dark = dark;
            }
            None => {
                let mut view = HtmlState::new(generation, scale, width);
                view.dark = dark;
                body.html_view = Some(view);
            }
        }
        self.renderer
            .get_or_insert_with(|| Renderer::start(self.wake.clone()))
            .send(html::Request::Load(load));
    }

    /// Whether HTML mail may be adapted to the dark theme: the window is dark and `[ui] html_colours` allows it.
    pub(crate) fn adapts_html(&self) -> bool {
        self.html_dark && self.html_colours == HtmlColours::Adapt
    }

    /// Whether "Show original colours" applies: the shown message's HTML is on screen and would be adapted.
    pub(crate) fn offers_original_colours(&self) -> bool {
        self.adapts_html() && self.body.as_ref().is_some_and(BodyState::shows_html)
    }

    /// Whether the shown message's HTML is styled dark: adapted, unless the user asked for its original colours.
    pub(crate) fn html_dark_page(&self) -> bool {
        self.adapts_html() && !self.body.as_ref().is_some_and(|b| b.original_colours)
    }

    /// Styles the laid-out page again when the theme or the colours choice changed; the render thread keeps the
    /// parsed document, and answers with a layout under a new generation.
    pub(super) fn restyle_html(&mut self) {
        let dark = self.html_dark_page();
        let Some(view) = self.body.as_mut().and_then(|b| b.html_view.as_mut()) else {
            return;
        };
        if view.dark == dark {
            return;
        }
        self.html_generation += 1;
        view.generation = self.html_generation;
        view.dark = dark;
        self.renderer
            .get_or_insert_with(|| Renderer::start(self.wake.clone()))
            .send(html::Request::Restyle {
                generation: view.generation,
                dark,
            });
    }

    /// Starts downloading what the shown page asked for; the tag is a fresh layout number, unique to this request.
    pub(super) fn load_remote(&mut self) {
        let Some(view) = self.body.as_mut().and_then(|b| b.html_view.as_mut()) else {
            return;
        };
        if view.remote.is_empty() || view.fetching.is_some() {
            return;
        }
        self.html_generation += 1;
        let tag = self.html_generation;
        view.fetching = Some(tag);
        let room = html::MAX_REMOTE.saturating_sub(view.fetched.len());
        let urls = std::mem::take(&mut view.remote)
            .into_iter()
            .take(room)
            .collect();
        self.fetcher
            .get_or_insert_with(|| Fetcher::start(self.wake.clone()))
            .send(html::FetchJob { tag, urls });
    }

    /// Takes finished downloads for the shown message and lays it out again with them.
    pub(super) fn receive_remote(&mut self) {
        let Some(fetcher) = &self.fetcher else {
            return;
        };
        for done in fetcher.done() {
            let Some(view) = self.body.as_mut().and_then(|b| b.html_view.as_mut()) else {
                continue;
            };
            if view.fetching != Some(done.tag) {
                continue;
            }
            view.fetching = None;
            std::sync::Arc::make_mut(&mut view.fetched).extend(done.fetched);
            let (scale, width) = view.sent;
            self.lay_out(scale, width);
        }
    }

    /// Records the part of the page on screen, and asks for its strips.
    pub(super) fn html_visible(&mut self, top: f32, bottom: f32) {
        if let Some(view) = self.body.as_mut().and_then(|b| b.html_view.as_mut()) {
            view.visible = (top, bottom);
        }
        self.paint_visible();
    }

    /// Asks for the strips around what is on screen that are neither painted nor asked for, and lets go of those far
    /// from it. A new layout or selection empties `requested`, so they are asked for again.
    pub(super) fn paint_visible(&mut self) {
        let (Some(view), Some(renderer)) = (
            self.body.as_mut().and_then(|b| b.html_view.as_mut()),
            self.renderer.as_mut(),
        ) else {
            return;
        };
        let (top, bottom) = view.visible;
        let Some(page) = view.page else { return };
        let scale = view.sent.0;
        for strip in html::wanted_strips(top, bottom, page.1, scale) {
            if view.requested.insert(strip) {
                renderer.send(html::Request::Paint {
                    generation: view.generation,
                    strip,
                });
            }
        }
        for strip in html::far_strips(view.strips.keys().copied(), top, bottom, scale) {
            view.strips.remove(&strip);
            view.requested.remove(&strip);
        }
    }

    /// Asks which link is under the pointer; the answer arrives as `Reply::Link`.
    pub(super) fn html_hover(&mut self, at: Option<[f32; 2]>) {
        let (Some(view), Some(renderer)) = (
            self.body.as_mut().and_then(|b| b.html_view.as_mut()),
            self.renderer.as_mut(),
        ) else {
            return;
        };
        view.hovered_at = at;
        match at {
            Some(at) => renderer.send(html::Request::Hit {
                generation: view.generation,
                x: at[0],
                y: at[1],
            }),
            None => view.hover = None,
        }
    }

    /// Selects the page's text between two points; the answer arrives as `Reply::Selected`.
    pub(super) fn html_select(&mut self, from: [f32; 2], to: [f32; 2]) {
        let (Some(view), Some(renderer)) = (
            self.body.as_mut().and_then(|b| b.html_view.as_mut()),
            self.renderer.as_mut(),
        ) else {
            return;
        };
        view.selected_at = Some((from, to));
        renderer.send(html::Request::Select {
            generation: view.generation,
            from,
            to,
        });
    }
}
