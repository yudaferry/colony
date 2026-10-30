//! Simple Markdown to Telegram HTML, chunking after conversion, and sending with a
//! plain-text fallback. Supported: `**bold**`/`__bold__`, `` `code` ``, fenced
//! blocks, `- `/`* ` bullets and `#` headings. Everything else is HTML-escaped.

use super::api::TelegramApi;
use super::split::{TG_LIMIT, split_message};
use anyhow::Result;

fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// Escapes `&`, `<` and `>` (the only characters Telegram HTML needs).
pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

/// Inline conversion of one line: code spans, then bold (unless `bold` is false).
fn inline(s: &str, bold: bool) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        if let Some(r) = rest.strip_prefix('`')
            && let Some(end) = r.find('`')
            && end > 0
        {
            out.push_str("<code>");
            out.push_str(&escape_html(&r[..end]));
            out.push_str("</code>");
            i += end + 2;
            continue;
        }
        if bold {
            let marker = if rest.starts_with("**") {
                Some("**")
            } else if rest.starts_with("__") {
                Some("__")
            } else {
                None
            };
            if let Some(m) = marker
                && let Some(end) = rest[2..].find(m)
                && end > 0
            {
                out.push_str("<b>");
                out.push_str(&inline(&rest[2..2 + end], false));
                out.push_str("</b>");
                i += end + 4;
                continue;
            }
        }
        let c = rest.chars().next().unwrap();
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
        i += c.len_utf8();
    }
    out
}

enum Kind {
    Text,
    Pre,
    FenceOpen,
    FenceClose,
}

struct Item<'a> {
    kind: Kind,
    html: String,
    raw: &'a str,
}

fn parse(md: &str) -> Vec<Item<'_>> {
    let mut items = Vec::new();
    let mut in_fence = false;
    for line in md.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            let kind = if in_fence { Kind::FenceClose } else { Kind::FenceOpen };
            in_fence = !in_fence;
            items.push(Item { kind, html: String::new(), raw: line });
        } else if in_fence {
            items.push(Item { kind: Kind::Pre, html: escape_html(line), raw: line });
        } else {
            let indent = &line[..line.len() - trimmed.len()];
            let hashes = trimmed.chars().take_while(|&c| c == '#').count();
            let html = if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
                format!("<b>{}</b>", inline(trimmed[hashes..].trim(), false))
            } else if let Some(r) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
                format!("{indent}\u{2022} {}", inline(r, true))
            } else {
                inline(line, true)
            };
            items.push(Item { kind: Kind::Text, html, raw: line });
        }
    }
    items
}

/// One message to send: HTML text, its plain (original Markdown) fallback, and
/// whether it can only go as plain text (an over-long single line).
#[derive(Debug, PartialEq, Eq)]
pub struct Chunk {
    pub html: String,
    pub plain: String,
    pub plain_only: bool,
}

const PRE_CLOSE: &str = "</pre>";

/// Converts Markdown to HTML chunks of at most `limit` UTF-16 units each, cut only
/// on line boundaries. A fenced block that spans chunks is closed and reopened.
pub fn to_chunks(md: &str, limit: usize) -> Vec<Chunk> {
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut html = String::new();
    let mut raw: Vec<&str> = Vec::new();
    let mut open_pre = false;

    fn flush(chunks: &mut Vec<Chunk>, html: &mut String, raw: &mut Vec<&str>, open_pre: &mut bool) {
        if *open_pre {
            html.push_str(PRE_CLOSE);
            *open_pre = false;
        }
        if !html.trim().is_empty() {
            chunks.push(Chunk { html: std::mem::take(html), plain: raw.join("\n"), plain_only: false });
        }
        html.clear();
        raw.clear();
    }

    for item in parse(md) {
        // Text this item would append, and the pre state afterwards.
        let (add, pre_after) = match item.kind {
            Kind::Text => {
                let sep = if html.is_empty() { "" } else { "\n" };
                (format!("{sep}{}", item.html), false)
            }
            Kind::Pre => {
                if open_pre {
                    (format!("\n{}", item.html), true)
                } else {
                    let sep = if html.is_empty() { "" } else { "\n" };
                    (format!("{sep}<pre>{}", item.html), true)
                }
            }
            Kind::FenceOpen => (String::new(), open_pre),
            Kind::FenceClose => (if open_pre { PRE_CLOSE.to_string() } else { String::new() }, false),
        };
        // A text line after an unclosed pre cannot happen (parse closes it), but be safe.
        let reserve = if pre_after && matches!(item.kind, Kind::Pre) { PRE_CLOSE.len() } else { 0 };
        if utf16_len(&html) + utf16_len(&add) + reserve > limit && !html.is_empty() {
            flush(&mut chunks, &mut html, &mut raw, &mut open_pre);
            add_alone(&mut chunks, &mut html, &mut raw, &mut open_pre, &item, limit);
            continue;
        }
        if utf16_len(&add) + reserve > limit {
            add_alone(&mut chunks, &mut html, &mut raw, &mut open_pre, &item, limit);
            continue;
        }
        html.push_str(&add);
        raw.push(item.raw);
        open_pre = pre_after && matches!(item.kind, Kind::Pre) || (matches!(item.kind, Kind::FenceOpen) && open_pre);
    }
    flush(&mut chunks, &mut html, &mut raw, &mut open_pre);
    if chunks.is_empty() {
        chunks.push(Chunk { html: "(empty)".into(), plain: "(empty)".into(), plain_only: false });
    }
    chunks
}

