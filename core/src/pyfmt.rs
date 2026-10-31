//! Python-compatible formatting, so the Rust engine writes byte-identical configs to the
//! Python engine it replaces (verified by the parity tests).
use crate::json::Value;

/// Python 3 round(): halves go to the even neighbour.
pub fn round(x: f64) -> i64 {
    let r = x.round();
    if (x - x.trunc()).abs() == 0.5 {
        let t = x.trunc();
        return if (t as i64) % 2 == 0 { t as i64 } else { r as i64 };
    }
    r as i64
}

/// repr() of a Python float: "1.0", "1.25", "0.1".
pub fn float(x: f64) -> String {
    if x.is_finite() && x.fract() == 0.0 && x.abs() < 1e16 {
        format!("{x:.1}")
    } else {
        format!("{x}")
    }
}

/// A JSON number as Python would print it after json.load (ints stay ints).
pub fn num(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 { format!("{}", x as i64) } else { float(x) }
}

fn esc(s: &str, ascii: bool) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            '\u{08}' => o.push_str("\\b"),
            '\u{0c}' => o.push_str("\\f"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c if ascii && (c as u32) > 0x7e => {
                let mut b = [0u16; 2];
                for u in c.encode_utf16(&mut b) {
                    o.push_str(&format!("\\u{:04x}", u));
                }
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// json.dumps(v, indent=2, ensure_ascii=ascii)
pub fn dumps(v: &Value, ascii: bool) -> String {
    dumps_with(v, ascii, &|_| false)
}

/// json.dumps where the numbers at paths `floats` accepts were Python floats (print "1.0").
pub fn dumps_with(v: &Value, ascii: bool, floats: &dyn Fn(&[String]) -> bool) -> String {
    let mut o = String::new();
    dump_into(v, 0, ascii, &mut o, &mut vec![], floats);
    o
}

fn dump_into(v: &Value, lvl: usize, ascii: bool, o: &mut String, path: &mut Vec<String>, floats: &dyn Fn(&[String]) -> bool) {
    let pad = |n: usize| "  ".repeat(n);
    match v {
        Value::Null => o.push_str("null"),
        Value::Bool(b) => o.push_str(if *b { "true" } else { "false" }),
        Value::Num(n) => o.push_str(&if floats(path) { float(*n) } else { num(*n) }),
        Value::Str(s) => o.push_str(&esc(s, ascii)),
        Value::Arr(a) if a.is_empty() => o.push_str("[]"),
        Value::Obj(kv) if kv.is_empty() => o.push_str("{}"),
        Value::Arr(a) => {
            o.push_str("[\n");
            for (i, x) in a.iter().enumerate() {
                o.push_str(&pad(lvl + 1));
                path.push(i.to_string());
                dump_into(x, lvl + 1, ascii, o, path, floats);
                path.pop();
                o.push_str(if i + 1 < a.len() { ",\n" } else { "\n" });
            }
            o.push_str(&pad(lvl));
            o.push(']');
        }
        Value::Obj(kv) => {
            o.push_str("{\n");
            for (i, (k, x)) in kv.iter().enumerate() {
                o.push_str(&pad(lvl + 1));
                o.push_str(&esc(k, ascii));
                o.push_str(": ");
                path.push(k.clone());
                dump_into(x, lvl + 1, ascii, o, path, floats);
                path.pop();
                o.push_str(if i + 1 < kv.len() { ",\n" } else { "\n" });
            }
            o.push_str(&pad(lvl));
            o.push('}');
        }
    }
}

/// string.Template(t).safe_substitute(vals)
pub fn template(t: &str, vals: &[(&str, String)]) -> String {
    let b: Vec<char> = t.chars().collect();
    let mut o = String::with_capacity(t.len());
    let mut i = 0;
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    while i < b.len() {
        if b[i] != '$' {
            o.push(b[i]);
            i += 1;
            continue;
        }
        if i + 1 < b.len() && b[i + 1] == '$' {
            o.push('$');
            i += 2;
            continue;
        }
        let braced = i + 1 < b.len() && b[i + 1] == '{';
        let start = if braced { i + 2 } else { i + 1 };
        let mut j = start;
        if j < b.len() && (b[j].is_ascii_alphabetic() || b[j] == '_') {
            while j < b.len() && ident(b[j]) {
                j += 1;
            }
        }
        let name: String = b[start..j].iter().collect();
        let close_ok = !braced || (j < b.len() && b[j] == '}');
        match vals.iter().find(|(k, _)| *k == name) {
            Some((_, v)) if !name.is_empty() && close_ok => {
                o.push_str(v);
                i = if braced { j + 1 } else { j };
            }
            _ => {
                o.push('$');
                i += 1;
            }
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rounding_like_python() {
        assert_eq!((round(0.5), round(1.5), round(2.5), round(-0.5), round(2.6), round(13.75)), (0, 2, 2, 0, 3, 14));
        assert_eq!((float(1.0), float(1.25), float(0.1)), ("1.0".into(), "1.25".into(), "0.1".into()));
    }
    #[test]
    fn dumps_like_python() {
        let v = crate::json::parse(r#"{"a": [1, "x"], "b": {}, "c": {"d": true}, "e": "é ⏻"}"#).unwrap();
        assert_eq!(dumps(&v, false), "{\n  \"a\": [\n    1,\n    \"x\"\n  ],\n  \"b\": {},\n  \"c\": {\n    \"d\": true\n  },\n  \"e\": \"é ⏻\"\n}");
        assert_eq!(dumps(&Value::Str("é".into()), true), "\"\\u00e9\"");
    }
    #[test]
    fn template_like_python() {
        let v = [("font", "Mono".to_string()), ("pad", "9".to_string())];
        assert_eq!(template("a $font ${pad}px $$x $missing ${bad", &v), "a Mono 9px $x $missing ${bad");
    }
}
