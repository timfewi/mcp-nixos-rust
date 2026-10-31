//! HTML text extraction and option catalogue parsing.

use std::collections::HashSet;
use std::sync::LazyLock;

use scraper::node::Node;
use scraper::{ElementRef, Html, Selector};

use crate::text::{char_len, collapse, first_chars, strip, words};

/// One parsed Home Manager / nix-darwin option.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlOption {
    pub name: String,
    pub description: String,
    pub r#type: String,
}

pub fn selector(css: &str) -> Selector {
    #[allow(clippy::expect_used)]
    Selector::parse(css).expect("static CSS selector is valid")
}

fn collect(element: ElementRef<'_>, skip: &HashSet<ego_tree::NodeId>, out: &mut Vec<String>) {
    for child in element.children() {
        if skip.contains(&child.id()) {
            continue;
        }
        match child.value() {
            Node::Text(text) => out.push(text.to_string()),
            Node::Element(el) if matches!(el.name(), "script" | "style" | "template") => {}
            Node::Element(_) => {
                if let Some(child) = ElementRef::wrap(child) {
                    collect(child, skip, out);
                }
            }
            _ => {}
        }
    }
}

fn pieces(element: ElementRef<'_>, skip: &HashSet<ego_tree::NodeId>) -> Vec<String> {
    let mut out = Vec::new();
    collect(element, skip, &mut out);
    out
}

/// BeautifulSoup `get_text(sep, strip=...)`.
pub fn get_text(element: ElementRef<'_>, separator: &str, strip_nodes: bool) -> String {
    let parts = pieces(element, &HashSet::new());
    if strip_nodes {
        let kept: Vec<&str> = parts.iter().map(|part| strip(part)).filter(|part| !part.is_empty()).collect();
        kept.join(separator)
    } else {
        parts.join(separator)
    }
}

/// Text of a fragment with every `selectors` match removed, joined by spaces.
pub fn fragment_text_without(html: &str, selectors: &[&str]) -> String {
    let doc = Html::parse_fragment(html);
    let mut skip = HashSet::new();
    for css in selectors {
        skip.extend(doc.select(&selector(css)).map(|el| el.id()));
    }
    pieces(doc.root_element(), &skip).join(" ")
}

/// Strip HTML tags and clean up text for plain text output.
pub fn strip_html(html: &str) -> String {
    if html.is_empty() {
        return String::new();
    }
    // Plain text is the common case for catalogue fields and needs no parser.
    if !html.contains(['<', '&']) {
        return collapse(html);
    }
    let doc = Html::parse_fragment(html);
    collapse(&get_text(doc.root_element(), " ", false))
}

fn shorten(description: &str) -> String {
    if char_len(description) > 200 { first_chars(description, 200).to_owned() } else { description.to_owned() }
}

fn next_element<'a>(element: ElementRef<'a>, name: &str) -> Option<ElementRef<'a>> {
    element.next_siblings().filter_map(ElementRef::wrap).find(|sibling| sibling.value().name() == name)
}

