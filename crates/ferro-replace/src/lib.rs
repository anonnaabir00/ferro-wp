//! Streaming, serialization-aware search-replace.
//!
//! Replaces byte strings like a naive tool, but tracks PHP serialized
//! strings (`s:N:"..."`) and recomputes their lengths, recursing into
//! nested serialized data. In SQL mode it understands mysqldump string
//! escaping (backslash sequences, including the escaped structural
//! quotes `s:N:\"...\"`) and re-escapes on output.

mod serialized;

pub use serialized::validate_serialized;

use std::io::{Read, Write};

const EMIT_CHUNK: usize = 1 << 20; // flush output every 1 MiB

/// A set of (from, to) byte pairs, longest-first so specific variants
/// win over shorter overlapping ones. In SQL mode an additional list
/// with mysqldump-escaped variants is used (dump text escapes
/// backslashes and quotes).
#[derive(Debug, Clone)]
pub struct Replacer {
    pairs: Vec<(Vec<u8>, Vec<u8>)>,
    sql_pairs: Vec<(Vec<u8>, Vec<u8>)>,
    max_needle: usize,
}

impl Replacer {
    pub fn new(pairs: &[(String, String)]) -> Self {
        let pairs = prepare(pairs);
        let mut sql_pairs = pairs.clone();
        sql_pairs.extend(
            pairs
                .iter()
                .map(|(f, t)| (sql_escape_bytes(f), sql_escape_bytes(t))),
        );
        sql_pairs.sort_by_key(|(f, _)| std::cmp::Reverse(f.len()));
        sql_pairs.dedup();
        let max_needle = sql_pairs.iter().map(|(f, _)| f.len()).max().unwrap_or(0);
        Self {
            pairs,
            sql_pairs,
            max_needle,
        }
    }

    /// The variant set for URL replacement: the URL itself, the other
    /// scheme, protocol-relative form, and the JSON-escaped form.
    pub fn url_variants(from: &str, to: &str) -> Vec<(String, String)> {
        let mut pairs = vec![(from.to_string(), to.to_string())];
        if let Some(rest) = from.strip_prefix("https://") {
            pairs.push((format!("http://{rest}"), to.to_string()));
        } else if let Some(rest) = from.strip_prefix("http://") {
            pairs.push((format!("https://{rest}"), to.to_string()));
        }
        if let Some(scheme_pos) = from.find("://") {
            let bare = &from[scheme_pos + 3..];
            let to_bare = to.find("://").map(|p| &to[p + 3..]).unwrap_or(to);
            pairs.push((format!("//{bare}"), format!("//{to_bare}")));
        }
        // JSON-escaped variants (https:\/\/example.com).
        for (f, t) in pairs.clone() {
            let f_esc = f.replace('/', "\\/");
            let t_esc = t.replace('/', "\\/");
            if f_esc != f {
                pairs.push((f_esc, t_esc));
            }
        }
        pairs
    }

