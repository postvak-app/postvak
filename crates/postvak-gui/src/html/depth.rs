//! Parsing that gives up on markup nested too deep. html5ever scans its stack of open elements for most tags, so a page
//! nested n levels deep takes time in n²: 100,000 nested `<div>`s, about 1 MB, keep the render thread busy for minutes.
use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use blitz_dom::{BaseDocument, NodeId};
use blitz_html::DocumentHtmlParser;
use html5ever::tendril::{StrTendril, TendrilSink};
use html5ever::tree_builder::{ElementFlags, NodeOrText, QuirksMode, TreeBuilderOpts, TreeSink};
use html5ever::{Attribute, QualName};

use super::legacy;

/// How much markup the parser takes between checks: past the limit, one chunk adds at most a few thousand levels.
const CHUNK: usize = 4096;

/// Parses `html` into `doc`; false, leaving `doc` half built, once an element lands more than `limit` levels deep.
/// Mail is HTML whatever it declares: Blitz's own entry point sniffs `xmlns="…xhtml"` and parses as XML, where a void
/// `<img>` swallows the rest of the page.
pub fn parse(doc: &mut BaseDocument, html: &str, limit: usize) -> bool {
    let too_deep = Cell::new(false);
    let mut mutator = doc.mutate();
    let sink = Bounded {
        inner: DocumentHtmlParser::new(&mut mutator),
        depths: RefCell::new(HashMap::new()),
        limit,
        too_deep: &too_deep,
    };
    let opts = html5ever::ParseOpts {
        // No script runs, so `<noscript>` content shows, as Blitz's own HTML path does.
        tree_builder: TreeBuilderOpts {
            scripting_enabled: false,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut parser = html5ever::parse_document(sink, opts);
    for chunk in chunks(html) {
        parser.process(StrTendril::from_slice(chunk));
        if too_deep.get() {
            return false;
        }
    }
    parser.finish();
    !too_deep.get()
}

/// `html` in pieces of about `CHUNK` bytes, split between characters.
fn chunks(mut html: &str) -> impl Iterator<Item = &str> {
    std::iter::from_fn(move || {
        if html.is_empty() {
            return None;
        }
        let mut end = CHUNK.min(html.len());
        while !html.is_char_boundary(end) {
            end += 1;
        }
        let (chunk, rest) = html.split_at(end);
        html = rest;
        Some(chunk)
    })
}

/// Blitz's sink, noting how deep each node lands. A node moved later keeps the depth it landed at; the depth check after
/// the parse has the last word.
struct Bounded<'p, 'm, 'doc> {
    inner: DocumentHtmlParser<'m, 'doc>,
    /// The document and nodes not yet placed are at 0.
    depths: RefCell<HashMap<NodeId, usize>>,
    limit: usize,
    too_deep: &'p Cell<bool>,
}

impl Bounded<'_, '_, '_> {
    fn depth(&self, id: NodeId) -> usize {
        self.depths.borrow().get(&id).copied().unwrap_or(0)
    }

    fn place(&self, child: &NodeOrText<NodeId>, depth: usize) {
        if let NodeOrText::AppendNode(id) = *child {
            self.depths.borrow_mut().insert(id, depth);
            if depth > self.limit {
                self.too_deep.set(true);
            }
        }
    }
}