/// Adds `item` to a fresh (empty) chunk; a line too long even alone goes out as
/// plain-text pieces.
fn add_alone<'a>(
    chunks: &mut Vec<Chunk>,
    html: &mut String,
    raw: &mut Vec<&'a str>,
    open_pre: &mut bool,
    item: &Item<'a>,
    limit: usize,
) {
    let (add, pre_after) = match item.kind {
        Kind::Text => (item.html.clone(), false),
        Kind::Pre => (format!("<pre>{}", item.html), true),
        _ => (String::new(), false),
    };
    let reserve = if pre_after { PRE_CLOSE.len() } else { 0 };
    if utf16_len(&add) + reserve > limit {
        // Too long even alone: ship the original line as plain pieces.
        if !html.is_empty() {
            if *open_pre {
                html.push_str(PRE_CLOSE);
                *open_pre = false;
            }
            chunks.push(Chunk { html: std::mem::take(html), plain: raw.join("\n"), plain_only: false });
            raw.clear();
        }
        for piece in split_message(item.raw, limit) {
            chunks.push(Chunk { html: String::new(), plain: piece, plain_only: true });
        }
        return;
    }
    html.push_str(&add);
    raw.push(item.raw);
    *open_pre = pre_after;
}

/// Separator line of the sender header: 14 box-drawing characters.
const HEADER_RULE: &str = "\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}";

/// Sender header as `(html, plain)`: "🤖 **NAME**", a rule, an empty line. The name is
/// upper-cased, stripped of control characters and escaped, so it cannot add markup.
pub fn sender_header(name: &str) -> (String, String) {
    let clean: String = name
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_uppercase();
    (
        format!("\u{1f916} <b>{}</b>\n{HEADER_RULE}\n\n", escape_html(&clean)),
        format!("\u{1f916} {clean}\n{HEADER_RULE}\n\n"),
    )
}