    fn match_needle<'a>(&'a self, at: &'a [u8], sql_escape: bool) -> Option<(&'a [u8], &'a [u8])> {
        let pairs = if sql_escape {
            &self.sql_pairs
        } else {
            &self.pairs
        };
        pairs
            .iter()
            .find(|(from, _)| at.starts_with(from))
            .map(|(from, to)| (from.as_slice(), to.as_slice()))
    }

    /// Replace in a complete in-memory buffer.
    pub fn replace(&self, input: &[u8], sql_escape: bool) -> Vec<u8> {
        let mut out = Vec::with_capacity(input.len() + input.len() / 16);
        self.process_slice(input, sql_escape, &mut out);
        out
    }

    /// Replace a stream without holding it all in memory. Bounded by
    /// the largest single serialized string, not the input size.
    pub fn replace_stream<R: Read, W: Write>(
        &self,
        reader: R,
        mut writer: W,
        sql_escape: bool,
    ) -> std::io::Result<u64> {
        let mut window = Window::new(reader, self.max_needle + 64);
        let mut out: Vec<u8> = Vec::with_capacity(EMIT_CHUNK);
        let mut written = 0u64;
        loop {
            window.fill()?;
            if window.at_eof() {
                break;
            }
            if let Some((len, header_len)) = parse_serialized_header(window.peek(), sql_escape) {
                if let Some(content) = window.read_serialized_string(len, header_len, sql_escape)? {
                    // Verified well-formed: process recursively and emit.
                    // Content is logical (unescaped): recurse in raw mode,
                    // then re-escape the result for the dump.
                    let mut inner = Vec::with_capacity(content.len() + 64);
                    self.process_slice(&content, false, &mut inner);
                    write_escaped(&mut out, format!("s:{}:", inner.len()).as_bytes(), false);
                    write_struct_quote(&mut out, sql_escape);
                    write_escaped(&mut out, &inner, sql_escape);
                    write_struct_quote(&mut out, sql_escape);
                } else {
                    // Not actually a serialized string; copy one byte.
                    out.push(window.next_byte().unwrap());
                }
            } else if let Some((from, to)) = self.match_needle(window.peek(), sql_escape) {
                let len = from.len();
                out.extend_from_slice(to);
                window.consume(len);
            } else {
                out.push(window.next_byte().unwrap());
            }
            if out.len() >= EMIT_CHUNK {
                writer.write_all(&out)?;
                written += out.len() as u64;
                out.clear();
            }
        }
        writer.write_all(&out)?;
        written += out.len() as u64;
        Ok(written)
    }

    /// Recursive core over a complete slice.
    fn process_slice(&self, input: &[u8], sql_escape: bool, out: &mut Vec<u8>) {
        let mut pos = 0;
        while pos < input.len() {
            let at = &input[pos..];
            if let Some((len, header_len)) = parse_serialized_header(at, sql_escape) {
                if let Some((content, consumed)) =
                    read_serialized_body(at, header_len, len, sql_escape)
                {
                    let mut inner = Vec::with_capacity(content.len() + 64);
                    self.process_slice(&content, false, &mut inner);
                    write_escaped(out, format!("s:{}:", inner.len()).as_bytes(), false);
                    write_struct_quote(out, sql_escape);
                    write_escaped(out, &inner, sql_escape);
                    write_struct_quote(out, sql_escape);
                    pos += consumed;
                    continue;
                }
            }
            if let Some((from, to)) = self.match_needle(at, sql_escape) {
                out.extend_from_slice(to);
                pos += from.len();
                continue;
            }
            out.push(at[0]);
            pos += 1;
        }
    }
}

/// Parse `s:<digits>:"` at the start of `at` (or the dump-escaped
/// `s:<digits>:\"` when `sql_escape`). Returns (length, header bytes).
fn parse_serialized_header(at: &[u8], sql_escape: bool) -> Option<(usize, usize)> {
    if at.len() < 5 || at[0] != b's' || at[1] != b':' {
        return None;
    }
    let mut len: usize = 0;
    let mut i = 2;
    while i < at.len() && at[i].is_ascii_digit() {
        len = len.checked_mul(10)?.checked_add((at[i] - b'0') as usize)?;
        i += 1;
    }
    if i == 2 {
        return None;
    }
    if at.get(i) == Some(&b':') && at.get(i + 1) == Some(&b'"') {
        return Some((len, i + 2));
    }
    if sql_escape
        && at.get(i) == Some(&b':')
        && at.get(i + 1) == Some(&b'\\')
        && at.get(i + 2) == Some(&b'"')
    {
        return Some((len, i + 3));
    }
    None
}

fn prepare(pairs: &[(String, String)]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut pairs: Vec<(Vec<u8>, Vec<u8>)> = pairs
        .iter()
        .filter(|(from, _)| !from.is_empty())
        .map(|(f, t)| (f.clone().into_bytes(), t.clone().into_bytes()))
        .collect();
    pairs.sort_by_key(|(f, _)| std::cmp::Reverse(f.len()));
    pairs.dedup();
    pairs
}

/// mysqldump escaping of a byte string (for needle variants).
fn sql_escape_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 8);
    write_escaped(&mut out, bytes, true);
    out
}

/// Read a serialized string body starting at `start` within `at`.
/// Returns (logical content, total consumed bytes incl. closing quote).
/// SQL escape sequences are translated when `sql_escape` is set.
fn read_serialized_body(
    at: &[u8],
    start: usize,
    len: usize,
    sql_escape: bool,
) -> Option<(Vec<u8>, usize)> {
    let mut content = Vec::with_capacity(len);
    let mut i = start;
    while content.len() < len {
        let &b = at.get(i)?;
        if sql_escape && b == b'\\' {
            let &esc = at.get(i + 1)?;
            let translated = match esc {
                b'0' => 0u8,
                b'n' => b'\n',
                b'r' => b'\r',
                b't' => b'\t',
                b'Z' => 0x1a,
                b'\\' => b'\\',
                b'\'' => b'\'',
                b'"' => b'"',
                other => other,
            };
            content.push(translated);
            i += 2;
        } else {
            content.push(b);
            i += 1;
        }
    }
    // Closing structural quote: raw `"`, or mysqldump-escaped `\"`.
    match at.get(i) {
        Some(&b'"') => Some((content, i + 1)),
        Some(&b'\\') if sql_escape && at.get(i + 1) == Some(&b'"') => Some((content, i + 2)),
        _ => None,
    }
}

