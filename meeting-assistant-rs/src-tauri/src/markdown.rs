//! Markdown to a token stream, for a frontend that builds DOM nodes from it.
//!
//! # Why tokens and not HTML
//!
//! `pulldown_cmark::html::push_html` exists and is one line. It is not used
//! here, and the reason is not the one you would expect.
//!
//! It is **not** primarily about script execution. The app's CSP is
//! `default-src 'self'` with no `unsafe-inline`, so the webview already refuses
//! inline scripts, inline event handlers, `javascript:` URLs, remote images,
//! frames and form posts. An injected `<script>` would not run whichever
//! renderer produced it.
//!
//! It is about **impersonation**. Once this app is a general `.md` handler, the
//! input is any file from anywhere — a download, an attachment, a repository —
//! and an HTML string rendered through `innerHTML` can produce markup that
//! *looks like the app*: a convincing "your API key expired, re-enter it" panel
//! in the app's own fonts, inside a window bearing the app's own name. No script
//! is needed for that, and CSP has no opinion about it.
//!
//! A token stream narrows it sharply. The frontend maps `tag` through a fixed
//! allowlist to `createElement` and assigns `textContent`, so there is no
//! representable value that yields an **input, a button, or any control that
//! accepts what a user types**. Interactive impersonation is genuinely gone.
//!
//! **Visual impersonation is not**, and an earlier version of this comment
//! overstated the claim by saying "or a styled panel". A document can still
//! produce panel-like blocks: `.library-body pre` is a recessed box in the
//! app's own colour, `blockquote` is an accent-bordered one, and a `#` heading
//! renders at the same size as the window's own title. A hostile file can
//! therefore *look* official and offer an accent-coloured link. What it cannot
//! do is take input, and the link is handled by `open_external_url`, which
//! refuses every scheme but `http`, `https` and `mailto`.
//!
//! It also keeps **"no `innerHTML` anywhere"** intact, which is one of the two
//! mitigations the `withGlobalTauri` risk acceptance rests on, rather than
//! weakening it to "no `innerHTML` from untrusted input" at exactly the moment
//! untrusted input becomes the point.
//!
//! # Raw HTML is shown, not deleted
//!
//! CommonMark allows raw HTML and `pulldown-cmark` emits it as `Event::Html`
//! and `Event::InlineHtml`. An earlier version of this module **dropped** both,
//! on the reasoning that nothing downstream would render them anyway.
//!
//! That was silently destructive, and measurably so. An HTML block of type 1
//! (`<pre>`, `<script>`, `<style>`, `<textarea>`) ends only at its closing tag,
//! not at a blank line — so a single unterminated `<pre>` swallows the rest of
//! the file. Measured against the real parser, `"<pre>x\n\n# Heading\n\nbody"`
//! produced **zero tokens**: a blank window, no error, no way to tell that the
//! document had not simply been empty. `<details>` blocks and centred badge rows
//! are ordinary in real README files, and Phase 2 opens arbitrary ones.
//!
//! So the markup is emitted as **text**, inside a code block for a block-level
//! run. Nothing is lost, and nothing becomes markup: the frontend assigns it
//! with `textContent`, so `<script>` arrives on screen as those nine characters.
//! The parser's own structure is still the only thing that produces elements.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag};
use serde::Serialize;

/// One instruction for the frontend's DOM builder.
///
/// Deliberately flat rather than a tree: `pulldown-cmark` is a pull parser and
/// emits a stream, and a stream is what a stack-based builder consumes. Building
/// a tree here only to flatten it there would be work for nothing.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum Token {
    /// Open an element. `tag` is from a closed set — see [`Block`].
    Start {
        tag: &'static str,
        /// Heading level, 1-6. Only present for `heading`.
        #[serde(skip_serializing_if = "Option::is_none")]
        level: Option<u8>,
        /// Link destination, already scheme-checked. Only present for `link`.
        #[serde(skip_serializing_if = "Option::is_none")]
        href: Option<String>,
        /// `true` for an ordered list. Only present for `list`.
        #[serde(skip_serializing_if = "Option::is_none")]
        ordered: Option<bool>,
    },
    /// Close the most recently opened element.
    End,
    /// Literal text. The frontend assigns this with `textContent`.
    Text { v: String },
    /// A hard line break inside a paragraph.
    Break,
}

