//! shadow — Shadow TUI Restricted system tools. One binary, several names:
//!   shadow-sync    state + GitHub synchronisation        shadow-netd   Wi-Fi self-repair daemon
//!   shadow-healer  Lua module self-correction            shadownet     compatibility name
#[allow(dead_code)]
mod json;
#[path = "../../powerd/src/sys.rs"]
#[allow(dead_code)]
mod sys;
mod ai;
#[allow(dead_code)]
mod distro;
mod greeter_admin;
mod healer;
mod http;
#[allow(dead_code)]
mod pyfmt;
mod rx;
mod setup;
mod st;
mod sync;
mod policy;
mod updater;
mod wifi;

const HELP: &str = "\
shadow — Shadow TUI Restricted system tools

  setup [--remove [--purge]]          per-user first run / removal (migrates from Shadow Network)
  greeter enable|disable|status|preview|warning …   login screen (greetd)
  wifi status|scan|connect|campus|repair|forget|device|fix-device|log|iwd-provision …
  netd [--once]                       Wi-Fi self-repair daemon (shadow-netd.service, root)
  sync init|push|status|bootstrap|restore|auto …     state repo on GitHub (shadow-sync)
  healer status|list|show|approve|reject|revert|check …   Lua self-correction (shadow-healer)
  ai status|setup|ask|collect|issues|propose|review|approve|reject   local model + approved code fixes
  update check|run|rollback|status|set-repo URL [BRANCH]

Also: shadow-bar, shadow-launch, shadow-net, shadow-config, shadow-files, shadow-lock, shadow-greet,
      shadow-sessiond, shadow-displayd, shadow-powerd, shadow-term, shadow-sec (optional).
";

fn out(r: Result<String, String>) -> i32 {
    match r {
        Ok(s) => { println!("{s}"); 0 }
        Err(e) => { eprintln!("shadow: {e}"); 1 }
    }
}

fn main() {
    let argv0 = std::env::args().next().unwrap_or_default();
    let base = std::path::Path::new(&argv0).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let p = st::paths();
    let code = match base.as_str() {
        "shadow-sync" => sync::main(&a),
        "shadow-healer" => healer::main(&a),
        "shadow-netd" => wifi::netd_main(&a),
        _ => match a.as_slice() {
            ["setup", rest @ ..] => setup::main(rest),
            ["greeter", rest @ ..] => greeter_admin::main(&p, rest),
            ["wifi", rest @ ..] => wifi::main(rest),
            ["netd", rest @ ..] => wifi::netd_main(rest),
            ["sync", rest @ ..] | ["backup", rest @ ..] => sync::main(rest),
            ["healer", rest @ ..] => healer::main(rest),
            ["update", rest @ ..] => updater::main(&p, rest),
            ["ai"] | ["ai", "status"] => { println!("{}", ai::status()); 0 }
            ["ai", "setup", rest @ ..] => out(ai::setup(rest)),
            ["ai", "ask", q @ ..] if !q.is_empty() => out(ai::ask(&q.join(" "))),
            ["ai", "collect"] => out(ai::collect()),
            ["ai", "issues"] => { for (id, last) in ai::list_issues() { println!("{id}  {last}"); } 0 }
            ["ai", "propose", id] => out(ai::propose(id)),
            ["ai", "proposals"] => { for (n, s) in ai::list_proposals() { println!("{n}  {s}"); } 0 }
            ["ai", "review", n] => out(ai::review(n)),
            ["ai", "approve", n] => out(ai::approve(n, None)),
            ["ai", "reject", n] => out(ai::reject(n)),
            ["--version"] | ["version"] => { println!("shadow {}", env!("CARGO_PKG_VERSION")); 0 }
            ["-h"] | ["--help"] | ["help"] | [] => { print!("{HELP}"); 0 }
            _ => { eprint!("{HELP}"); 2 }
        },
    };
    std::process::exit(code);
}