fn write_escaped(out: &mut Vec<u8>, bytes: &[u8], sql_escape: bool) {
    if !sql_escape {
        out.extend_from_slice(bytes);
        return;
    }
    for &b in bytes {
        match b {
            0 => out.extend_from_slice(b"\\0"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            0x1a => out.extend_from_slice(b"\\Z"),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\'' => out.extend_from_slice(b"\\'"),
            b'"' => out.extend_from_slice(b"\\\""),
            _ => out.push(b),
        }
    }
}

/// The structural quote around a serialized string, escaped the way
/// mysqldump escapes it inside a string literal when in SQL mode.
fn write_struct_quote(out: &mut Vec<u8>, sql_escape: bool) {
    if sql_escape {
        out.extend_from_slice(b"\\\"");
    } else {
        out.push(b'"');
    }
}

/// Buffered reader with unbounded lookahead (grows when a serialized
/// string needs it; compacts after consumption).
struct Window<R: Read> {
    reader: R,
    buf: Vec<u8>,
    pos: usize,
    eof: bool,
    min_lookahead: usize,
}

impl<R: Read> Window<R> {
    fn new(reader: R, min_lookahead: usize) -> Self {
        Self {
            reader,
            buf: Vec::with_capacity(1 << 20),
            pos: 0,
            eof: false,
            min_lookahead,
        }
    }

    fn fill(&mut self) -> std::io::Result<()> {
        self.compact();
        while !self.eof && self.buf.len() - self.pos < self.min_lookahead {
            let mut chunk = [0u8; 1 << 16];
            let n = self.reader.read(&mut chunk)?;
            if n == 0 {
                self.eof = true;
            } else {
                self.buf.extend_from_slice(&chunk[..n]);
            }
        }
        Ok(())
    }

    fn ensure(&mut self, n: usize) -> std::io::Result<()> {
        while !self.eof && self.buf.len() - self.pos < n {
            let mut chunk = [0u8; 1 << 16];
            let read = self.reader.read(&mut chunk)?;
            if read == 0 {
                self.eof = true;
            } else {
                self.buf.extend_from_slice(&chunk[..read]);
            }
        }
        Ok(())
    }

    fn compact(&mut self) {
        if self.pos > (1 << 20) && self.pos > self.buf.len() / 2 {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
    }

    fn at_eof(&self) -> bool {
        self.eof && self.pos >= self.buf.len()
    }

    fn peek(&self) -> &[u8] {
        &self.buf[self.pos..]
    }

    fn consume(&mut self, n: usize) {
        self.pos = (self.pos + n).min(self.buf.len());
    }

    fn next_byte(&mut self) -> Option<u8> {
        let b = self.buf.get(self.pos).copied();
        if b.is_some() {
            self.pos += 1;
        }
        b
    }

    /// Read a serialized string body of `len` logical bytes (SQL-unescaped
    /// when `sql_escape`), consuming it and the closing quote. Returns
    /// None (consuming nothing) if it isn't well-formed.
    fn read_serialized_string(
        &mut self,
        len: usize,
        header_len: usize,
        sql_escape: bool,
    ) -> std::io::Result<Option<Vec<u8>>> {
        // Worst case every byte is an escape pair.
        self.ensure(header_len + 2 * len + 2)?;
        match read_serialized_body(self.peek(), header_len, len, sql_escape) {
            Some((content, consumed)) => {
                self.consume(consumed);
                Ok(Some(content))
            }
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_serialized_headers() {
        assert_eq!(parse_serialized_header(b"s:5:\"hello", false), Some((5, 5)));
        assert_eq!(parse_serialized_header(b"s:123:\"x", false), Some((123, 7)));
        assert_eq!(
            parse_serialized_header(b"s:5:\\\"hello", true),
            Some((5, 6))
        );
        // Raw quotes are still accepted in SQL mode (they are valid SQL).
        assert_eq!(parse_serialized_header(b"s:5:\"hello", true), Some((5, 5)));
        assert_eq!(parse_serialized_header(b"i:5;", false), None);
        assert_eq!(parse_serialized_header(b"s::\"x", false), None);
        assert_eq!(parse_serialized_header(b"s:5:\\\"hello", false), None);
    }

    #[test]
    fn replaces_plain_text() {
        let r = Replacer::new(&[("https://a.com".into(), "https://b.localhost:8443".into())]);
        let out = r.replace(b"site: https://a.com/page", false);
        assert_eq!(out, b"site: https://b.localhost:8443/page");
    }

    #[test]
    fn fixes_serialized_lengths() {
        let r = Replacer::new(&[("https://a.com".into(), "https://b.localhost:8443".into())]);
        let input = br#"a:1:{s:3:"url";s:15:"https://a.com/x";}"#;
        let out = r.replace(input, false);
        let expected = format!(
            "a:1:{{s:3:\"url\";s:{}:\"https://b.localhost:8443/x\";}}",
            "https://b.localhost:8443/x".len()
        );
        assert_eq!(out, expected.as_bytes());
        assert!(validate_serialized(&out));
    }

    #[test]
    fn nested_serialization_round_trips() {
        let r = Replacer::new(&[("foo".into(), "longer-foo".into())]);
        let inner = "a:1:{i:0;s:3:\"foo\";}";
        let outer = format!("a:1:{{i:0;s:{}:\"{inner}\";}}", inner.len());
        let out = r.replace(outer.as_bytes(), false);
        assert!(
            validate_serialized(&out),
            "output should be valid serialized data: {}",
            String::from_utf8_lossy(&out)
        );
        let inner2 = "a:1:{i:0;s:10:\"longer-foo\";}";
        let expected = format!("a:1:{{i:0;s:{}:\"{inner2}\";}}", inner2.len());
        assert_eq!(out, expected.as_bytes());
    }

    #[test]
    fn multibyte_utf8_uses_byte_lengths() {
        let r = Replacer::new(&[("héllo".into(), "héllo wörld".into())]);
        let input = "s:10:\"xxhélloxx\";".as_bytes(); // héllo is 6 bytes -> content 10 bytes
        let out = r.replace(input, false);
        assert!(validate_serialized(&out));
        // "xxhéllo wörldxx" = 17 bytes
        assert!(String::from_utf8(out.clone()).unwrap().starts_with("s:17:"));
    }

    #[test]
    fn sql_mode_unescapes_and_reescapes() {
        let r = Replacer::new(&[("a.com".into(), "beispiel.de".into())]);
        // Dump form (as mysqldump writes it): \n, \' and the structural
        // quotes are escaped; the length counts unescaped bytes:
        // line(4) \n(1) a.com(5) '(1) s(1) = 12
        let dump = br#"s:12:\"line\na.com\'s\";"#;
        let out = r.replace(dump, true);
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("beispiel.de"), "{text}");
        assert!(text.contains("\\n"), "{text}");
        assert!(text.contains("\\'"), "{text}");
        // logical now: 4 + 1 + 11 + 1 + 1 = 18, structural quotes re-escaped
        assert_eq!(text, r#"s:18:\"line\nbeispiel.de\'s\";"#);
    }

    #[test]
    fn url_variants_cover_scheme_relative_and_json() {
        let pairs = Replacer::url_variants("https://prod.com", "https://dev.localhost:8443");
        let joined: Vec<String> = pairs.iter().map(|(f, _)| f.clone()).collect();
        assert!(joined.contains(&"https://prod.com".to_string()));
        assert!(joined.contains(&"http://prod.com".to_string()));
        assert!(joined.contains(&"//prod.com".to_string()));
        assert!(joined.contains(&"https:\\/\\/prod.com".to_string()));
    }

    #[test]
    fn stream_matches_slice() {
        let r = Replacer::new(&[("https://a.com".into(), "https://b.localhost:8443".into())]);
        let mut input = Vec::new();
        for _ in 0..1000 {
            input.extend_from_slice(br#"INSERT INTO t VALUES (1,'a:1:{s:3:"url";s:13:"https://a.com";}'),(2,'https://a.com/plain');"#);
            input.push(b'\n');
        }
        let sliced = r.replace(&input, true);
        let mut streamed = Vec::new();
        r.replace_stream(&input[..], &mut streamed, true).unwrap();
        assert_eq!(sliced, streamed);
    }

    #[test]
    fn malformed_serialized_strings_pass_through() {
        let r = Replacer::new(&[("foo".into(), "bar".into())]);
        let input = b"s:10:\"short\" and s:3:\"foo\" foo";
        let out = r.replace(input, false);
        // s:10:"short" is truncated (only 5 bytes before quote) -> left alone.
        // s:3:"foo" -> replaced with length fix. bare foo -> replaced.
        assert_eq!(out, b"s:10:\"short\" and s:3:\"bar\" bar".as_slice());
    }
}
