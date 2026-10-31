//! sakura — Shadow TUI Restricted's text apps, std-only (no crates). Installed under several
//! names: shadow-config (settings), shadow-net, shadow-files, shadow-launch.
//!
//!   sakura launch [--safe]          launcher shell (run inside a terminal window)
//!   sakura menu --items F --result F [--prompt P] [--title T]
//!                                   dmenu-style picker (power menu, clipboard)
//!   sakura settings [page]          keyboard settings editor
//!   sakura greeter [--demo] [--theme ID] [--self-test]
//!                                   greetd login screen
mod common;
mod files;
mod greeter;
#[allow(dead_code)]
mod json;
mod launcher;
mod net;
mod settings;
mod term;

const HELP: &str = "sakura — Shadow TUI Restricted text apps (std-only Rust)

USAGE:
  sakura launch [--safe]
  sakura menu --items FILE --result FILE [--prompt TEXT] [--title TEXT]
  sakura settings [PAGE]           (= shadow-config) live config.toml matrix
  sakura greeter [--demo] [--theme ID] [--self-test]
  sakura files [DIR] [--print-cwd]
  sakura net
  sakura drawer
  sakura --version
";

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // shadow-config / shadow-net / shadow-files / shadow-launch are names of this binary
    let argv0 = std::env::args().next().unwrap_or_default();
    let base = argv0.rsplit('/').next().unwrap_or("");
    let alias = match base { "shadow-config" => Some("settings"), "shadow-net" => Some("net"), "shadow-files" => Some("files"), "shadow-launch" => Some("launch"), _ => None };
    if let Some(a) = alias {
        args.insert(0, a.to_string());
    }
    let code = match args.first().map(String::as_str) {
        Some("settings") => settings::run(args.get(1).map(String::as_str)),
        Some("launch") | Some("launcher") => launcher::run(args.iter().any(|a| a == "--safe")),
        Some("menu") => launcher::menu(&args[1..]),
        Some("greeter") => greeter::run(&args[1..]),
        Some("files") => files::run(&args[1..]),
        Some("drawer") => launcher::drawer(),
        Some("net") | Some("network") => net::run(&args[1..]),
        Some("--version") | Some("-V") => {
            println!("dot//sakura {}", option_env!("SHADOWNET_VERSION").unwrap_or("2.0.0"));
            0
        }
        _ => {
            print!("{HELP}");
            if args.is_empty() { 0 } else { 2 }
        }
    };
    std::process::exit(code);
}
