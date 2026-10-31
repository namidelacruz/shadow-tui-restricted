//! The compositor's side of the adaptive layer: loads /var/lib/shadow-tui/modules/*.lua into the
//! sandbox and exposes a small capability API. Lua never touches compositor state directly: it
//! returns values or queues actions, and the Rust core decides.
//!
//!   shadow.rule(fn(win) -> {preset=1..3, requires_pointer=bool, fullscreen=bool})
//!   shadow.hud(name, fn() -> string)          -- shown in the top bar (20 ms budget)
//!   shadow.on("focus"|"spawn", fn(app_id, title))
//!   shadow.bind("Mod+X", fn())
//!   shadow.spawn(cmd)  shadow.action("focus-next")  shadow.notify(text)  shadow.log(text)
//!   shadow.state()  -> {ac=bool, battery=n, windows=n}
//!
//! Errors are appended to $XDG_RUNTIME_DIR/shadow/lua-errors.log (shadow-healer's trigger), and a
//! module that fails three times is switched off until the next reload.
use crate::state::State;
use shadow_lua::mlua::{Function, Lua, RegistryKey, Table, Value};
use shadow_lua::Sandbox;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

#[derive(Default)]
pub struct Rule {
    pub preset: Option<usize>,
    pub requires_pointer: Option<bool>,
    pub full: Option<bool>,
}

#[derive(Default)]
struct Reg {
    rules: Vec<(String, RegistryKey)>,
    widgets: Vec<(String, String, RegistryKey)>,
    events: Vec<(String, String, RegistryKey)>,
    binds: Vec<(String, String, RegistryKey)>,
    actions: Vec<String>,
    current: String,
    snapshot: (bool, i64, i64),
}

pub struct LuaHost {
    sb: Sandbox,
    reg: Rc<RefCell<Reg>>,
    errors: HashMap<String, u32>,
    loaded: Vec<String>,
}

pub fn modules_dir() -> PathBuf {
    std::env::var_os("SHADOW_MODULES").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/var/lib/shadow-tui/modules"))
}
fn error_log() -> PathBuf {
    PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into())).join("shadow/lua-errors.log")
}

impl LuaHost {
    pub fn new() -> Result<LuaHost, String> {
        let sb = Sandbox::new(4000).map_err(|e| e.to_string())?;
        let reg: Rc<RefCell<Reg>> = Default::default();
        install_api(&sb.lua, reg.clone()).map_err(|e| e.to_string())?;
        let mut h = LuaHost { sb, reg, errors: HashMap::new(), loaded: vec![] };
        let mut files: Vec<PathBuf> = std::fs::read_dir(modules_dir()).map(|r| r.flatten().map(|e| e.path()).filter(|p| p.extension().map_or(false, |e| e == "lua")).collect()).unwrap_or_default();
        files.sort();
        for f in files {
            let name = f.file_stem().unwrap().to_string_lossy().into_owned();
            let src = std::fs::read_to_string(&f).unwrap_or_default();
            h.reg.borrow_mut().current = name.clone();
            match h.sb.load_module(&name, &src) {
                Ok(()) => h.loaded.push(name),
                Err(e) => h.record(&name, &format!("load: {e}")),
            }
        }
        Ok(h)
    }

    pub fn modules(&self) -> Vec<String> {
        self.loaded.clone()
    }

