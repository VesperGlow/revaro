//! Thin, read-only helpers over the `markup5ever_rcdom` tree.
//!
//! Both the chapter sanitizer and the navigation-document parser walk an HTML5
//! parse tree. Keeping the traversal helpers in one place means the two walks
//! agree on what "the element's name" and "the first matching attribute" mean —
//! in particular that attributes are matched by *local* name, which is how Go's
//! HTML parser normalised `xlink:href` to `href`.

use html5ever::tendril::TendrilSink;
use html5ever::{ParseOpts, parse_document};
use markup5ever_rcdom::{Handle, NodeData, RcDom};

/// Parse HTML bytes into a DOM, or `None` when the parser fails.
pub(crate) fn parse_html(source: &[u8]) -> Option<RcDom> {
    let mut reader = source;
    parse_document(RcDom::default(), ParseOpts::default())
        .from_utf8()
        .read_from(&mut reader)
        .ok()
}

/// Element children of `node`, cloned so the `RefCell` borrow is released
/// before the caller recurses.
pub(crate) fn children(node: &Handle) -> Vec<Handle> {
    node.children.borrow().clone()
}

/// The element's local name, or `None` for non-element nodes.
pub(crate) fn tag_name(node: &Handle) -> Option<String> {
    match &node.data {
        NodeData::Element { name, .. } => Some(name.local.to_string()),
        _ => None,
    }
}

/// First attribute whose local name is `key`.
pub(crate) fn attr(node: &Handle, key: &str) -> Option<String> {
    let NodeData::Element { attrs, .. } = &node.data else {
        return None;
    };
    for attribute in attrs.borrow().iter() {
        if &*attribute.name.local == key {
            return Some(attribute.value.to_string());
        }
    }
    None
}

/// Concatenated text of `node` and its descendants.
pub(crate) fn text_content(node: &Handle) -> String {
    let mut out = String::new();
    collect_text(node, &mut out);
    out
}

/// Recursively append text nodes to `out`.
fn collect_text(node: &Handle, out: &mut String) {
    if let NodeData::Text { contents } = &node.data {
        out.push_str(&contents.borrow());
    }
    for child in children(node) {
        collect_text(&child, out);
    }
}

/// DFS for the first element with local name `tag`.
pub(crate) fn find_element(node: &Handle, tag: &str) -> Option<Handle> {
    if let NodeData::Element { name, .. } = &node.data
        && &*name.local == tag
    {
        return Some(node.clone());
    }
    for child in children(node) {
        if let Some(found) = find_element(&child, tag) {
            return Some(found);
        }
    }
    None
}

/// Collect descendant elements (and `node` itself) with the given local name.
pub(crate) fn collect_elements(node: &Handle, tag: &str, out: &mut Vec<Handle>) {
    if let Some(local) = tag_name(node)
        && local == tag
    {
        out.push(node.clone());
    }
    for child in children(node) {
        collect_elements(&child, tag, out);
    }
}
