//! dot//sakura — the Shadow Network launcher shell (`sakura launch`) and
//! menu (`sakura menu`, a dmenu-style picker used for the power menu and
//! clipboard history; replaces fuzzel entirely).
//!
//! Runs in a small floating shadow-term window. Type to fuzzy-search desktop apps
//! and Shadow Network actions; prefixes:  `>` actions only · `!cmd` run a shell
//! command · `!!cmd` run it in a terminal · `?` help.
//! Exit code 0 = launched or cancelled; anything else makes the launcher wrapper
//! retry in --safe mode (no settings/themes/history).
use crate::common::*;
use crate::json::Value;
use crate::term::{Input, Key, Style, Term};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Clone)]
struct Entry {
    id: String,
    name: String,
    detail: String,
    tag: &'static str,
    cmd: String,
    terminal: bool,
    keywords: String,
    cats: String,
}

fn data_dirs() -> Vec<PathBuf> {
    let mut v = vec![PathBuf::from(std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| home().join(".local/share").to_string_lossy().to_string()))];
    let sys = std::env::var("XDG_DATA_DIRS").unwrap_or_default();
    let sys = if sys.is_empty() { "/usr/local/share:/usr/share".to_string() } else { sys };
    v.extend(sys.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    v.push(PathBuf::from("/var/lib/flatpak/exports/share"));
    v.push(home().join(".local/share/flatpak/exports/share"));
    v
}

fn strip_field_codes(exec: &str) -> String {
    let mut out = String::new();
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            match chars.next() {
                Some('%') => out.push('%'),
                Some(_) | None => {}
            }
        } else {
            out.push(c);
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn parse_desktop(path: &PathBuf, id: &str) -> Option<Entry> {
    let text = fs::read_to_string(path).ok()?;
    let mut in_main = false;
    let mut kv: HashMap<String, String> = HashMap::new();
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_main = l == "[Desktop Entry]";
            continue;
        }
        if !in_main || l.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = l.split_once('=') {
            kv.entry(k.trim().to_string()).or_insert_with(|| v.trim().to_string());
        }
    }
    let is_true = |k: &str| kv.get(k).map(|v| v == "true").unwrap_or(false);
    if kv.get("Type").map(|t| t != "Application").unwrap_or(true) || is_true("NoDisplay") || is_true("Hidden") {
        return None;
    }
    let exec = strip_field_codes(kv.get("Exec")?);
    if exec.is_empty() {
        return None;
    }
    Some(Entry {
        id: format!("app:{id}"),
        name: kv.get("Name")?.clone(),
        detail: kv.get("GenericName").or_else(|| kv.get("Comment")).cloned().unwrap_or_default(),
        tag: "app",
        cmd: exec,
        terminal: is_true("Terminal"),
        keywords: kv.get("Keywords").cloned().unwrap_or_default(),
        cats: kv.get("Categories").cloned().unwrap_or_default(),
    })
}

fn load_apps() -> Vec<Entry> {
    let mut seen: HashMap<String, ()> = HashMap::new();
    let mut out = vec![];
    for d in data_dirs() {
        let dir = d.join("applications");
        let mut stack = vec![dir.clone()];
        while let Some(cur) = stack.pop() {
            let Ok(rd) = fs::read_dir(&cur) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                    continue;
                }
                if p.extension().map(|x| x != "desktop").unwrap_or(true) {
                    continue;
                }
                let id = p.strip_prefix(&dir).map(|r| r.to_string_lossy().replace('/', "-")).unwrap_or_default();
                if seen.contains_key(&id) {
                    continue; // earlier dirs (user) win
                }
                seen.insert(id.clone(), ());
                if let Some(en) = parse_desktop(&p, &id) {
                    out.push(en);
                }
            }
        }
    }
    out
}