impl<'m, 'doc> TreeSink for Bounded<'_, 'm, 'doc> {
    type Handle = NodeId;
    type Output = ();
    type ElemName<'a>
        = <DocumentHtmlParser<'m, 'doc> as TreeSink>::ElemName<'a>
    where
        Self: 'a;

    fn finish(self) {
        self.inner.finish();
    }

    fn parse_error(&self, msg: Cow<'static, str>) {
        self.inner.parse_error(msg);
    }

    fn get_document(&self) -> NodeId {
        self.inner.get_document()
    }

    fn elem_name<'a>(&'a self, target: &'a NodeId) -> Self::ElemName<'a> {
        self.inner.elem_name(target)
    }

    fn create_element(
        &self,
        name: QualName,
        mut attrs: Vec<Attribute>,
        flags: ElementFlags,
    ) -> NodeId {
        // Legacy colours go to Blitz as `#rrggbb`; the stored markup is untouched.
        attrs.retain_mut(|attr| {
            if !legacy::is_colour_attribute(&name.local, &attr.name.local) {
                return true;
            }
            match legacy::parse(&attr.value) {
                Some(hex) => {
                    attr.value = StrTendril::from_slice(&hex);
                    true
                }
                None => false,
            }
        });
        self.inner.create_element(name, attrs, flags)
    }

    fn create_comment(&self, text: StrTendril) -> NodeId {
        self.inner.create_comment(text)
    }

    fn create_pi(&self, target: StrTendril, data: StrTendril) -> NodeId {
        self.inner.create_pi(target, data)
    }

    fn append(&self, parent: &NodeId, child: NodeOrText<NodeId>) {
        self.place(&child, self.depth(*parent) + 1);
        self.inner.append(parent, child);
    }

    fn append_based_on_parent_node(
        &self,
        element: &NodeId,
        prev_element: &NodeId,
        child: NodeOrText<NodeId>,
    ) {
        // Beside `element` when it has a parent, else in `prev_element`: the deeper of the two.
        let depth = self.depth(*element).max(self.depth(*prev_element) + 1);
        self.place(&child, depth);
        self.inner
            .append_based_on_parent_node(element, prev_element, child);
    }

    fn append_doctype_to_document(
        &self,
        name: StrTendril,
        public_id: StrTendril,
        system_id: StrTendril,
    ) {
        self.inner
            .append_doctype_to_document(name, public_id, system_id);
    }

    fn mark_script_already_started(&self, node: &NodeId) {
        self.inner.mark_script_already_started(node);
    }

    fn pop(&self, node: &NodeId) {
        self.inner.pop(node);
    }

    fn get_template_contents(&self, target: &NodeId) -> NodeId {
        let contents = self.inner.get_template_contents(target);
        // What goes into the template nests below it.
        self.place(&NodeOrText::AppendNode(contents), self.depth(*target));
        contents
    }

    fn same_node(&self, x: &NodeId, y: &NodeId) -> bool {
        self.inner.same_node(x, y)
    }

    fn set_quirks_mode(&self, mode: QuirksMode) {
        self.inner.set_quirks_mode(mode);
    }

    fn append_before_sibling(&self, sibling: &NodeId, new_node: NodeOrText<NodeId>) {
        self.place(&new_node, self.depth(*sibling));
        self.inner.append_before_sibling(sibling, new_node);
    }

    fn add_attrs_if_missing(&self, target: &NodeId, attrs: Vec<Attribute>) {
        self.inner.add_attrs_if_missing(target, attrs);
    }

    fn associate_with_form(
        &self,
        target: &NodeId,
        form: &NodeId,
        nodes: (&NodeId, Option<&NodeId>),
    ) {
        self.inner.associate_with_form(target, form, nodes);
    }

    fn remove_from_parent(&self, target: &NodeId) {
        self.inner.remove_from_parent(target);
    }

    fn reparent_children(&self, node: &NodeId, new_parent: &NodeId) {
        self.inner.reparent_children(node, new_parent);
    }

    fn is_mathml_annotation_xml_integration_point(&self, handle: &NodeId) -> bool {
        self.inner
            .is_mathml_annotation_xml_integration_point(handle)
    }

    fn set_current_line(&self, line_number: u64) {
        self.inner.set_current_line(line_number);
    }

    fn allow_declarative_shadow_roots(&self, intended_parent: &NodeId) -> bool {
        self.inner.allow_declarative_shadow_roots(intended_parent)
    }

    fn attach_declarative_shadow(
        &self,
        location: &NodeId,
        template: &NodeId,
        attrs: &[Attribute],
    ) -> bool {
        self.inner
            .attach_declarative_shadow(location, template, attrs)
    }

    fn maybe_clone_an_option_into_selectedcontent(&self, option: &NodeId) {
        self.inner
            .maybe_clone_an_option_into_selectedcontent(option);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_split_between_characters_and_keep_every_byte() {
        let html = "é".repeat(CHUNK);
        let pieces: Vec<_> = chunks(&html).collect();
        assert!(pieces.len() > 1);
        assert_eq!(pieces.concat(), html);
        assert!(chunks("").next().is_none());
    }

    #[test]
    fn markup_within_the_limit_parses_whole() {
        let mut doc = BaseDocument::new(Default::default());
        let html = format!("{}x{}", "<div>".repeat(100), "</div>".repeat(100));
        assert!(parse(&mut doc, &html.repeat(50), 128));
        assert_eq!(
            doc.query_selector_all("div").map(|d| d.len()).ok(),
            Some(5000)
        );
    }

    #[test]
    fn markup_past_the_limit_stops_within_a_chunk() {
        let mut doc = BaseDocument::new(Default::default());
        // Parsed whole, this takes minutes.
        assert!(!parse(&mut doc, &"<div>".repeat(100_000), 512));
        let parsed = doc.query_selector_all("div").map(|d| d.len()).unwrap_or(0);
        assert!(parsed < 512 + CHUNK, "{parsed} divs");
    }
}