    fn record(&mut self, module: &str, err: &str) {
        let n = self.errors.entry(module.to_string()).or_insert(0);
        *n += 1;
        let p = error_log();
        let _ = std::fs::create_dir_all(p.parent().unwrap());
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
            let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let _ = writeln!(f, "{ts} module={module} error={}", err.replace('\n', " | "));
        }
        tracing::warn!("lua module {module}: {err}");
    }
    fn enabled(&self, module: &str) -> bool {
        self.errors.get(module).copied().unwrap_or(0) < 3
    }

    fn call(&mut self, module: &str, key_idx: (&str, usize), ms: u64, args: impl FnOnce(&Lua) -> shadow_lua::mlua::Result<shadow_lua::mlua::MultiValue>) -> Option<Value> {
        if !self.enabled(module) {
            return None;
        }
        let reg = self.reg.clone();
        let r = self.sb.guarded(ms, |lua| {
            let r = reg.borrow();
            let key = match key_idx.0 {
                "rule" => &r.rules[key_idx.1].1,
                "widget" => &r.widgets[key_idx.1].2,
                "event" => &r.events[key_idx.1].2,
                _ => &r.binds[key_idx.1].2,
            };
            let f: Function = lua.registry_value(key)?;
            drop(r);
            let a = args(lua)?;
            f.call::<Value>(a)
        });
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                self.record(module, &e.to_string());
                None
            }
        }
    }

    pub fn set_snapshot(&self, ac: bool, battery: i64, windows: i64) {
        self.reg.borrow_mut().snapshot = (ac, battery, windows);
    }

    pub fn rule(&mut self, app_id: &str, title: &str) -> Rule {
        let mut out = Rule::default();
        let n = self.reg.borrow().rules.len();
        for i in 0..n {
            let m = self.reg.borrow().rules[i].0.clone();
            let (a, t) = (app_id.to_string(), title.to_string());
            let v = self.call(&m, ("rule", i), 20, move |lua| {
                let w = lua.create_table()?;
                w.set("app_id", a)?;
                w.set("title", t)?;
                Ok(shadow_lua::mlua::MultiValue::from_iter([Value::Table(w)]))
            });
            if let Some(Value::Table(t)) = v {
                if let Ok(p) = t.get::<i64>("preset") { out.preset = Some((p.max(1) - 1) as usize); }
                if let Ok(b) = t.get::<bool>("requires_pointer") { out.requires_pointer = Some(b); }
                if let Ok(b) = t.get::<bool>("fullscreen") { out.full = Some(b); }
            }
        }
        out
    }

    pub fn widgets(&mut self) -> Vec<String> {
        let n = self.reg.borrow().widgets.len();
        let mut v = vec![];
        for i in 0..n {
            let (m, name) = { let r = self.reg.borrow(); (r.widgets[i].0.clone(), r.widgets[i].1.clone()) };
            if let Some(Value::String(s)) = self.call(&m, ("widget", i), 20, |_| Ok(Default::default())) {
                let s: String = s.to_string_lossy().chars().filter(|c| !c.is_control()).take(40).collect();
                if !s.is_empty() {
                    v.push(format!("{} {s}", name.to_uppercase()));
                }
            }
        }
        v
    }

    pub fn event(&mut self, ev: &str, a: &str, b: &str) {
        let n = self.reg.borrow().events.len();
        for i in 0..n {
            let (m, e) = { let r = self.reg.borrow(); (r.events[i].0.clone(), r.events[i].1.clone()) };
            if e == ev {
                let (a, b) = (a.to_string(), b.to_string());
                self.call(&m, ("event", i), 20, move |lua| Ok(shadow_lua::mlua::MultiValue::from_iter([Value::String(lua.create_string(&a)?), Value::String(lua.create_string(&b)?)])));
            }
        }
    }

    pub fn binds(&self) -> Vec<(String, usize)> {
        self.reg.borrow().binds.iter().enumerate().map(|(i, (_, k, _))| (k.clone(), i)).collect()
    }
    pub fn call_bind(&mut self, i: usize) {
        let m = match self.reg.borrow().binds.get(i) { Some(b) => b.0.clone(), None => return };
        self.call(&m, ("bind", i), 50, |_| Ok(Default::default()));
    }
    pub fn take_actions(&mut self) -> Vec<String> {
        std::mem::take(&mut self.reg.borrow_mut().actions)
    }
    /// memory pressure: run a full collection
    pub fn gc(&self) {
        let _ = self.sb.lua.gc_collect();
    }
}