fn actions(_settings: &Value) -> Vec<Entry> {
    let a = |id: &str, name: &str, detail: &str, cmd: &str| Entry {
        id: format!("nd:{id}"),
        name: name.into(),
        detail: detail.into(),
        tag: "shadow-network",
        cmd: cmd.into(),
        terminal: false,
        keywords: "shadow-network".into(),
        cats: "ShadowNetwork".into(),
    };
    let v = vec![
        a("settings", "Settings", "shadow-config: live config matrix", "__term__ sakura settings"),
        a("net", "Network", "shadow-net: Wi-Fi, campus setup, repair", "__term__ sakura net"),
        a("files", "Files", "shadow-files", "__term__ sakura files"),
        a("next", "Next window", "rotate the orbital ring", "shadowctl action focus-next"),
        a("full", "Fullscreen", "toggle the centre window", "shadowctl action fullscreen"),
        a("hud", "Toggle HUD", "", "shadowctl action hud-toggle"),
        a("lock", "Lock screen", "", "shadowctl lock"),
        a("power", "Power status", "shadow-powerd", "__term__ sh -c 'shadow-powerd status; echo; read -r -p \"press enter\" _'"),
        a("sync", "Sync state to GitHub", "shadow-sync push", "__term__ sh -c 'shadow-sync push; echo; read -r -p \"press enter\" _'"),
        a("healer", "Healer status", "self-correction log", "__term__ sh -c 'shadow-healer status; echo; read -r -p \"press enter\" _'"),
        a("revert", "Revert last automated fix", "shadow-healer", "shadowctl action healer-revert"),
        a("logout", "Log out", "leave the Shadow TUI session", "shadowctl action quit"),
        a("suspend", "Suspend", "", "systemctl suspend"),
        a("reboot", "Reboot", "", "systemctl reboot"),
        a("poweroff", "Shut down", "", "systemctl poweroff"),
    ];
    let _ = list_themes;
    v
}

/// Subsequence fuzzy score; higher is better. None = no match.
fn score(q: &str, hay: &str) -> Option<i32> {
    if q.is_empty() {
        return Some(0);
    }
    let h: Vec<char> = hay.to_lowercase().chars().collect();
    let mut s = 0;
    let mut hi = 0;
    let mut last: Option<usize> = None;
    for qc in q.to_lowercase().chars() {
        let mut found = false;
        while hi < h.len() {
            if h[hi] == qc {
                s += 10;
                if last.map(|l| l + 1 == hi).unwrap_or(false) {
                    s += 15; // consecutive
                }
                if hi == 0 || !h[hi - 1].is_alphanumeric() {
                    s += 20; // word start
                }
                last = Some(hi);
                hi += 1;
                found = true;
                break;
            }
            hi += 1;
        }
        if !found {
            return None;
        }
    }
    Some(s - (h.len() as i32 / 4))
}

fn history_path() -> PathBuf {
    state_dir().join("launcher.hist")
}

fn load_history() -> HashMap<String, i32> {
    fs::read_to_string(history_path())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('\t').and_then(|(k, v)| v.parse().ok().map(|n| (k.to_string(), n))))
        .collect()
}

fn hours_path() -> PathBuf {
    state_dir().join("launcher.hours")
}

fn now_hour() -> usize {
    crate::common::cmd_out(&["date", "+%H"]).1.trim().parse().unwrap_or(12)
}

/// Per-app launch counts by hour of day: "id\tc0,c1,…,c23"
fn load_hours() -> HashMap<String, [u32; 24]> {
    fs::read_to_string(hours_path())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (k, v) = l.split_once('\t')?;
            let mut a = [0u32; 24];
            for (i, x) in v.split(',').take(24).enumerate() {
                a[i] = x.parse().unwrap_or(0);
            }
            Some((k.to_string(), a))
        })
        .collect()
}

/// How much this app is used around this time of day (±1 hour), for ranking.
fn hour_score(h: &HashMap<String, [u32; 24]>, id: &str, hour: usize) -> i32 {
    h.get(id).map_or(0, |a| (a[(hour + 23) % 24] + 2 * a[hour] + a[(hour + 1) % 24]) as i32)
}

fn bump_hours(id: &str) {
    let mut h = load_hours();
    let e = h.entry(id.to_string()).or_insert([0; 24]);
    e[now_hour()] += 1;
    let text: String = h.iter().map(|(k, a)| format!("{k}\t{}\n", a.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(","))).collect();
    let _ = atomic_write(&hours_path(), &text);
}

