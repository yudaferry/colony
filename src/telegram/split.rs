//! Splitting text to Telegram's 4096-unit limit (UTF-16 code units).

pub const TG_LIMIT: usize = 4096;

fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// Byte index of the longest prefix of `s` that fits in `limit` UTF-16 units.
fn fit_end(s: &str, limit: usize) -> usize {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        let n = c.len_utf16();
        if units + n > limit {
            return i;
        }
        units += n;
    }
    s.len()
}

/// Chunks of at most `limit` UTF-16 units whose concatenation is `text`.
/// Prefers to cut after the last `\n` inside the window, else cuts at a char
/// boundary. Empty text gives one chunk `(empty)`.
pub fn split_message(text: &str, limit: usize) -> Vec<String> {
    if text.is_empty() {
        return vec!["(empty)".to_string()];
    }
    // One char can take 2 units; a smaller limit could never make progress.
    let limit = limit.max(2);
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let end = fit_end(rest, limit);
        if end == rest.len() {
            out.push(rest.to_string());
            break;
        }
        let cut = match rest[..end].rfind('\n') {
            Some(p) if p > 0 => p + 1,
            _ => end,
        };
        out.push(rest[..cut].to_string());
        rest = &rest[cut..];
    }
    out
}

/// Keeps the last lines of `text` that fit in `limit` units (for `/last`).
/// If even the last line is too long, keeps its tail.
pub fn tail_fit(text: &str, limit: usize) -> String {
    if utf16_len(text) <= limit {
        return text.to_string();
    }
    let mut kept: Vec<&str> = Vec::new();
    let mut total = 0;
    for line in text.split('\n').rev() {
        let add = utf16_len(line) + usize::from(!kept.is_empty());
        if total + add > limit {
            break;
        }
        total += add;
        kept.push(line);
    }
    if kept.is_empty() {
        let last = text.rsplit('\n').next().unwrap_or("");
        let mut chars: Vec<char> = Vec::new();
        let mut units = 0;
        for c in last.chars().rev() {
            if units + c.len_utf16() > limit {
                break;
            }
            units += c.len_utf16();
            chars.push(c);
        }
        chars.reverse();
        return chars.into_iter().collect();
    }
    kept.reverse();
    kept.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_one_chunk() {
        assert_eq!(split_message("hello", TG_LIMIT), vec!["hello".to_string()]);
        assert_eq!(split_message("", TG_LIMIT), vec!["(empty)".to_string()]);
    }

    #[test]
    fn ten_thousand_chars_split_in_order() {
        let text = "a".repeat(10_000);
        let chunks = split_message(&text, TG_LIMIT);
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|c| c.encode_utf16().count() <= TG_LIMIT));
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn prefers_newline_boundary() {
        let chunks = split_message("aaa\nbbb\nccc", 9);
        assert_eq!(chunks, vec!["aaa\nbbb\n".to_string(), "ccc".to_string()]);
        assert_eq!(chunks.concat(), "aaa\nbbb\nccc");
    }

    #[test]
    fn split_never_breaks_chars_and_respects_utf16_limit() {
        let text = "\u{1F600}".repeat(5_000);
        let chunks = split_message(&text, TG_LIMIT);
        assert!(chunks.len() >= 3);
        for c in &chunks {
            assert!(c.encode_utf16().count() <= TG_LIMIT);
        }
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn emoji_heavy_lines_with_variation_selectors_respect_limit() {
        let line = "\u{1f4e8}\u{26a0}\u{fe0f}\u{23f3}\u{2705} sent \u{1f7e2}\n";
        let text = line.repeat(1_000);
        let chunks = split_message(&text, TG_LIMIT);
        assert!(chunks.len() >= 2);
        for c in &chunks {
            assert!(c.encode_utf16().count() <= TG_LIMIT);
        }
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn tail_fit_keeps_last_lines() {
        assert_eq!(tail_fit("line1\nline2\nline3", 11), "line2\nline3");
        assert_eq!(tail_fit("short", 100), "short");
        let long = "x".repeat(50);
        assert_eq!(tail_fit(&long, 10), "x".repeat(10));
    }
}
