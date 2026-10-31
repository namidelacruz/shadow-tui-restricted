//! shadow-niri-gen: print the default generated niri config (packaging check: niri validate).
fn main() {
    print!("{}", shadow_niri::config::generate(&shadow_niri::config::Gen::default(), ""));
}