/// Sends Markdown `text` as HTML chunks. A chunk Telegram rejects is resent once as
/// plain text (its original Markdown). `reply_to` goes on the first chunk only, and so
/// does the sender header (when `sender` is given).
pub fn send_markdown(
    api: &dyn TelegramApi,
    chat_id: i64,
    text: &str,
    reply_to: Option<i64>,
    sender: Option<&str>,
) -> Result<()> {
    let header = sender.map(sender_header);
    let (hh, hp) = header.as_ref().map_or(("", ""), |(h, p)| (h.as_str(), p.as_str()));
    // Room for the header on every chunk, so the first one stays within the limit.
    let limit = TG_LIMIT.saturating_sub(utf16_len(hh).max(utf16_len(hp)));
    for (i, c) in to_chunks(text, limit).iter().enumerate() {
        let rt = if i == 0 { reply_to } else { None };
        let (h, p) = if i == 0 { (hh, hp) } else { ("", "") };
        if c.plain_only {
            api.send_message(chat_id, &format!("{p}{}", c.plain), rt)?;
        } else if api.send_html(chat_id, &format!("{h}{}", c.html), rt).is_err() {
            api.send_message(chat_id, &format!("{p}{}", c.plain), rt)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telegram::api::FakeTelegram;

    fn one(md: &str) -> String {
        let c = to_chunks(md, TG_LIMIT);
        assert_eq!(c.len(), 1, "{c:?}");
        c[0].html.clone()
    }

    #[test]
    fn bold_and_code() {
        assert_eq!(one("a **b** and __c__ `d`"), "a <b>b</b> and <b>c</b> <code>d</code>");
        assert_eq!(one("`**x**`"), "<code>**x**</code>");
        assert_eq!(one("lone ** star and ` tick"), "lone ** star and ` tick");
    }

    #[test]
    fn escaping() {
        assert_eq!(one("a < b & c > d <b>x</b>"), "a &lt; b &amp; c &gt; d &lt;b&gt;x&lt;/b&gt;");
        assert_eq!(one("`<x>` **&**"), "<code>&lt;x&gt;</code> <b>&amp;</b>");
    }

    #[test]
    fn bullets_and_headings() {
        assert_eq!(one("- one\n* two\n  - three\n**not bullet**"), "\u{2022} one\n\u{2022} two\n  \u{2022} three\n<b>not bullet</b>");
        assert_eq!(one("# Title\n## Sub **x**\n#nospace"), "<b>Title</b>\n<b>Sub **x**</b>\n#nospace");
    }

    #[test]
    fn fenced_block() {
        assert_eq!(
            one("before\n```rust\nlet a = 1 < 2;\n- not bullet\n```\nafter"),
            "before\n<pre>let a = 1 &lt; 2;\n- not bullet</pre>\nafter"
        );
        // Unclosed fence is closed.
        assert_eq!(one("```\nx"), "<pre>x</pre>");
    }

    #[test]
    fn empty_text() {
        assert_eq!(one(""), "(empty)");
    }

    /// Every tag is balanced and no chunk ends inside a tag or entity.
    fn balanced(h: &str) {
        for t in ["b", "code", "pre"] {
            assert_eq!(h.matches(&format!("<{t}>")).count(), h.matches(&format!("</{t}>")).count(), "{h}");
        }
        assert!(h.matches('<').count() == h.matches('>').count(), "{h}");
    }

    #[test]
    fn split_never_breaks_a_tag() {
        let mut md = String::from("**head**\n```\n");
        for i in 0..400 {
            md.push_str(&format!("line {i} <x> & more padding text here\n"));
        }
        md.push_str("```\n");
        for i in 0..300 {
            md.push_str(&format!("- item **{i}** with `code` and <angle>\n"));
        }
        let chunks = to_chunks(&md, 1000);
        assert!(chunks.len() > 5);
        for c in &chunks {
            assert!(utf16_len(&c.html) <= 1000, "{}", utf16_len(&c.html));
            balanced(&c.html);
        }
        assert!(chunks.iter().filter(|c| c.html.contains("<pre>")).count() > 1);
        // Nothing lost: every line number shows up.
        let all: String = chunks.iter().map(|c| c.html.as_str()).collect::<Vec<_>>().join("\n");
        assert!(all.contains("line 0 ") && all.contains("line 399 ") && all.contains("<b>299</b>"));
    }

    #[test]
    fn overlong_line_goes_plain_in_order() {
        let md = format!("**a**\n{}\n**z**", "x".repeat(10_000));
        let chunks = to_chunks(&md, TG_LIMIT);
        assert_eq!(chunks.len(), 5);
        assert!(chunks[1].plain_only && chunks[2].plain_only && chunks[3].plain_only);
        assert_eq!(chunks[0].html, "<b>a</b>");
        assert_eq!(chunks[4].html, "<b>z</b>");
        assert!(chunks.iter().all(|c| utf16_len(&c.html) <= TG_LIMIT && utf16_len(&c.plain) <= TG_LIMIT));
    }

    #[test]
    fn sends_html_then_falls_back_to_plain() {
        let api = FakeTelegram::new();
        send_markdown(&api, 5, "**hi** <x>", Some(9), None).unwrap();
        assert_eq!(api.sent.borrow().as_slice(), &[(5, "<b>hi</b> &lt;x&gt;".to_string(), Some(9))]);
        assert_eq!(api.html_sent.borrow().len(), 1);

        let api = FakeTelegram::new();
        api.reject_html.set(true);
        send_markdown(&api, 5, "**hi** <x>", Some(9), None).unwrap();
        // Plain resend keeps the original text, nothing lost.
        assert_eq!(api.sent.borrow().as_slice(), &[(5, "**hi** <x>".to_string(), Some(9))]);
        assert!(api.html_sent.borrow().is_empty());
    }

    #[test]
    fn both_failing_is_an_error() {
        let api = FakeTelegram::new();
        api.fail_send.set(true);
        assert!(send_markdown(&api, 5, "x", None, None).is_err());
    }
}
