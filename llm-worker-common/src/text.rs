//! Helpers for untrusted text (chat messages, LLM output).

/// Collapse every kind of line break into a single space so a chat message
/// occupies exactly one line of a prompt. Prompts list messages as
/// `[id]: content` lines; a raw `\r`, `\u{2028}`, ... inside content would
/// otherwise let a message forge additional `[id]: ...` lines.
pub fn single_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if is_line_break(c) {
            // treat "\r\n" as one break
            if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push(' ');
        } else if c == '\0' {
            // dropped: Postgres text cannot store NUL
        } else {
            out.push(c);
        }
    }
    out
}

/// Characters that terminals, tokenizers or humans treat as a line break.
pub fn is_line_break(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r' | '\u{000B}' | '\u{000C}' | '\u{0085}' | '\u{2028}' | '\u{2029}'
    )
}

/// Keep at most `max_chars` characters, appending `…` when truncated.
pub fn truncate_chars(s: &str, max_chars: usize) -> String {
    match s.char_indices().nth(max_chars) {
        None => s.to_string(),
        Some((idx, _)) => {
            let mut t = s[..idx].to_string();
            t.push('…');
            t
        }
    }
}

/// Remove NUL characters, which Postgres `text` cannot store (SQLSTATE 22021).
pub fn strip_nul(s: &str) -> String {
    if s.contains('\0') {
        s.replace('\0', "")
    } else {
        s.to_string()
    }
}

/// Strip a surrounding markdown code fence (```json ... ```), which some
/// providers emit despite being asked for bare JSON.
pub fn strip_code_fence(raw: &str) -> &str {
    let t = raw.trim();
    let Some(rest) = t.strip_prefix("```") else {
        return t;
    };
    let Some(body) = rest.strip_suffix("```") else {
        return t;
    };
    // drop an info string such as "json" on the opening fence line
    match body.find('\n') {
        Some(nl) if !body[..nl].contains(['{', '[']) => body[nl + 1..].trim(),
        _ => body.trim(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_line_flattens_all_line_breaks() {
        assert_eq!(single_line("a\nb"), "a b");
        assert_eq!(single_line("a\r\nb"), "a b");
        assert_eq!(
            single_line("a\rb\u{2028}c\u{2029}d\u{85}e\u{b}f\u{c}g"),
            "a b c d e f g"
        );
        assert_eq!(single_line("nul\0byte"), "nulbyte");
        assert_eq!(single_line("plain"), "plain");
    }

    #[test]
    fn truncation_is_char_based() {
        assert_eq!(truncate_chars("héllo", 10), "héllo");
        assert_eq!(truncate_chars("héllo", 2), "hé…");
        assert_eq!(truncate_chars("", 0), "");
        assert_eq!(truncate_chars("ab", 0), "…");
    }

    #[test]
    fn code_fences() {
        assert_eq!(strip_code_fence("```json\n{\"a\":1}\n```"), "{\"a\":1}");
        assert_eq!(strip_code_fence("```\n{\"a\":1}```"), "{\"a\":1}");
        assert_eq!(strip_code_fence("```{\"a\":1}```"), "{\"a\":1}");
        assert_eq!(strip_code_fence("  {\"a\":1}  "), "{\"a\":1}");
        assert_eq!(strip_code_fence("```"), "```");
        assert_eq!(strip_code_fence("``````"), "");
    }
}
