//! Privacy guard for text that is rendered *into* thumbnails.
//!
//! Thumbnails are not ephemeral: Explorer persists them in `thumbcache_*.db`, where they outlive
//! the original file (even after it is deleted or moved to an encrypted volume) and are readable
//! by any process running as the user and by forensic tooling. A plain-text / document preview
//! therefore must never paint credentials. This module:
//!
//! 1. strips control, BiDi-override and zero-width characters (display spoofing, e.g. RTLO
//!    making `invoice\u{202E}fdp.exe` look like `invoiceexe.pdf`), and
//! 2. masks things that look like secrets: PEM private-key blocks, `key = value` pairs whose key
//!    names a credential, credentials embedded in URLs, and well-known token shapes (cloud access
//!    keys, VCS / chat tokens, JWTs, generic high-entropy strings).
//!
//! The detectors are intentionally conservative toward *showing less*: a false positive only
//! hides a few characters of a thumbnail, while a false negative leaks a secret to disk.

#![forbid(unsafe_code)]

/// Replacement shown in place of a masked secret.
pub const MASK: &str = "\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}";

/// Returns `true` for characters that must never reach a GDI text renderer.
pub fn is_spoofing_or_control_char(c: char) -> bool {
    (c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
        || matches!(
            c,
            '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{2069}'
                | '\u{FEFF}'
                | '\u{061C}'
                | '\u{115F}'
                | '\u{1160}'
                | '\u{3164}'
                | '\u{FFA0}'
                | '\u{FFF9}'..='\u{FFFB}'
                | '\u{E0000}'..='\u{E007F}'
        )
}

/// Removes control / BiDi / invisible characters; keeps `\t`, `\r`, `\n`.
pub fn sanitize_display_text(s: &str) -> String {
    s.chars().filter(|&c| !is_spoofing_or_control_char(c)).collect()
}

/// Sanitizes a single-line label (file names in torrent listings, document lines, cells):
/// strips spoofing characters and also folds line breaks/tabs into spaces.
pub fn sanitize_label(s: &str) -> String {
    sanitize_display_text(s)
        .chars()
        .map(|c| if matches!(c, '\n' | '\r' | '\t') { ' ' } else { c })
        .collect()
}

const SENSITIVE_KEY_MARKERS: &[&str] = &[
    "password",
    "passwd",
    "passphrase",
    "pwd",
    "secret",
    "token",
    "apikey",
    "api_key",
    "api-key",
    "accesskey",
    "access_key",
    "access-key",
    "privatekey",
    "private_key",
    "private-key",
    "credential",
    "authorization",
    "auth",
    "bearer",
    "cookie",
    "connectionstring",
    "connection_string",
    "conn_str",
    "dsn",
    "sas",
    "signature",
    "otp",
    "pin",
    "seed",
    "mnemonic",
];

/// Splits an identifier into lowercase words on separators and camelCase boundaries:
/// `accessToken` -> [access, token], `DB_PASSWORD` -> [db, password].
fn key_words(key: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in key.chars() {
        if !c.is_ascii_alphanumeric() {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
            continue;
        }
        if c.is_ascii_uppercase() && prev_lower && !cur.is_empty() {
            words.push(std::mem::take(&mut cur));
        }
        prev_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
        cur.push(c.to_ascii_lowercase());
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
}

fn key_is_sensitive(key: &str) -> bool {
    let k = key.trim();
    if k.is_empty() || k.chars().count() > 64 {
        return false;
    }
    let words = key_words(k);
    if words.is_empty() {
        return false;
    }
    let joined: String = words.concat();
    SENSITIVE_KEY_MARKERS.iter().any(|m| {
        let m_compact: String = m.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        if m_compact.len() >= 6 {
            joined.contains(&m_compact)
        } else {
            // Short markers (`pwd`, `pin`, `otp`, `auth`, `token`...) must be a whole word so that
            // `author`, `spinner` or `tokenizer` are not masked.
            words.iter().any(|w| w == &m_compact)
        }
    })
}

/// Byte offset just after the first `=`/`:` assignment operator that follows a plausible key,
/// together with the key text. Only the first ~64 characters are considered a key.
fn split_assignment(line: &str) -> Option<(&str, usize)> {
    for (idx, ch) in line.char_indices() {
        if idx > 80 {
            return None;
        }
        if ch == '=' || ch == ':' {
            // Skip `://` (URLs) and `==` comparisons.
            let rest = &line[idx + ch.len_utf8()..];
            if ch == ':' && rest.starts_with("//") {
                return None;
            }
            if ch == '=' && rest.starts_with('=') {
                return None;
            }
            return Some((&line[..idx], idx + ch.len_utf8()));
        }
    }
    None
}

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-' | '.')
}

/// Heuristic for a standalone secret token (no surrounding key).
pub fn looks_like_secret_token(tok: &str) -> bool {
    let t = tok.trim_matches(|c: char| matches!(c, '"' | '\'' | '`' | ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>'));
    if t.len() < 16 || t.len() > 4096 || !t.chars().all(is_token_char) {
        return false;
    }
    const PREFIXES: &[&str] = &[
        "AKIA", "ASIA", "AGPA", "AIDA", "AROA", // cloud access key ids
        "ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_", "glpat-", // VCS tokens
        "xoxb-", "xoxp-", "xoxa-", "xoxr-", "xapp-", // chat tokens
        "sk_live_", "rk_live_", "sk-", "AIza", "ya29.", "npm_", "pypi-", "hf_", "SG.",
    ];
    if PREFIXES.iter().any(|p| t.starts_with(p)) && t.len() >= 20 {
        return true;
    }
    // JSON Web Token: three base64url segments, header starts with `eyJ` (`{"`).
    if t.starts_with("eyJ") && t.matches('.').count() == 2 && t.len() >= 30 {
        return true;
    }
    // Generic high-entropy material: long, mixed upper/lower/digit, mostly base64 alphabet.
    if t.len() >= 32 {
        let upper = t.chars().any(|c| c.is_ascii_uppercase());
        let lower = t.chars().any(|c| c.is_ascii_lowercase());
        let digit = t.chars().any(|c| c.is_ascii_digit());
        let dots = t.matches('.').count();
        if upper && lower && digit && dots <= 2 {
            return true;
        }
    }
    false
}

/// Masks `scheme://user:password@host` credentials.
fn redact_url_credentials(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(pos) = rest.find("://") {
        let (head, tail) = rest.split_at(pos + 3);
        out.push_str(head);
        // Authority ends at the first '/', whitespace or quote.
        let auth_end = tail
            .find(|c: char| c == '/' || c.is_whitespace() || c == '"' || c == '\'')
            .unwrap_or(tail.len());
        let authority = &tail[..auth_end];
        if let Some(at) = authority.rfind('@') {
            let userinfo = &authority[..at];
            if let Some(colon) = userinfo.find(':') {
                out.push_str(&userinfo[..colon + 1]);
                out.push_str(MASK);
                out.push_str(&authority[at..]);
            } else {
                out.push_str(authority);
            }
        } else {
            out.push_str(authority);
        }
        rest = &tail[auth_end..];
    }
    out.push_str(rest);
    out
}

/// Masks the value of every `key=value` segment, separated by `;` or `&`, whose key names a
/// credential. Segments without a sensitive key are kept verbatim.
fn redact_delimited_pairs(line: &str) -> String {
    if !line.contains(';') && !line.contains('&') {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len());
    let mut seg_start = 0usize;
    let bytes = line.as_bytes();
    for i in 0..=bytes.len() {
        let at_end = i == bytes.len();
        if at_end || bytes[i] == b';' || bytes[i] == b'&' {
            let seg = &line[seg_start..i];
            let masked = match seg.find('=') {
                Some(eq) if key_is_sensitive(seg[..eq].trim()) && !seg[eq + 1..].trim().is_empty() => {
                    format!("{}={}", &seg[..eq], MASK)
                }
                _ => seg.to_string(),
            };
            out.push_str(&masked);
            if !at_end {
                out.push(bytes[i] as char);
            }
            seg_start = i + 1;
        }
    }
    out
}

/// Redacts a single line (no PEM state). Exposed for callers that render isolated fields.
pub fn redact_line(line: &str) -> String {
    let line = redact_url_credentials(line);

    // 1. `key = value` / `key: value` / `"key": "value"` / `export KEY=value`.
    if let Some((key, val_start)) = split_assignment(&line) {
        let key = key.trim_end();
        let key_tail = key
            .rsplit(|c: char| c.is_whitespace() || c == ',' || c == '{' || c == '(')
            .next()
            .unwrap_or(key);
        if key_is_sensitive(key_tail) {
            let value = line[val_start..].trim();
            if !value.is_empty() {
                let lead = &line[..val_start];
                return format!("{lead} {MASK}");
            }
        }
    }

    // 2. Delimited key/value lists (connection strings `a=b;Password=x;`, query strings `a=b&token=x`).
    let line = redact_delimited_pairs(&line);

    // 3. Standalone token shapes anywhere in the line.
    let mut out = String::with_capacity(line.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if !word.is_empty() {
            if looks_like_secret_token(word) {
                out.push_str(MASK);
            } else {
                out.push_str(word);
            }
            word.clear();
        }
    };
    for c in line.chars() {
        if is_token_char(c) {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

/// Redacts a multi-line text block, preserving line structure. PEM / PGP / OpenSSH private-key
/// blocks are collapsed to a single masked marker line.
pub fn redact_text_block(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_private_block = false;
    for (i, raw_line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        let upper = line.to_ascii_uppercase();
        if upper.contains("-----BEGIN") && (upper.contains("PRIVATE KEY") || upper.contains("PGP PRIVATE")) {
            in_private_block = true;
            out.push_str("-----BEGIN PRIVATE KEY----- ");
            out.push_str(MASK);
            continue;
        }
        if in_private_block {
            if upper.contains("-----END") {
                in_private_block = false;
                out.push_str("-----END PRIVATE KEY-----");
            }
            // Key material lines are dropped entirely.
            continue;
        }
        out.push_str(&redact_line(line));
    }
    out
}

/// Convenience: sanitize + redact a list of already-extracted preview lines.
pub fn redact_lines(lines: Vec<String>) -> Vec<String> {
    let joined = lines.join("\n");
    redact_text_block(&sanitize_display_text(&joined))
        .split('\n')
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_assignments_and_keeps_benign_text() {
        assert!(!redact_line("db_password = hunter2-very-secret").contains("hunter2"));
        assert!(!redact_line("  \"apiKey\": \"abc123XYZ\",").contains("abc123XYZ"));
        assert!(!redact_line("export AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG").contains("wJalr"));
        assert_eq!(redact_line("Hello world, chapter 1"), "Hello world, chapter 1");
        assert_eq!(redact_line("width = 1024"), "width = 1024");
        assert_eq!(redact_line("if a == b { return; }"), "if a == b { return; }");
        assert_eq!(redact_line("author: Jane Doe"), "author: Jane Doe");
        assert!(!redact_line("accessToken: abcdef").contains("abcdef"));
        assert!(!redact_line("Server=db;User Id=sa;Password=P4ss;").contains("P4ss"));
        assert!(!redact_line("Authorization: Bearer abcDEF123456").contains("abcDEF"));
    }

    #[test]
    fn masks_token_shapes_and_url_credentials() {
        let fake_aws = format!("{}{}", "AKIA", "ABCDEFGHIJKLMNOP");
        assert!(!redact_line(&format!("key {fake_aws} here")).contains(&fake_aws));
        let fake_jwt = format!("{}.{}.{}", "eyJhbGciOiJIUzI1NiJ9", "eyJzdWIiOiIxMjM0In0", "c2lnbmF0dXJl");
        assert!(!redact_line(&fake_jwt).contains("eyJzdWIi"));
        let url = "https://alice:Sup3rS3cret@example.invalid/path";
        let red = redact_line(url);
        assert!(!red.contains("Sup3rS3cret"));
        assert!(red.contains("alice:"));
        assert!(red.contains("@example.invalid/path"));
    }

    #[test]
    fn collapses_private_key_blocks() {
        let pem = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\nQUFBQUFB\n-----END OPENSSH PRIVATE KEY-----\nafter";
        let red = redact_text_block(pem);
        assert!(!red.contains("b3BlbnNzaC1"));
        assert!(!red.contains("QUFBQUFB"));
        assert!(red.ends_with("after"));
    }

    #[test]
    fn strips_spoofing_characters() {
        let s = sanitize_label("invoice\u{202E}fdp.exe\u{200B}\tx");
        assert_eq!(s, "invoicefdp.exe x");
    }
}
