//! A strict parser for PHP serialized values. Used to prove that
//! replacement output still parses (round-trip property tests) and to
//! sanity-check user-facing operations.

/// True if `input` is exactly one well-formed PHP serialized value.
pub fn validate_serialized(input: &[u8]) -> bool {
    let mut pos = 0;
    parse_value(input, &mut pos) && pos == input.len()
}

fn parse_value(b: &[u8], pos: &mut usize) -> bool {
    match b.get(*pos) {
        Some(b'N') => expect(b, pos, b"N;"),
        Some(b'b') => expect(b, pos, b"b:") && parse_int_body(b, pos) && expect(b, pos, b";"),
        Some(b'i') => expect(b, pos, b"i:") && parse_int_body(b, pos) && expect(b, pos, b";"),
        Some(b'd') => {
            expect(b, pos, b"d:")
                && take_while(b, pos, |c| {
                    c.is_ascii_digit()
                        || matches!(c, b'-' | b'+' | b'.' | b'e' | b'E' | b'N' | b'F' | b'I')
                }) > 0
                && expect(b, pos, b";")
        }
        Some(b's') => parse_string(b, pos).is_some(),
        Some(b'a') => {
            if !expect(b, pos, b"a:") {
                return false;
            }
            let Some(count) = parse_uint_body(b, pos) else {
                return false;
            };
            if !expect(b, pos, b":{") {
                return false;
            }
            for _ in 0..count {
                if !parse_value(b, pos) || !parse_value(b, pos) {
                    return false;
                }
            }
            expect(b, pos, b"}")
        }
        Some(b'O') => {
            if !expect(b, pos, b"O:") {
                return false;
            }
            let Some(name_len) = parse_uint_body(b, pos) else {
                return false;
            };
            if !expect(b, pos, b":\"") {
                return false;
            }
            if !take(b, pos, name_len) || !expect(b, pos, b"\":") {
                return false;
            }
            let Some(count) = parse_uint_body(b, pos) else {
                return false;
            };
            if !expect(b, pos, b":{") {
                return false;
            }
            for _ in 0..count {
                if !parse_value(b, pos) || !parse_value(b, pos) {
                    return false;
                }
            }
            expect(b, pos, b"}")
        }
        Some(b'R') | Some(b'r') => {
            // References: R:1; r:2;
            *pos += 2;
            parse_int_body(b, pos) && expect(b, pos, b";")
        }
        _ => false,
    }
}

/// Parse `s:<len>:"<len bytes>";` — returns the content slice bounds.
fn parse_string(b: &[u8], pos: &mut usize) -> Option<()> {
    if !expect(b, pos, b"s:") {
        return None;
    }
    let len = parse_uint_body(b, pos)?;
    if !expect(b, pos, b":\"") {
        return None;
    }
    if !take(b, pos, len) {
        return None;
    }
    if !expect(b, pos, b"\";") {
        return None;
    }
    Some(())
}

fn expect(b: &[u8], pos: &mut usize, what: &[u8]) -> bool {
    if b.len() >= *pos + what.len() && &b[*pos..*pos + what.len()] == what {
        *pos += what.len();
        true
    } else {
        false
    }
}

fn take(b: &[u8], pos: &mut usize, n: usize) -> bool {
    if b.len() >= *pos + n {
        *pos += n;
        true
    } else {
        false
    }
}

fn take_while(b: &[u8], pos: &mut usize, f: impl Fn(u8) -> bool) -> usize {
    let start = *pos;
    while let Some(&c) = b.get(*pos) {
        if !f(c) {
            break;
        }
        *pos += 1;
    }
    *pos - start
}

fn parse_uint_body(b: &[u8], pos: &mut usize) -> Option<usize> {
    let start = *pos;
    let mut value: usize = 0;
    while let Some(&c) = b.get(*pos) {
        if !c.is_ascii_digit() {
            break;
        }
        value = value.checked_mul(10)?.checked_add((c - b'0') as usize)?;
        *pos += 1;
    }
    if *pos == start {
        return None;
    }
    Some(value)
}

fn parse_int_body(b: &[u8], pos: &mut usize) -> bool {
    if b.get(*pos) == Some(&b'-') || b.get(*pos) == Some(&b'+') {
        *pos += 1;
    }
    take_while(b, pos, |c| c.is_ascii_digit()) > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_core_types() {
        assert!(validate_serialized(b"N;"));
        assert!(validate_serialized(b"b:1;"));
        assert!(validate_serialized(b"i:-42;"));
        assert!(validate_serialized(b"d:3.14;"));
        assert!(validate_serialized(b"s:5:\"hello\";"));
        assert!(validate_serialized(b"s:0:\"\";"));
        assert!(validate_serialized(b"a:2:{i:0;s:1:\"a\";i:1;b:1;}"));
        assert!(validate_serialized(b"O:8:\"stdClass\":1:{s:1:\"x\";i:1;}"));
        assert!(validate_serialized(
            b"a:1:{s:1:\"k\";a:1:{s:1:\"n\";s:3:\"foo\";}}"
        ));
    }

    #[test]
    fn rejects_broken_values() {
        assert!(!validate_serialized(b"s:6:\"hello\";")); // wrong length
        assert!(!validate_serialized(b"s:5:\"hello\"; trailing"));
        assert!(!validate_serialized(b"a:2:{i:0;s:1:\"a\";}"));
        assert!(!validate_serialized(b""));
        assert!(!validate_serialized(b"x:1;"));
        assert!(!validate_serialized(b"s:5:\"hello\"")); // missing ;
    }
}
