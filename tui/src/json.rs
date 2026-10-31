//! Small JSON parser/serializer (std only). Objects keep key order so a
//! round-trip through the TUI leaves settings.json recognisable and keeps
//! keys the TUI doesn't know about.
use std::fmt::Write as _;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Value>),
    Obj(Vec<(String, Value)>),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(kv) => kv.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn path(&self, path: &[&str]) -> Option<&Value> {
        let mut cur = self;
        for p in path {
            cur = cur.get(p)?;
        }
        Some(cur)
    }
    pub fn str(&self) -> Option<&str> {
        if let Value::Str(s) = self { Some(s) } else { None }
    }
    pub fn num(&self) -> Option<f64> {
        if let Value::Num(n) = self { Some(*n) } else { None }
    }
    pub fn bool(&self) -> Option<bool> {
        if let Value::Bool(b) = self { Some(*b) } else { None }
    }
    pub fn path_str(&self, path: &[&str]) -> Option<String> {
        self.path(path).and_then(|v| v.str()).map(|s| s.to_string())
    }
    pub fn path_num(&self, path: &[&str]) -> Option<f64> {
        self.path(path).and_then(|v| v.num())
    }
    pub fn path_bool(&self, path: &[&str]) -> Option<bool> {
        self.path(path).and_then(|v| v.bool())
    }
    pub fn entries(&self) -> &[(String, Value)] {
        if let Value::Obj(kv) = self { kv } else { &[] }
    }

    /// Set a value at a path, creating intermediate objects.
    pub fn set(&mut self, path: &[&str], val: Value) {
        if path.is_empty() {
            *self = val;
            return;
        }
        if !matches!(self, Value::Obj(_)) {
            *self = Value::Obj(vec![]);
        }
        if let Value::Obj(kv) = self {
            let idx = match kv.iter().position(|(k, _)| k == path[0]) {
                Some(i) => i,
                None => {
                    kv.push((path[0].to_string(), Value::Null));
                    kv.len() - 1
                }
            };
            kv[idx].1.set(&path[1..], val);
        }
    }

    pub fn remove(&mut self, path: &[&str]) {
        if path.is_empty() {
            return;
        }
        if path.len() == 1 {
            if let Value::Obj(kv) = self {
                kv.retain(|(k, _)| k != path[0]);
            }
            return;
        }
        if let Value::Obj(kv) = self {
            if let Some((_, v)) = kv.iter_mut().find(|(k, _)| k == path[0]) {
                v.remove(&path[1..]);
            }
        }
    }

    pub fn pretty(&self) -> String {
        let mut s = String::new();
        write_val(self, 0, &mut s);
        s.push('\n');
        s
    }
}

fn write_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn write_val(v: &Value, ind: usize, out: &mut String) {
    let pad = |n: usize| "  ".repeat(n);
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Num(n) => {
            if n.fract() == 0.0 && n.abs() < 1e15 {
                let _ = write!(out, "{}", *n as i64);
            } else if n.is_finite() {
                let _ = write!(out, "{}", n);
            } else {
                out.push_str("null");
            }
        }
        Value::Str(s) => write_str(s, out),
        Value::Arr(a) => {
            if a.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (i, x) in a.iter().enumerate() {
                out.push_str(&pad(ind + 1));
                write_val(x, ind + 1, out);
                out.push_str(if i + 1 < a.len() { ",\n" } else { "\n" });
            }
            out.push_str(&pad(ind));
            out.push(']');
        }
        Value::Obj(kv) => {
            if kv.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            for (i, (k, x)) in kv.iter().enumerate() {
                out.push_str(&pad(ind + 1));
                write_str(k, out);
                out.push_str(": ");
                write_val(x, ind + 1, out);
                out.push_str(if i + 1 < kv.len() { ",\n" } else { "\n" });
            }
            out.push_str(&pad(ind));
            out.push('}');
        }
    }
}

