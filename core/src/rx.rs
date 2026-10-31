//! Tiny case-insensitive backtracking regex (std-only) — exactly the subset the Wi-Fi failure
//! classifier uses: literals, `.`, `[..]`/`[^..]`, `\w \d \s \b`, `( | )`, `* + ?`.
//! Like Python's `re`, `.` does not match a newline.

#[derive(Debug, Clone)]
enum Node {
    Lit(char),
    Any,
    Class(Vec<(char, char)>, bool),
    Word,
    Digit,
    Space,
    Bound,
    Group(Vec<Vec<Item>>),
}

#[derive(Debug, Clone)]
struct Item {
    n: Node,
    min: u32,
    max: u32,
}

pub struct Rx(Vec<Vec<Item>>);

fn lc(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

fn parse_alts(p: &[char], i: &mut usize) -> Vec<Vec<Item>> {
    let mut alts = vec![vec![]];
    while *i < p.len() {
        let c = p[*i];
        *i += 1;
        let n = match c {
            ')' => return alts,
            '|' => {
                alts.push(vec![]);
                continue;
            }
            '(' => {
                if p.get(*i) == Some(&'?') && p.get(*i + 1) == Some(&':') {
                    *i += 2;
                }
                Node::Group(parse_alts(p, i))
            }
            '.' => Node::Any,
            '[' => {
                let neg = p.get(*i) == Some(&'^');
                if neg {
                    *i += 1;
                }
                let mut set = vec![];
                let mut first = true;
                while *i < p.len() && (p[*i] != ']' || first) {
                    first = false;
                    let mut a = p[*i];
                    *i += 1;
                    if a == '\\' && *i < p.len() {
                        a = p[*i];
                        *i += 1;
                    }
                    if p.get(*i) == Some(&'-') && p.get(*i + 1).map_or(false, |c| *c != ']') {
                        let b = p[*i + 1];
                        *i += 2;
                        set.push((lc(a), lc(b)));
                    } else {
                        set.push((lc(a), lc(a)));
                    }
                }
                *i += 1;
                Node::Class(set, neg)
            }
            '\\' => {
                let e = p.get(*i).copied().unwrap_or('\\');
                *i += 1;
                match e {
                    'w' => Node::Word,
                    'd' => Node::Digit,
                    's' => Node::Space,
                    'b' => Node::Bound,
                    o => Node::Lit(lc(o)),
                }
            }
            o => Node::Lit(lc(o)),
        };
        let (min, max) = match p.get(*i) {
            Some('*') => (0, u32::MAX),
            Some('+') => (1, u32::MAX),
            Some('?') => (0, 1),
            _ => (1, 1),
        };
        if (min, max) != (1, 1) {
            *i += 1;
        }
        alts.last_mut().unwrap().push(Item { n, min, max });
    }
    alts
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn one(n: &Node, t: &[char], i: usize, k: &mut dyn FnMut(usize) -> bool) -> bool {
    match n {
        Node::Group(alts) => alts.iter().any(|a| seq(a, t, i, k)),
        Node::Bound => {
            let a = i > 0 && is_word(t[i - 1]);
            let b = i < t.len() && is_word(t[i]);
            a != b && k(i)
        }
        _ => {
            let Some(&c) = t.get(i) else { return false };
            let ok = match n {
                Node::Lit(l) => c == *l,
                Node::Any => c != '\n',
                Node::Class(set, neg) => set.iter().any(|(a, b)| *a <= c && c <= *b) != *neg,
                Node::Word => is_word(c),
                Node::Digit => c.is_ascii_digit(),
                Node::Space => c.is_whitespace(),
                _ => false,
            };
            ok && k(i + 1)
        }
    }
}

fn rep(it: &Item, rest: &[Item], t: &[char], i: usize, count: u32, k: &mut dyn FnMut(usize) -> bool) -> bool {
    if count < it.max {
        let mut next = |j: usize| j != i && rep(it, rest, t, j, count + 1, k);
        if one(&it.n, t, i, &mut next) {
            return true;
        }
    }
    count >= it.min && seq(rest, t, i, k)
}

fn seq(s: &[Item], t: &[char], i: usize, k: &mut dyn FnMut(usize) -> bool) -> bool {
    match s.split_first() {
        None => k(i),
        Some((it, rest)) => {
            if (it.min, it.max) == (1, 1) {
                one(&it.n, t, i, &mut |j| seq(rest, t, j, k))
            } else {
                rep(it, rest, t, i, 0, k)
            }
        }
    }
}

impl Rx {
    pub fn new(pat: &str) -> Rx {
        let p: Vec<char> = pat.chars().collect();
        let mut i = 0;
        Rx(parse_alts(&p, &mut i))
    }
    /// `re.search(pat, text, re.I)`; pass text already lower-cased via `prep`.
    pub fn search(&self, t: &[char]) -> bool {
        (0..=t.len()).any(|s| self.0.iter().any(|a| seq(a, t, s, &mut |_| true)))
    }
}

pub fn prep(text: &str) -> Vec<char> {
    text.chars().map(lc).collect()
}

#[allow(dead_code)]
pub fn search_i(pat: &str, text: &str) -> bool {
    Rx::new(pat).search(&prep(text))
}

#[cfg(test)]
mod t {
    use super::search_i as s;
    #[test]
    fn subset() {
        assert!(s(r"device .*not ready", "Error: Device wlan0 is not ready"));
        assert!(!s(r"device .*not ready", "device wlan0\nnot ready"));
        assert!(s(r"[Ww]i-?Fi (is )?disabled", "wifi disabled"));
        assert!(s(r"reason=(2|15|23)\b", "x reason=15 y"));
        assert!(!s(r"reason=(2|15|23)\b", "reason=150"));
        assert!(s(r"Device '[^']+' not found", "Error: Device 'wlp2s0' not found."));
        assert!(s(r"DHCP.*time(d)? ?out", "dhcp4 timed out"));
        assert!(s(r"certificate verif\w* failed", "certificate verification failed"));
        assert!(s(r"802\.1X supplicant failed", "802.1x supplicant failed"));
        assert!(!s(r"802\.1X supplicant failed", "802a1X supplicant failed"));
        assert!(s(r"E=691", "e=691 r=1"));
        assert!(s(r"self.signed certificate", "self-signed certificate in chain"));
    }
}