/// The elements a document may produce. Anything not here is dropped.
///
/// Chosen for what a summary or a README needs, and nothing that carries
/// behaviour or accepts input. There is deliberately no `div`, no `span`, no
/// `img`, no `form`, no `input`, no `button`.
mod block {
    pub const PARAGRAPH: &str = "paragraph";
    pub const HEADING: &str = "heading";
    pub const LIST: &str = "list";
    pub const ITEM: &str = "item";
    pub const CODE: &str = "code";
    /// Inline `` `code` ``, distinct from a fenced block.
    pub const CODE_INLINE: &str = "code_inline";
    pub const QUOTE: &str = "quote";
    pub const EMPHASIS: &str = "emphasis";
    pub const STRONG: &str = "strong";
    pub const STRIKE: &str = "strike";
    pub const LINK: &str = "link";
    pub const RULE: &str = "rule";
    pub const TABLE: &str = "table";
    pub const ROW: &str = "row";
    pub const CELL: &str = "cell";
}

/// Schemes a link may use.
///
/// `javascript:` is the obvious exclusion, but `file:` and `data:` matter as
/// much: the first turns a document into a probe for local paths, and the second
/// can carry a whole payload inline. An unknown scheme is refused rather than
/// allowed, so a new one cannot arrive by default.
const ALLOWED_SCHEMES: [&str; 3] = ["http://", "https://", "mailto:"];

/// Whether a link destination may be offered to the user.
///
/// Relative and fragment links are kept as text but carry no destination —
/// there is nowhere for them to go in a single-document viewer.
fn safe_href(dest: &str) -> Option<String> {
    let trimmed = dest.trim();
    let lowered = trimmed.to_ascii_lowercase();
    ALLOWED_SCHEMES
        .iter()
        .any(|scheme| lowered.starts_with(scheme))
        .then(|| trimmed.to_string())
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn start(tag: &'static str) -> Token {
    Token::Start {
        tag,
        level: None,
        href: None,
        ordered: None,
    }
}

/// Parse Markdown into tokens.
///
/// Tables and strikethrough are enabled because a real-world `.md` uses them.
///
/// Footnotes, task lists and math are **not**: each needs its own `Options`
/// flag, and turning one on without a matching element in the frontend's map
/// would only change how it fails. An earlier version of this comment claimed
/// footnotes were enabled while the code enabled two flags — the arms below for
/// `FootnoteReference`, `TaskListMarker` and the math events are therefore
/// unreachable today, and kept only so enabling a flag cannot silently produce
/// an unhandled event.
///
/// The visible consequence, until they are: `[^1]` renders as an inert `^1` and
/// the footnote's own text is dropped, `- [ ] task` renders as literal
/// `[ ] task`, and `> [!NOTE]` renders as literal `[!NOTE]`.
pub fn to_tokens(source: &str) -> Vec<Token> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);

    let mut out = Vec::new();
    // Consecutive `Event::Html` lines, pending flush as one code block.
    let mut raw_html = String::new();
    // Tracks whether the element opened by each `Start` was emitted, so `End`
    // knows whether to close something. Without it, dropping an unsupported
    // start would leave its end closing an unrelated element.
    let mut emitted: Vec<bool> = Vec::new();

    for event in Parser::new_ext(source, options) {
        // Any non-HTML event ends a run of raw HTML.
        if !matches!(event, Event::Html(_)) && !raw_html.is_empty() {
            flush_html(&mut out, &mut raw_html);
        }

        match event {
            Event::Start(tag) => {
                let token = match tag {
                    Tag::Paragraph => Some(start(block::PARAGRAPH)),
                    Tag::Heading { level, .. } => Some(Token::Start {
                        tag: block::HEADING,
                        level: Some(heading_level(level)),
                        href: None,
                        ordered: None,
                    }),
                    Tag::List(first) => Some(Token::Start {
                        tag: block::LIST,
                        level: None,
                        href: None,
                        ordered: Some(first.is_some()),
                    }),
                    Tag::Item => Some(start(block::ITEM)),
                    Tag::CodeBlock(_) => Some(start(block::CODE)),
                    Tag::BlockQuote(_) => Some(start(block::QUOTE)),
                    Tag::Emphasis => Some(start(block::EMPHASIS)),
                    Tag::Strong => Some(start(block::STRONG)),
                    Tag::Strikethrough => Some(start(block::STRIKE)),
                    Tag::Link { dest_url, .. } => Some(Token::Start {
                        tag: block::LINK,
                        level: None,
                        // A refused scheme leaves the text and drops the
                        // destination, rather than dropping the link's text with
                        // it — the words are still part of the document.
                        href: safe_href(&dest_url),
                        ordered: None,
                    }),
                    Tag::Table(_) => Some(start(block::TABLE)),
                    Tag::TableHead | Tag::TableRow => Some(start(block::ROW)),
                    Tag::TableCell => Some(start(block::CELL)),
                    // Images cannot display: the CSP allows `img-src 'self'
                    // data:` only, so a remote image is blocked and a local path
                    // does not resolve through the asset protocol.
                    //
                    // Dropping the tag keeps the alt text, which is the only
                    // thing a reader can be given. Note that alt text is *not*
                    // plain: CommonMark allows inline markup inside it, so
                    // `![[a](https://x)](y.png)` promotes a real link into the
                    // document. `safe_href` still vets it, but it is a place a
                    // link can hide.
                    Tag::Image { .. } => None,
                    _ => None,
                };

                emitted.push(token.is_some());
                if let Some(token) = token {
                    out.push(token);
                }
            }
            Event::End(end) => {
                // `TagEnd::Image` has no matching `Start` in `emitted` only if
                // the stack is empty, which the parser does not produce.
                let _ = end;
                if emitted.pop().unwrap_or(false) {
                    out.push(Token::End);
                }
            }
            Event::Text(text) => out.push(Token::Text { v: text.into_string() }),
            // Its own element, not folded into text: `` `--force` `` in a
            // sentence should not read as ordinary prose.
            Event::Code(text) => {
                out.push(start(block::CODE_INLINE));
                out.push(Token::Text { v: text.into_string() });
                out.push(Token::End);
            }
            Event::SoftBreak => out.push(Token::Text { v: " ".into() }),
            Event::HardBreak => out.push(Token::Break),
            Event::Rule => {
                out.push(start(block::RULE));
                out.push(Token::End);
            }
            // Kept as text. See the module header: dropping these lost whole
            // documents. Block-level runs arrive one line at a time, so they
            // are accumulated and flushed as a single code block rather than
            // one block per line.
            Event::Html(t) => raw_html.push_str(&t),
            // Inline: emitted in place, so it does not break the paragraph it
            // sits in.
            Event::InlineHtml(t) => out.push(Token::Text { v: t.into_string() }),
            Event::FootnoteReference(_) | Event::TaskListMarker(_) => {}
            Event::InlineMath(t) | Event::DisplayMath(t) => {
                out.push(Token::Text { v: t.into_string() })
            }
        }
    }

    flush_html(&mut out, &mut raw_html);
    out
}

