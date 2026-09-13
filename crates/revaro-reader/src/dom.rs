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

use crate::MAX_HTML_TREE_DEPTH;

/// Parse HTML bytes into a DOM, or `None` when the parser fails.
pub(crate) fn parse_html(source: &[u8]) -> Option<RcDom> {
    let mut reader = source;
    let dom = parse_document(RcDom::default(), ParseOpts::default())
        .from_utf8()
        .read_from(&mut reader)
        .ok()?;
    html_tree_within_depth(&dom.document).then_some(dom)
}

/// Check the parsed tree with an explicit stack before any compatibility walk.
///
/// The parser itself is iterative, but the sanitizer historically used a few
/// recursive helpers. Rejecting an over-deep tree here gives all callers the
/// same bound, including navigation and flow parsing, and makes that bound
/// independent of which helper happens to visit the tree first.
fn html_tree_within_depth(root: &Handle) -> bool {
    let mut pending = vec![(root.clone(), 0usize)];
    while let Some((node, depth)) = pending.pop() {
        let descendants = children(&node);
        if depth == MAX_HTML_TREE_DEPTH {
            if !descendants.is_empty() {
                return false;
            }
            continue;
        }
        pending.extend(
            descendants
                .into_iter()
                .map(|child| (child, depth.saturating_add(1))),
        );
    }
    true
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
    let mut pending = vec![node.clone()];
    while let Some(current) = pending.pop() {
        if let NodeData::Text { contents } = &current.data {
            out.push_str(&contents.borrow());
        }
        let mut descendants = children(&current);
        descendants.reverse();
        pending.extend(descendants);
    }
    out
}

/// DFS for the first element with local name `tag`.
pub(crate) fn find_element(node: &Handle, tag: &str) -> Option<Handle> {
    let mut pending = vec![node.clone()];
    while let Some(current) = pending.pop() {
        if let NodeData::Element { name, .. } = &current.data
            && &*name.local == tag
        {
            return Some(current);
        }
        let mut descendants = children(&current);
        descendants.reverse();
        pending.extend(descendants);
    }
    None
}

/// Collect descendant elements (and `node` itself) with the given local name.
pub(crate) fn collect_elements(node: &Handle, tag: &str, out: &mut Vec<Handle>) {
    let mut pending = vec![node.clone()];
    while let Some(current) = pending.pop() {
        if let Some(local) = tag_name(&current)
            && local == tag
        {
            out.push(current.clone());
        }
        let mut descendants = children(&current);
        descendants.reverse();
        pending.extend(descendants);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nested_document(levels: usize) -> String {
        let mut html = String::from("<html><body>");
        for _ in 0..levels {
            html.push_str("<div>");
        }
        html.push_str("text");
        for _ in 0..levels {
            html.push_str("</div>");
        }
        html.push_str("</body></html>");
        html
    }

    #[test]
    fn html_tree_depth_is_bounded_before_walks() {
        assert!(
            parse_html(nested_document(MAX_HTML_TREE_DEPTH.saturating_sub(8)).as_bytes()).is_some()
        );
        assert!(parse_html(nested_document(MAX_HTML_TREE_DEPTH + 8).as_bytes()).is_none());
    }

    #[test]
    fn text_and_element_walks_preserve_document_order_without_recursion() {
        let dom = parse_html(b"<html><body><p>a<span>b</span></p><p>c</p></body></html>").unwrap();
        let body = find_element(&dom.document, "body").unwrap();
        assert_eq!(text_content(&body), "abc");
        let mut paragraphs = Vec::new();
        collect_elements(&body, "p", &mut paragraphs);
        assert_eq!(paragraphs.len(), 2);
        assert_eq!(text_content(&paragraphs[0]), "ab");
        assert_eq!(text_content(&paragraphs[1]), "c");
    }
}