pub fn parse(src: &str) -> Result<Value, String> {
    let mut p = P { b: src.as_bytes(), i: 0 };
    p.ws();
    let v = p.val(0)?;
    p.ws();
    if p.i != p.b.len() {
        return Err(format!("trailing data at byte {}", p.i));
    }
    Ok(v)
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> P<'a> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\n' | b'\r' | b'\t') {
            self.i += 1;
        }
    }
    fn err<T>(&self, m: &str) -> Result<T, String> {
        Err(format!("{} at byte {}", m, self.i))
    }
    fn lit(&mut self, s: &str, v: Value) -> Result<Value, String> {
        if self.b[self.i..].starts_with(s.as_bytes()) {
            self.i += s.len();
            Ok(v)
        } else {
            self.err("bad literal")
        }
    }
    fn val(&mut self, depth: usize) -> Result<Value, String> {
        if depth > 64 {
            return self.err("nesting too deep");
        }
        if self.i >= self.b.len() {
            return self.err("unexpected end");
        }
        match self.b[self.i] {
            b'n' => self.lit("null", Value::Null),
            b't' => self.lit("true", Value::Bool(true)),
            b'f' => self.lit("false", Value::Bool(false)),
            b'"' => Ok(Value::Str(self.string()?)),
            b'[' => {
                self.i += 1;
                let mut a = vec![];
                self.ws();
                if self.i < self.b.len() && self.b[self.i] == b']' {
                    self.i += 1;
                    return Ok(Value::Arr(a));
                }
                loop {
                    self.ws();
                    a.push(self.val(depth + 1)?);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Value::Arr(a));
                        }
                        _ => return self.err("expected , or ]"),
                    }
                }
            }
            b'{' => {
                self.i += 1;
                let mut kv = vec![];
                self.ws();
                if self.i < self.b.len() && self.b[self.i] == b'}' {
                    self.i += 1;
                    return Ok(Value::Obj(kv));
                }
                loop {
                    self.ws();
                    if self.b.get(self.i) != Some(&b'"') {
                        return self.err("expected key");
                    }
                    let k = self.string()?;
                    self.ws();
                    if self.b.get(self.i) != Some(&b':') {
                        return self.err("expected :");
                    }
                    self.i += 1;
                    self.ws();
                    let v = self.val(depth + 1)?;
                    if let Some(slot) = kv.iter_mut().find(|(kk, _): &&mut (String, Value)| *kk == k) {
                        slot.1 = v;
                    } else {
                        kv.push((k, v));
                    }
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Value::Obj(kv));
                        }
                        _ => return self.err("expected , or }"),
                    }
                }
            }
            b'-' | b'0'..=b'9' => {
                let s = self.i;
                while self.i < self.b.len() && matches!(self.b[self.i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                    self.i += 1;
                }
                std::str::from_utf8(&self.b[s..self.i])
                    .ok()
                    .and_then(|t| t.parse::<f64>().ok())
                    .map(Value::Num)
                    .ok_or_else(|| format!("bad number at byte {}", s))
            }
            _ => self.err("unexpected character"),
        }
    }
    fn hex4(&mut self) -> Result<u32, String> {
        if self.i + 4 > self.b.len() {
            return self.err("short \\u escape");
        }
        let s = std::str::from_utf8(&self.b[self.i..self.i + 4]).map_err(|e| e.to_string())?;
        self.i += 4;
        u32::from_str_radix(s, 16).map_err(|e| e.to_string())
    }
    fn string(&mut self) -> Result<String, String> {
        self.i += 1; // opening quote
        let mut out = String::new();
        loop {
            let start = self.i;
            while self.i < self.b.len() && self.b[self.i] != b'"' && self.b[self.i] != b'\\' {
                self.i += 1;
            }
            out.push_str(std::str::from_utf8(&self.b[start..self.i]).map_err(|e| e.to_string())?);
            match self.b.get(self.i) {
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.i += 1;
                    let c = *self.b.get(self.i).ok_or("unterminated escape")?;
                    self.i += 1;
                    match c {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let mut cp = self.hex4()?;
                            if (0xd800..0xdc00).contains(&cp) && self.b[self.i..].starts_with(b"\\u") {
                                self.i += 2;
                                let lo = self.hex4()?;
                                cp = 0x10000 + ((cp - 0xd800) << 10) + (lo.wrapping_sub(0xdc00) & 0x3ff);
                            }
                            out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                        }
                        _ => return self.err("bad escape"),
                    }
                }
                _ => return self.err("unterminated string"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip() {
        let src = r#"{"a": 1, "b": [true, null, "x\"yé😀"], "c": {"d": 1.25}}"#;
        let v = parse(src).unwrap();
        let again = parse(&v.pretty()).unwrap();
        assert_eq!(v, again);
        assert_eq!(v.path_num(&["c", "d"]), Some(1.25));
        assert_eq!(v.path(&["b"]).unwrap(), &Value::Arr(vec![Value::Bool(true), Value::Null, Value::Str("x\"yé😀".into())]));
    }
}