/// Emit accumulated raw HTML as one code block, and clear the buffer.
fn flush_html(out: &mut Vec<Token>, raw: &mut String) {
    let trimmed = raw.trim();
    if !trimmed.is_empty() {
        out.push(start(block::CODE));
        out.push(Token::Text {
            v: trimmed.to_string(),
        });
        out.push(Token::End);
    }
    raw.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(tokens: &[Token]) -> String {
        tokens
            .iter()
            .filter_map(|t| match t {
                Token::Text { v } => Some(v.as_str()),
                _ => None,
            })
            .collect()
    }

    fn tags(tokens: &[Token]) -> Vec<&str> {
        tokens
            .iter()
            .filter_map(|t| match t {
                Token::Start { tag, .. } => Some(*tag),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn renders_what_the_apps_own_summaries_contain() {
        // Measured across every summary on a real machine: headings, bold and
        // bullet lists, and nothing else.
        let tokens = to_tokens("# Executive summary\n\n**Decisions**\n\n* One\n* Two\n");
        let t = tags(&tokens);
        assert!(t.contains(&block::HEADING), "{t:?}");
        assert!(t.contains(&block::STRONG), "{t:?}");
        assert!(t.contains(&block::LIST), "{t:?}");
        assert!(t.contains(&block::ITEM), "{t:?}");
        assert!(text_of(&tokens).contains("Executive summary"));
    }

    /// Raw HTML must become *text*, never an element.
    ///
    /// Note what is deliberately NOT asserted: that `steal()` is absent. It is
    /// present, as the literal characters `<script>steal()</script>`, and that
    /// is the intended outcome — the alternative is deleting content the user
    /// can see in their own file. It cannot execute: the frontend assigns it
    /// with `textContent`, and no `script` element is ever constructed.
    #[test]
    fn a_script_tag_becomes_inert_text_and_never_an_element() {
        let tokens = to_tokens("Hello\n\n<script>steal()</script>\n\n<b>bold</b> word\n");

        for tag in tags(&tokens) {
            assert!(
                !tag.eq_ignore_ascii_case("script"),
                "a script element reached the token stream"
            );
        }
        assert!(
            text_of(&tokens).contains("<script>"),
            "the markup should be visible as text, not deleted: {:?}",
            text_of(&tokens)
        );
        assert!(text_of(&tokens).contains("word"), "surrounding text was lost");
    }

    /// The bug that made dropping raw HTML unacceptable.
    ///
    /// A CommonMark HTML block of type 1 ends only at its closing tag, not at a
    /// blank line, so an unterminated `<pre>` consumes the rest of the file.
    /// While these events were dropped this produced **zero tokens** — a blank
    /// window with no error, indistinguishable from an empty document.
    #[test]
    fn an_unterminated_html_block_does_not_swallow_the_document() {
        let tokens = to_tokens("<pre>x\n\n# Real heading\n\nreal body\n");

        assert!(!tokens.is_empty(), "the whole document was lost");
        let text = text_of(&tokens);
        assert!(
            text.contains("Real heading") && text.contains("real body"),
            "content after the unterminated block was lost: {text:?}"
        );
    }

    #[test]
    fn a_details_block_keeps_the_prose_around_it() {
        let tokens = to_tokens("<details>\n<summary>s</summary>\n\n# Inside\n\n</details>");
        assert!(text_of(&tokens).contains("Inside"));
        // The markup itself is visible rather than silently removed.
        assert!(text_of(&tokens).contains("<details>"));
    }

    #[test]
    fn a_javascript_link_keeps_its_text_and_loses_its_destination() {
        let tokens = to_tokens("[click me](javascript:alert(1))");

        assert!(
            tags(&tokens).contains(&block::LINK),
            "the link element should still exist"
        );
        assert!(
            text_of(&tokens).contains("click me"),
            "the words are part of the document and must survive"
        );

        let hrefs: Vec<_> = tokens
            .iter()
            .filter_map(|t| match t {
                Token::Start { href, .. } => href.clone(),
                _ => None,
            })
            .collect();
        assert!(hrefs.is_empty(), "a javascript: destination got through: {hrefs:?}");
    }

    #[test]
    fn file_and_data_destinations_are_refused_too() {
        for dest in ["file:///etc/passwd", "data:text/html,<script>x</script>", "DATA:x", "JavaScript:x"] {
            assert_eq!(
                safe_href(dest),
                None,
                "{dest} should not be offered as a destination"
            );
        }
    }

    #[test]
    fn ordinary_destinations_are_kept() {
        assert_eq!(
            safe_href("https://example.com/x"),
            Some("https://example.com/x".to_string())
        );
        assert_eq!(safe_href("  http://example.com  "), Some("http://example.com".into()));
        assert_eq!(safe_href("mailto:a@b.c"), Some("mailto:a@b.c".into()));
        // Relative links have nowhere to go in a single-document viewer.
        assert_eq!(safe_href("./other.md"), None);
        assert_eq!(safe_href("#section"), None);
    }

    #[test]
    fn an_image_leaves_its_alt_text_and_no_element() {
        let tokens = to_tokens("![a diagram](https://example.com/x.png)");
        assert!(
            tags(&tokens).iter().all(|t| *t != "image"),
            "images cannot display under this CSP and must not be emitted"
        );
        assert!(
            text_of(&tokens).contains("a diagram"),
            "the alt text is the only thing a reader can be given"
        );
    }

    /// The attack this design exists to prevent: markup that impersonates the
    /// app's own interface. It needs no script, so CSP does not stop it.
    #[test]
    fn a_fabricated_login_panel_cannot_be_expressed() {
        let tokens = to_tokens(
            "<div class=\"panel\"><label>API key expired. Re-enter it:</label>\
             <input name=\"key\"><button>Save</button></div>",
        );

        for tag in tags(&tokens) {
            assert!(
                !matches!(tag, "div" | "input" | "button" | "form" | "label" | "span"),
                "{tag} is expressible, so a document can impersonate the app"
            );
        }
    }

    #[test]
    fn dropping_an_unsupported_start_does_not_close_the_wrong_element() {
        // The image start is dropped; its end must not close the paragraph.
        let tokens = to_tokens("Before ![alt](x.png) after");

        let starts = tokens
            .iter()
            .filter(|t| matches!(t, Token::Start { .. }))
            .count();
        let ends = tokens.iter().filter(|t| matches!(t, Token::End)).count();
        assert_eq!(starts, ends, "unbalanced stream: {tokens:?}");
    }

    #[test]
    fn tables_and_code_survive() {
        let tokens = to_tokens("| a | b |\n|---|---|\n| 1 | 2 |\n\n```\ncode\n```\n");
        let t = tags(&tokens);
        assert!(t.contains(&block::TABLE), "{t:?}");
        assert!(t.contains(&block::CELL), "{t:?}");
        assert!(t.contains(&block::CODE), "{t:?}");
        assert!(text_of(&tokens).contains("code"));
    }
}