fn install_api(lua: &Lua, reg: Rc<RefCell<Reg>>) -> shadow_lua::mlua::Result<()> {
    let api: Table = lua.create_table()?;
    let r = reg.clone();
    api.set("rule", lua.create_function(move |lua, f: Function| {
        let k = lua.create_registry_value(f)?;
        let m = r.borrow().current.clone();
        r.borrow_mut().rules.push((m, k));
        Ok(())
    })?)?;
    let r = reg.clone();
    api.set("hud", lua.create_function(move |lua, (name, f): (String, Function)| {
        let k = lua.create_registry_value(f)?;
        let m = r.borrow().current.clone();
        r.borrow_mut().widgets.push((m, name, k));
        Ok(())
    })?)?;
    let r = reg.clone();
    api.set("on", lua.create_function(move |lua, (ev, f): (String, Function)| {
        let k = lua.create_registry_value(f)?;
        let m = r.borrow().current.clone();
        r.borrow_mut().events.push((m, ev, k));
        Ok(())
    })?)?;
    let r = reg.clone();
    api.set("bind", lua.create_function(move |lua, (key, f): (String, Function)| {
        let k = lua.create_registry_value(f)?;
        let m = r.borrow().current.clone();
        r.borrow_mut().binds.push((m, key, k));
        Ok(())
    })?)?;
    let r = reg.clone();
    api.set("spawn", lua.create_function(move |_, cmd: String| {
        r.borrow_mut().actions.push(format!("spawn {cmd}"));
        Ok(())
    })?)?;
    let r = reg.clone();
    api.set("action", lua.create_function(move |_, a: String| {
        // only layout actions; spawning goes through shadow.spawn
        if ["focus-next", "focus-prev", "width-up", "width-down", "fullscreen", "hud-toggle", "lock"].contains(&a.as_str()) {
            r.borrow_mut().actions.push(a);
        }
        Ok(())
    })?)?;
    let r = reg.clone();
    api.set("notify", lua.create_function(move |_, t: String| {
        r.borrow_mut().actions.push(format!("notify {}", t.chars().filter(|c| !c.is_control()).take(60).collect::<String>()));
        Ok(())
    })?)?;
    api.set("log", lua.create_function(|_, t: String| {
        tracing::info!("lua: {t}");
        Ok(())
    })?)?;
    let r = reg.clone();
    api.set("state", lua.create_function(move |lua, ()| {
        let (ac, bat, n) = r.borrow().snapshot;
        let t = lua.create_table()?;
        t.set("ac", ac)?;
        t.set("battery", bat)?;
        t.set("windows", n)?;
        Ok(t)
    })?)?;
    lua.globals().set("shadow", api)?;
    Ok(())
}

pub fn reload(st: &mut State) {
    st.lua = match LuaHost::new() {
        Ok(h) => Some(h),
        Err(e) => {
            tracing::error!("lua host: {e}");
            None
        }
    };
    st.binds = crate::input::compile_binds(st);
}

#[cfg(test)]
mod t {
    #[test]
    fn modules() {
        let d = std::env::temp_dir().join(format!("lmods-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("10-rules.lua"), "shadow.rule(function(w) if w.app_id == 'firefox' then return {preset = 3, requires_pointer = true} end end)\nshadow.hud('cpu', function() return '12%' end)\nshadow.bind('Mod+X', function() shadow.spawn('shadow-term') end)\n").unwrap();
        std::fs::write(d.join("20-evil.lua"), "os.execute('id')").unwrap();
        std::fs::write(d.join("30-crash.lua"), "shadow.hud('bad', function() error('boom') end)").unwrap();
        std::env::set_var("SHADOW_MODULES", &d);
        std::env::set_var("XDG_RUNTIME_DIR", &d);
        let mut h = super::LuaHost::new().unwrap();
        assert_eq!(h.modules(), vec!["10-rules", "30-crash"]); // the module calling os.execute was refused
        let r = h.rule("firefox", "x");
        assert_eq!((r.preset, r.requires_pointer), (Some(2), Some(true)));
        assert_eq!(h.widgets(), vec!["CPU 12%"]);
        for _ in 0..3 {
            h.widgets();
        }
        let log = std::fs::read_to_string(d.join("shadow/lua-errors.log")).unwrap();
        assert!(log.contains("module=20-evil") && log.contains("module=30-crash") && log.contains("boom"));
        assert_eq!(log.matches("module=30-crash").count(), 3); // switched off after three failures
        h.call_bind(0);
        assert_eq!(h.take_actions(), vec!["spawn shadow-term"]);
    }
}