fn bump_history(id: &str) {
    bump_hours(id);
    let mut h = load_history();
    *h.entry(id.to_string()).or_insert(0) += 1;
    let mut v: Vec<_> = h.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v.truncate(300);
    let text: String = v.iter().map(|(k, n)| format!("{k}\t{n}\n")).collect();
    let _ = atomic_write(&history_path(), &text);
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// `safe`: ignore settings, themes and history (used as an automatic retry
/// after a crash, so a bad config file can't take the launcher down).
pub fn run(safe: bool) -> i32 {
    let settings = if safe { Value::Obj(vec![]) } else { read_json(&settings_path()).unwrap_or(Value::Obj(vec![])) };
    let theme = if safe { Theme::fallback() } else { active_theme(&settings) };
    let ui = Ui::new(&theme, false);
    let terminal = settings.path_str(&["apps", "terminal"]).unwrap_or_else(|| "shadow-term".into());
    let mut entries = actions(&settings);
    entries.extend(load_apps());
    let hist = if safe { HashMap::new() } else { load_history() };
    let hours = if safe { HashMap::new() } else { load_hours() };
    let hour = now_hour();

    let mut t = match Term::enter(None) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("sakura: {e}");
            return 2;
        }
    };
    let mut input = Input::default();
    let mut sel = 0usize;
    let mut scroll = 0usize;
    let mut msg: Option<String> = None;
    loop {
        if t.resize() {
            t.force_redraw();
        }
        let q = input.buf.clone();
        let (mode, query) = if let Some(r) = q.strip_prefix("!!") {
            ("term", r.trim().to_string())
        } else if let Some(r) = q.strip_prefix('!') {
            ("shell", r.trim().to_string())
        } else if let Some(r) = q.strip_prefix('>') {
            ("actions", r.trim().to_string())
        } else if q.starts_with('?') {
            ("help", String::new())
        } else {
            ("all", q.trim().to_string())
        };
        let mut results: Vec<(i32, &Entry)> = if mode == "all" || mode == "actions" {
            entries
                .iter()
                .filter(|e| mode == "all" || e.tag == "shadow-network")
                .filter_map(|e| {
                    let hay = format!("{} {} {}", e.name, e.keywords, e.detail);
                    let base = score(&query, &e.name).map(|s| s + 30).or_else(|| score(&query, &hay))?;
                    Some((base + hist.get(&e.id).copied().unwrap_or(0).min(40) * 3 + hour_score(&hours, &e.id, hour).min(30) * 4, e))
                })
                .collect()
        } else {
            vec![]
        };
        results.sort_by(|a, b| b.0.cmp(&a.0).then((a.1.tag == "shadow-network").cmp(&(b.1.tag == "shadow-network"))).then(a.1.name.cmp(&b.1.name)));
        sel = sel.min(results.len().saturating_sub(1));

        // ---- draw
        let (w, h) = (t.w, t.h);
        t.clear(ui.base);
        let style_bar = Style::new(theme.c("bg"), theme.c("accent")).bold();
        t.fill(0, 0, w, 1, style_bar);
        let label = match (safe, theme.bar_style.as_str()) {
            (true, _) => " ❀ dot//sakura · SAFE MODE ",
            (_, "tactical") => " ❀ dot//sakura ▸ TACNET ",
            _ => " ❀ dot//sakura // GHOST.LINK ",
        };
        t.text(1, 0, label, style_bar, w);
        let tn = format!("{} ", theme.name);
        t.text(w.saturating_sub(tn.len() + 1), 0, &tn, Style::new(theme.c("bg"), theme.c("accent")), w);
        // prompt
        let prompt = match mode {
            "shell" => " $ ",
            "term" => " $$ ",
            "actions" => " ⚙ ",
            _ => " ❯ ",
        };
        t.text(1, 2, prompt, ui.accent2, 5);
        input.draw(&mut t, 1 + prompt.chars().count(), 2, w.saturating_sub(4 + prompt.chars().count()), Style::new(theme.c("fg"), theme.c("bg")), true, None);
        for x in 1..w - 1 {
            t.put(x, 3, ui.glyphs[4], Style::new(theme.c("inactive"), theme.c("bg")));
        }
        let list_y = 4;
        let list_h = h.saturating_sub(list_y + 2);
        if mode == "help" {
            let lines = [
                "type          search apps and Shadow TUI actions",
                ">text         Shadow TUI actions only",
                "!command      run a shell command",
                "!!command     run it inside a terminal",
                "↑ ↓ / Tab     move       Enter   launch",
                "Ctrl+U        clear      Esc     close",
            ];
            for (i, l) in lines.iter().enumerate() {
                t.text(3, list_y + i, l, if i % 2 == 0 { ui.base } else { ui.dim }, w - 4);
            }
        } else if mode == "shell" || mode == "term" {
            let what = if mode == "term" { format!("run in {terminal}:") } else { "run:".into() };
            t.text(3, list_y, &what, ui.dim, w - 4);
            t.text(3, list_y + 1, if query.is_empty() { "…" } else { &query }, ui.accent, w - 4);
        } else {
            if sel < scroll {
                scroll = sel;
            }
            if sel >= scroll + list_h {
                scroll = sel + 1 - list_h;
            }
            for (i, (_, e)) in results.iter().enumerate().skip(scroll).take(list_h) {
                let y = list_y + i - scroll;
                let selected = i == sel;
                let st = if selected { ui.sel } else { ui.base };
                t.fill(1, y, w - 2, 1, st);
                let tag_st = if selected { st } else if e.tag == "shadow-network" { ui.accent2 } else { ui.dim };
                t.text(2, y, if e.tag == "shadow-network" { "SN " } else { "   " }, tag_st, 3);
                let n = t.text(5, y, &e.name, st, w.saturating_sub(8));
                if !e.detail.is_empty() && 5 + n + 3 < w {
                    let dst = if selected { st } else { ui.dim };
                    t.text(5 + n + 2, y, &e.detail, Style { bold: false, ..dst }, w.saturating_sub(n + 10));
                }
            }
            if results.is_empty() {
                t.text(3, list_y, "no matches — try !command to run it directly, ? for help", ui.dim, w - 4);
            }
        }
        let foot = msg.clone().unwrap_or_else(|| format!("{} items · Enter launch · Esc close · ? help", results.len()));
        t.fill(0, h - 1, w, 1, Style::new(theme.c("fg_dim"), theme.c("bg_alt")));
        t.text(1, h - 1, &foot, Style::new(theme.c("fg_dim"), theme.c("bg_alt")), w - 2);
        let _ = t.present();

        // ---- input
        for k in t.keys() {
            msg = None;
            match k {
                Key::Esc | Key::Ctrl('c') | Key::Ctrl('q') => {
                    t.leave();
                    return 0;
                }
                Key::Up | Key::BackTab | Key::Ctrl('p') | Key::Ctrl('k') => sel = sel.saturating_sub(1),
                Key::Down | Key::Tab | Key::Ctrl('n') | Key::Ctrl('j') => sel = (sel + 1).min(results.len().saturating_sub(1)),
                Key::PageUp => sel = sel.saturating_sub(10),
                Key::PageDown => sel = (sel + 10).min(results.len().saturating_sub(1)),
                Key::Enter => {
                    let launch: Option<(String, String)> = match mode {
                        "shell" if !query.is_empty() => Some((format!("cmd:{query}"), query.clone())),
                        "term" if !query.is_empty() => Some((format!("cmd:{query}"), format!("{terminal} -e sh -c {}", sh_quote(&format!("{query}; exec $SHELL"))))),
                        "all" | "actions" => results.get(sel).map(|(_, e)| {
                            let cmd = if let Some(rest) = e.cmd.strip_prefix("__term__ ") {
                                format!("{terminal} -e {rest}")
                            } else if e.terminal {
                                format!("{terminal} -e sh -c {}", sh_quote(&e.cmd))
                            } else {
                                e.cmd.clone()
                            };
                            (e.id.clone(), cmd)
                        }),
                        _ => None,
                    };
                    if let Some((id, cmd)) = launch {
                        match spawn_detached(&cmd) {
                            Ok(()) => {
                                if !id.starts_with("cmd:") && !safe {
                                    bump_history(&id);
                                }
                                t.leave();
                                return 0;
                            }
                            Err(e) => msg = Some(format!("failed to start: {e}")),
                        }
                    }
                }
                other => {
                    if input.handle(other) {
                        sel = 0;
                        scroll = 0;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fuzzy() {
        assert!(score("ff", "Firefox").is_some());
        assert!(score("xyz", "Firefox").is_none());
        assert!(score("fire", "Firefox").unwrap() > score("fire", "Fancy irregular editor").unwrap_or(-999));
        assert_eq!(strip_field_codes("foo %U --bar %%x"), "foo --bar %x");
    }
}


/// `sakura menu --items FILE --result FILE [--prompt TEXT] [--title TEXT]`
/// Pick one line from FILE; writes it to the result file.
/// Exit 0 = chosen, 1 = cancelled, anything else = error.
pub fn menu(args: &[String]) -> i32 {
    let arg = |k: &str| args.windows(2).find(|w| w[0] == k).map(|w| w[1].clone());
    let (Some(items_p), Some(result_p)) = (arg("--items"), arg("--result")) else {
        eprintln!("usage: sakura menu --items FILE --result FILE [--prompt P] [--title T]");
        return 2;
    };
    let prompt = arg("--prompt").unwrap_or_else(|| "select".into());
    let title = arg("--title").unwrap_or_else(|| "menu".into());
    let items: Vec<String> = fs::read_to_string(&items_p)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(String::from)
        .collect();
    let settings = read_json(&settings_path()).unwrap_or(Value::Obj(vec![]));
    let theme = active_theme(&settings);
    let ui = Ui::new(&theme, false);
    let mut t = match Term::enter(None) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("sakura: {e}");
            return 2;
        }
    };
    let mut input = Input::default();
    let (mut sel, mut scroll) = (0usize, 0usize);
    loop {
        if t.resize() {
            t.force_redraw();
        }
        let q = input.buf.trim().to_string();
        let mut res: Vec<(i32, usize)> = items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| score(&q, it).map(|s| (s, i)))
            .collect();
        if !q.is_empty() {
            res.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        }
        sel = sel.min(res.len().saturating_sub(1));
        let (w, h) = (t.w, t.h);
        t.clear(ui.base);
        let bar = Style::new(theme.c("bg"), theme.c("accent")).bold();
        t.fill(0, 0, w, 1, bar);
        t.text(1, 0, &format!(" ❀ dot//sakura · {title} "), bar, w);
        t.text(1, 2, &format!(" {prompt} ❯ "), ui.accent2, w);
        let px = prompt.chars().count() + 5;
        input.draw(&mut t, px, 2, w.saturating_sub(px + 2), Style::new(theme.c("fg"), theme.c("bg")), true, None);
        for x in 1..w.saturating_sub(1) {
            t.put(x, 3, ui.glyphs[4], Style::new(theme.c("inactive"), theme.c("bg")));
        }
        let lh = h.saturating_sub(6);
        if sel < scroll {
            scroll = sel;
        }
        if sel >= scroll + lh {
            scroll = sel + 1 - lh;
        }
        for (row, (_, idx)) in res.iter().enumerate().skip(scroll).take(lh) {
            let y = 4 + row - scroll;
            let st = if row == sel { ui.sel } else { ui.base };
            t.fill(1, y, w.saturating_sub(2), 1, st);
            t.text(3, y, &items[*idx].replace('\t', "  "), st, w.saturating_sub(5));
        }
        if res.is_empty() {
            t.text(3, 4, "no matches", ui.dim, w);
        }
        t.fill(0, h - 1, w, 1, Style::new(theme.c("fg_dim"), theme.c("bg_alt")));
        t.text(1, h - 1, &format!("{} items · Enter choose · Esc cancel", res.len()), Style::new(theme.c("fg_dim"), theme.c("bg_alt")), w);
        let _ = t.present();
        for k in t.keys() {
            match k {
                Key::Esc | Key::Ctrl('c') => {
                    t.leave();
                    return 1;
                }
                Key::Up | Key::BackTab | Key::Ctrl('p') => sel = sel.saturating_sub(1),
                Key::Down | Key::Tab | Key::Ctrl('n') => sel = (sel + 1).min(res.len().saturating_sub(1)),
                Key::Enter => {
                    if let Some((_, idx)) = res.get(sel) {
                        let ok = fs::write(&result_p, &items[*idx]).is_ok();
                        t.leave();
                        return if ok { 0 } else { 2 };
                    }
                }
                other => {
                    if input.handle(other) {
                        sel = 0;
                        scroll = 0;
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------- drawer
const DRAWER_TABS: [&str; 11] = ["Suggested", "All", "Internet", "Office", "Development", "Graphics", "Media", "System", "Utilities", "Games", "Settings"];

fn drawer_tab(cats: &str) -> &'static str {
    let has = |c: &str| cats.split(';').any(|x| x == c);
    if has("ShadowNetwork") || has("Settings") || has("DesktopSettings") { "Settings" }
    else if has("WebBrowser") || has("Network") || has("Email") || has("Chat") { "Internet" }
    else if has("Office") { "Office" }
    else if has("Development") || has("IDE") { "Development" }
    else if has("Graphics") { "Graphics" }
    else if has("AudioVideo") || has("Audio") || has("Video") { "Media" }
    else if has("Game") { "Games" }
    else if has("System") || has("TerminalEmulator") || has("Monitor") { "System" }
    else { "Utilities" }
}

/// `sakura drawer` — application drawer: categorised tile grid, type to search,
/// "Suggested" learns what you open at this time of day.
pub fn drawer() -> i32 {
    let settings = read_json(&settings_path()).unwrap_or(Value::Obj(vec![]));
    let ui = Ui::new(&active_theme(&settings), false);
    let terminal = settings.path_str(&["apps", "terminal"]).unwrap_or_else(|| "shadow-term".into());
    let mut all = actions(&settings);
    all.extend(load_apps());
    all.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    let hist = load_history();
    let hours = load_hours();
    let hour = now_hour();
    let Ok(mut t) = Term::enter(None) else { return 1 };
    t.mouse(true);
    let mut tab = 0usize;
    let mut sel = 0usize;
    let mut scroll = 0usize;
    let mut q = Input::default();
    loop {
        if t.resize() {
            t.force_redraw();
        }
        let (w, h) = (t.w, t.h);
        let query = q.buf.to_lowercase();
        let mut items: Vec<&Entry> = all
            .iter()
            .filter(|e| {
                if !query.is_empty() {
                    return score(&query, &format!("{} {} {}", e.name, e.detail, e.keywords).to_lowercase()).is_some();
                }
                match DRAWER_TABS[tab] {
                    "All" => true,
                    "Suggested" => hour_score(&hours, &e.id, hour) > 0 || hist.get(&e.id).copied().unwrap_or(0) > 2,
                    t => drawer_tab(&e.cats) == t,
                }
            })
            .collect();
        if query.is_empty() && DRAWER_TABS[tab] == "Suggested" {
            items.sort_by_key(|e| -(hour_score(&hours, &e.id, hour) * 4 + hist.get(&e.id).copied().unwrap_or(0)));
            items.truncate(24);
        }
        let tw = 26usize;
        let cols = ((w.saturating_sub(4)) / tw).max(1);
        let rows_vis = (h.saturating_sub(7) / 3).max(1);
        sel = sel.min(items.len().saturating_sub(1));
        let srow = sel / cols;
        if srow < scroll {
            scroll = srow;
        }
        if srow >= scroll + rows_vis {
            scroll = srow + 1 - rows_vis;
        }
        // draw
        t.clear(ui.base);
        t.fill(0, 0, w, 1, ui.sel);
        t.text(0, 0, " ❀ dot//sakura ▸ APPLICATIONS", ui.sel, w);
        let mut x = 1;
        for (i, name) in DRAWER_TABS.iter().enumerate() {
            let st = if i == tab && query.is_empty() { ui.sel } else { ui.dim };
            let label = format!(" {name} ");
            t.text(x, 2, &label, st, w.saturating_sub(x));
            x += label.chars().count() + 1;
        }
        t.text(1, 4, "search ▸ ", ui.accent, 10);
        q.draw(&mut t, 10, 4, w.saturating_sub(12), ui.input, true, None);
        for (i, e) in items.iter().enumerate().skip(scroll * cols).take(rows_vis * cols) {
            let r = i / cols - scroll;
            let c = i % cols;
            let (x, y) = (2 + c * tw, 6 + r * 3);
            let selected = i == sel;
            let box_st = if selected { ui.sel } else { Style { bg: ui.theme.c("bg_alt"), ..ui.base } };
            t.fill(x, y, tw - 2, 2, box_st);
            t.text(x + 1, y, &e.name, if selected { ui.sel } else { Style { bg: ui.theme.c("bg_alt"), ..ui.accent2 } }, tw - 4);
            t.text(x + 1, y + 1, &e.detail, if selected { ui.sel } else { Style { bg: ui.theme.c("bg_alt"), ..ui.dim } }, tw - 4);
        }
        if items.is_empty() {
            t.text(2, 6, if DRAWER_TABS[tab] == "Suggested" { "nothing learned yet — apps you open often at this hour appear here" } else { "no apps" }, ui.dim, w);
        }
        t.text(0, h - 1, " ←↑↓→ move · Tab category · type to search · Enter open · Esc close", ui.dim, w);
        let _ = t.present();
        let mut launch: Option<&Entry> = None;
        for k in t.keys() {
            match k {
                Key::Esc | Key::Ctrl('c') => {
                    if q.buf.is_empty() {
                        t.leave();
                        return 0;
                    }
                    q.set("");
                }
                Key::Tab => { tab = (tab + 1) % DRAWER_TABS.len(); sel = 0; scroll = 0; q.set("") }
                Key::BackTab => { tab = (tab + DRAWER_TABS.len() - 1) % DRAWER_TABS.len(); sel = 0; scroll = 0; q.set("") }
                Key::Left => sel = sel.saturating_sub(1),
                Key::Right => sel = (sel + 1).min(items.len().saturating_sub(1)),
                Key::Up => sel = sel.saturating_sub(cols),
                Key::Down => sel = (sel + cols).min(items.len().saturating_sub(1)),
                Key::Enter => launch = items.get(sel).copied(),
                Key::Mouse(64, ..) => sel = sel.saturating_sub(cols),
                Key::Mouse(65, ..) => sel = (sel + cols).min(items.len().saturating_sub(1)),
                Key::Mouse(0, mx, my, true) => {
                    if my == 2 {
                        let mut x = 1;
                        for (i, name) in DRAWER_TABS.iter().enumerate() {
                            let l = name.chars().count() + 3;
                            if mx >= x && mx < x + l { tab = i; sel = 0; scroll = 0; q.set(""); }
                            x += l;
                        }
                    } else if my >= 6 && mx >= 2 {
                        let (c, r) = ((mx - 2) / tw, (my - 6) / 3);
                        let i = (r + scroll) * cols + c;
                        if c < cols && i < items.len() {
                            if i == sel { launch = items.get(i).copied() } else { sel = i }
                        }
                    }
                }
                other => {
                    if q.handle(other) {
                        sel = 0;
                        scroll = 0;
                    }
                }
            }
        }
        if let Some(e) = launch {
            let cmd = if let Some(rest) = e.cmd.strip_prefix("__term__ ") {
                format!("{terminal} -e {rest}")
            } else if e.terminal {
                format!("{terminal} -e sh -c {}", sh_quote(&e.cmd))
            } else {
                e.cmd.clone()
            };
            if spawn_detached(&cmd).is_ok() {
                bump_history(&e.id);
                t.leave();
                return 0;
            }
        }
    }
}
