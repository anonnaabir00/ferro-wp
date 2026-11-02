//! Property tests: replacement must always produce valid serialized
//! data, no matter how nested or what (multibyte) strings are involved,
//! and streaming output must equal in-memory output.

use ferro_replace::{validate_serialized, Replacer};
use proptest::prelude::*;

/// A tiny PHP serializer for generating fixtures.
#[derive(Debug, Clone)]
enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    Array(Vec<(Value, Value)>),
}

impl Value {
    fn serialize(&self) -> Vec<u8> {
        match self {
            Value::Null => b"N;".to_vec(),
            Value::Bool(b) => format!("b:{};", *b as u8).into_bytes(),
            Value::Int(i) => format!("i:{i};").into_bytes(),
            Value::Str(s) => format!("s:{}:\"{}\";", s.len(), s).into_bytes(),
            Value::Array(entries) => {
                let mut out = format!("a:{}:{{", entries.len()).into_bytes();
                for (k, v) in entries {
                    out.extend(k.serialize());
                    out.extend(v.serialize());
                }
                out.push(b'}');
                out
            }
        }
    }
}

fn arb_string() -> impl Strategy<Value = String> {
    // Multibyte-heavy alphabet plus structural characters.
    prop::collection::vec(
        prop::sample::select(vec![
            'a', 'b', 'Z', '0', '/', ':', '.', '-', '_', '"', '\'', '\\', ';', '{', '}', '\n', 'é',
            'ü', '漢', '字', '🚀', 'ß',
        ]),
        0..40,
    )
    .prop_map(|chars| chars.into_iter().collect::<String>())
}

fn arb_value(depth: usize) -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(Value::Int),
        arb_string().prop_map(Value::Str),
    ];
    leaf.prop_recursive(depth as u32, 16, 8, |inner| {
        prop::collection::vec((inner.clone(), inner), 0..5).prop_map(Value::Array)
    })
}

const NEEDLE: &str = "https://prod.example.com";
const REPLACEMENT: &str = "https://mysite.localhost:8443";

fn value_with_needles(depth: usize) -> impl Strategy<Value = Value> {
    // Sprinkle the needle into otherwise random values.
    (arb_value(depth), prop::collection::vec(0usize..3, 1..4)).prop_map(|(mut v, _)| {
        inject(&mut v);
        v
    })
}

fn inject(v: &mut Value) {
    match v {
        Value::Str(s) => {
            *s = format!("{s}{NEEDLE}/uploads/x.jpg");
        }
        Value::Array(entries) => {
            for (k, val) in entries.iter_mut() {
                inject(k);
                inject(val);
            }
        }
        _ => {}
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn replace_keeps_serialized_data_valid(value in value_with_needles(4)) {
        let input = value.serialize();
        let r = Replacer::new(&[(NEEDLE.into(), REPLACEMENT.into())]);
        let out = r.replace(&input, false);
        prop_assert!(
            validate_serialized(&out),
            "invalid serialized output: {}",
            String::from_utf8_lossy(&out)
        );
        prop_assert!(!out.windows(NEEDLE.len()).any(|w| w == NEEDLE.as_bytes()));
    }

    #[test]
    fn replace_is_round_trippable(value in value_with_needles(3)) {
        let input = value.serialize();
        let forward = Replacer::new(&[(NEEDLE.into(), REPLACEMENT.into())]);
        let backward = Replacer::new(&[(REPLACEMENT.into(), NEEDLE.into())]);
        let out = forward.replace(&input, false);
        let back = backward.replace(&out, false);
        prop_assert_eq!(back, input);
    }

    #[test]
    fn deeply_nested_values_survive(value in value_with_needles(6)) {
        let input = value.serialize();
        let r = Replacer::new(&[(NEEDLE.into(), REPLACEMENT.into())]);
        let out = r.replace(&input, false);
        prop_assert!(validate_serialized(&out));
    }

    #[test]
    fn stream_equals_slice(value in value_with_needles(4)) {
        let mut input = value.serialize();
        // Pad with plain dump-ish text containing needles.
        input.extend_from_slice(format!(",'plain {NEEDLE} text'").as_bytes());
        let r = Replacer::new(&[(NEEDLE.into(), REPLACEMENT.into())]);
        let sliced = r.replace(&input, false);
        let mut streamed = Vec::new();
        r.replace_stream(&input[..], &mut streamed, false).unwrap();
        prop_assert_eq!(sliced, streamed);
    }

    #[test]
    fn sql_mode_round_trip(content in arb_string(), needle in prop::sample::select(vec!["a.com", "x.org"])) {
        // Build a dump-escaped serialized string containing the needle,
        // escaped the way mysqldump escapes string literals.
        let logical = format!("{content}{needle}");
        let escaped = logical
            .replace('\\', "\\\\")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\0', "\\0")
            .replace('\u{1a}', "\\Z")
            .replace('\'', "\\'")
            .replace('"', "\\\"");
        let dump = format!("s:{}:\\\"{escaped}\\\";", logical.len());
        let r = Replacer::new(&[(needle.into(), "replaced.tld".into())]);
        let out = r.replace(dump.as_bytes(), true);
        prop_assert!(String::from_utf8_lossy(&out).contains("replaced.tld"));
        // Length header must match the logical (unescaped) content length.
        let new_logical_len = format!("{content}replaced.tld").len();
        prop_assert!(
            String::from_utf8_lossy(&out).starts_with(&format!("s:{new_logical_len}:\\\"")),
            "wrong length header: {}",
            String::from_utf8_lossy(&out)
        );
        // And replacing back must restore the original dump byte-for-byte.
        let back = Replacer::new(&[("replaced.tld".into(), needle.into())]).replace(&out, true);
        prop_assert_eq!(back, dump.into_bytes());
    }
}