static OPT_HEADING: LazyLock<Selector> = LazyLock::new(|| selector(r#"h2[id^="opt-"]"#));
static HEADER_ANCHOR: LazyLock<Selector> = LazyLock::new(|| selector("a.header"));
static EM: LazyLock<Selector> = LazyLock::new(|| selector("em"));
static DT: LazyLock<Selector> = LazyLock::new(|| selector("dt"));
static P: LazyLock<Selector> = LazyLock::new(|| selector("p"));
static TERM: LazyLock<Selector> = LazyLock::new(|| selector("span.term"));
static ANCHOR_ID: LazyLock<Selector> = LazyLock::new(|| selector("a[id]"));

/// Parse options from an mdBook-style document (Home Manager docs).
fn parse_mdbook(doc: &Html) -> Vec<HtmlOption> {
    let mut options = Vec::new();
    for heading in doc.select(&OPT_HEADING) {
        let anchor = heading.select(&HEADER_ANCHOR).next();
        let name = get_text(anchor.unwrap_or(heading), " ", true);
        if name.is_empty() {
            continue;
        }

        let mut description_parts = Vec::new();
        let mut type_info = String::new();
        let mut metadata_started = false;
        for sibling in heading.next_siblings().filter_map(ElementRef::wrap) {
            let tag = sibling.value().name();
            if tag == "h1" || tag == "h2" {
                break;
            }
            if tag != "p" {
                continue;
            }
            let label = sibling.select(&EM).next().map(|em| get_text(em, " ", true)).unwrap_or_default();
            match label.as_str() {
                "Type:" => {
                    metadata_started = true;
                    let text = get_text(sibling, " ", true);
                    type_info = strip(text.strip_prefix("Type:").unwrap_or(&text)).to_owned();
                }
                "Default:" | "Example:" | "Declared by:" => metadata_started = true,
                _ if !metadata_started => description_parts.push(get_text(sibling, " ", true)),
                _ => {}
            }
        }
        options.push(HtmlOption { description: shorten(&description_parts.join(" ")), name, r#type: type_info });
    }
    options
}

fn dt_name(dt: ElementRef<'_>, url: &str) -> String {
    if !url.contains("home-manager") {
        return get_text(dt, "", true);
    }
    if let Some(anchor) = dt.select(&ANCHOR_ID).next() {
        return anchor
            .value()
            .attr("id")
            .and_then(|id| id.strip_prefix("opt-"))
            .map(|id| id.replace("_name_", "<name>"))
            .unwrap_or_default();
    }
    let direct = dt.children().find_map(|child| child.value().as_text().map(|text| text.to_string()));
    match direct {
        Some(text) if !text.is_empty() => strip(&text).to_owned(),
        _ => get_text(dt, "", true),
    }
}

fn dd_type(dd: ElementRef<'_>) -> String {
    if let Some(term) = dd.select(&TERM).next()
        && get_text(term, "", false).contains("Type:")
    {
        return strip(&get_text(term, "", true).replace("Type:", "")).to_owned();
    }
    // DocBook (nix-darwin): `<p><em>Type:</em> null or boolean</p>`. Read the
    // whole paragraph; the label and value are split across elements.
    for para in dd.select(&P) {
        let text = collapse(&get_text(para, " ", false));
        if let Some(rest) = text.strip_prefix("Type:") {
            return strip(rest).to_owned();
        }
    }
    String::new()
}

/// Parse a full option catalogue. Dispatches on document structure, not URL:
/// mdBook docs (Home Manager) mark each option with an h2 `opt-` heading, while
/// DocBook manuals (nix-darwin) use dt/dd definition lists.
pub fn parse_options(html: &str, url: &str) -> Vec<HtmlOption> {
    let doc = Html::parse_document(html);
    if doc.select(&OPT_HEADING).next().is_some() {
        return parse_mdbook(&doc);
    }
    let mut options = Vec::new();
    for dt in doc.select(&DT) {
        let name = dt_name(dt, url);
        if !name.contains('.') && words(&name).count() > 1 {
            continue;
        }
        let Some(dd) = next_element(dt, "dd") else { continue };
        let description = match dd.select(&P).next() {
            Some(para) => get_text(para, "", true),
            None => get_text(dd, "", true).split('\n').next().unwrap_or("").to_owned(),
        };
        options.push(HtmlOption { description: shorten(&description), r#type: dd_type(dd), name });
    }
    options
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_html_collapses_whitespace_and_tags() {
        assert_eq!(strip_html(""), "");
        assert_eq!(strip_html("<p>Hello   <b>world</b>\n!</p>"), "Hello world !");
        assert_eq!(strip_html("a &amp; b"), "a & b");
        assert_eq!(strip_html("plain   text"), "plain text");
    }

    #[test]
    fn text_skips_scripts_styles_and_comments() {
        assert_eq!(strip_html("<p>a<script>x()</script><style>p{}</style><!-- c -->b</p>"), "a b");
    }

    #[test]
    fn mdbook_parses_description_and_type() {
        let html = r##"<main><h2 id="opt-programs.git.enable"><a class="header" href="#x">programs.git.enable</a></h2>
<p>Whether to enable <em>Git</em>.</p>
<p><em>Type:</em> boolean</p>
<p><em>Default:</em> false</p><p>ignored after metadata</p>
<h2 id="opt-b"><a class="header" href="#y">b.c</a></h2><p>Next.</p></main>"##;
        let options = parse_options(html, "https://example.org/print.html");
        assert_eq!(options.len(), 2);
        assert_eq!(options[0].name, "programs.git.enable");
        assert_eq!(options[0].description, "Whether to enable Git .");
        assert_eq!(options[0].r#type, "boolean");
        assert_eq!(options[1].description, "Next.");
    }

    #[test]
    fn docbook_parses_definition_lists() {
        let html = r#"<dl><dt><span class="term"><code>system.defaults.dock.autohide</code></span></dt>
<dd><p>Whether to hide the dock.</p><p><span class="emphasis"><em>Type:</em></span> null or boolean</p></dd>
<dt>Not an option name</dt><dd><p>skip</p></dd></dl>"#;
        let options = parse_options(html, "https://example.org/manual/index.html");
        assert_eq!(options.len(), 1);
        assert_eq!(options[0].name, "system.defaults.dock.autohide");
        assert_eq!(options[0].description, "Whether to hide the dock.");
        assert_eq!(options[0].r#type, "null or boolean");
    }

    #[test]
    fn descriptions_cap_at_200_characters() {
        let body = "x".repeat(300);
        let html = format!(r#"<h2 id="opt-a"><a class="header">a.b</a></h2><p>{body}</p>"#);
        assert_eq!(parse_options(&html, "u")[0].description.len(), 200);
    }
}
