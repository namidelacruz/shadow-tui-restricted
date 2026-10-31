//! Line-preserving edits of ~/.config/shadow-state/config.toml (comments and layout survive).
/// value of [section] key (strings unquoted)
pub fn get_in(text: &str, section: &str, key: &str) -> Option<String> {
    let mut cur = String::new();
    for l in text.lines() {
        let t = l.trim();
        if t.starts_with('[') && t.ends_with(']') {
            cur = t.trim_matches(|c| c == '[' || c == ']').trim().to_string();
            continue;
        }
        if cur != section || t.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = t.split_once('=') {
            if k.trim().trim_matches('"') == key {
                let v = v.split(" #").next().unwrap_or("").trim();
                return Some(if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') { v[1..v.len() - 1].to_string() } else { v.to_string() });
            }
        }
    }
    None
}

/// set [section] key = literal, keeping everything else; a commented default line is replaced
pub fn set_in(text: &str, section: &str, key: &str, literal: &str) -> String {
    let mut out: Vec<String> = vec![];
    let (mut cur, mut done, mut seen) = (String::new(), false, false);
    for l in text.lines() {
        let t = l.trim();
        if t.starts_with('[') && t.ends_with(']') {
            if cur == section && !done {
                while out.last().map_or(false, |x: &String| x.trim().is_empty()) {
                    out.pop();
                }
                out.push(format!("{key} = {literal}"));
                out.push(String::new());
                done = true;
            }
            cur = t.trim_matches(|c| c == '[' || c == ']').trim().to_string();
            seen |= cur == section;
        } else if cur == section && !done {
            let body = t.trim_start_matches('#').trim();
            if let Some((k, _)) = body.split_once('=') {
                if k.trim().trim_matches('"') == key {
                    out.push(format!("{key} = {literal}"));
                    done = true;
                    continue;
                }
            }
        }
        out.push(l.to_string());
    }
    if !done {
        if !seen {
            out.push(String::new());
            out.push(format!("[{section}]"));
        }
        out.push(format!("{key} = {literal}"));
    }
    out.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_keeps_comments() {
        let t = "# mine\n[look]\n# amber = \"#FFB000\"\nfont_pt = 11\n\n[bar]\nwidth = 230\n";
        let u = set_in(t, "look", "amber", "\"#FFC000\"");
        assert!(u.contains("# mine") && u.contains("amber = \"#FFC000\""));
        assert_eq!(get_in(&u, "look", "amber").as_deref(), Some("#FFC000"));
        let v = set_in(&u, "display.scale", "\"eDP-1\"", "1.5");
        assert_eq!(get_in(&v, "display.scale", "eDP-1").as_deref(), Some("1.5"));
        assert_eq!(get_in(&v, "bar", "width").as_deref(), Some("230"));
    }
}

/// remove [section] key (no-op if absent)
pub fn remove_in(text: &str, section: &str, key: &str) -> String {
    let mut cur = String::new();
    let mut out: Vec<&str> = vec![];
    for l in text.lines() {
        let t = l.trim();
        if t.starts_with('[') && t.ends_with(']') {
            cur = t.trim_matches(|c| c == '[' || c == ']').trim().to_string();
        } else if cur == section && !t.starts_with('#') {
            if let Some((k, _)) = t.split_once('=') {
                if k.trim().trim_matches('"') == key {
                    continue;
                }
            }
        }
        out.push(l);
    }
    out.join("\n") + "\n"
}
