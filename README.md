# Shadow TUI Restricted

A keyboard-first tactical desktop for Arch Linux, built to the clean-room specification CR-SPEC-SHADOW-001 and tuned for the Dell 14 Plus (2560×1600 panel, Lunar Lake, Intel NPU).

It has its own Wayland compositor, written from scratch on Smithay, with no niri code. Windows live on an orbital ring around a centre stage, and a bezel HUD shows the machine's state. The login screen is drawn by the same compositor, so it blends into the session. Around that sit a power daemon, a Wi-Fi self-repair daemon, a sandboxed self-correcting Lua layer and GitHub state sync. The security subsystem is a separate, optional package.

`docs/SPEC-TRACE.md` lists, requirement by requirement, what is implemented as specified, what is implemented differently and what is not implemented.

## Components

| Component | What it is |
|---|---|
| `shadow-compositor` | The display server: Smithay + DRM/KMS + libinput (`--winit` runs nested). Orbital ring layout, tactical GLSL shaders (floor grid, obsidian glass plate, wireframe nodes, globe), bezel HUD, keyboard-first input with pointer masking, damage-only rendering (0 FPS when idle), lock screen (PAM), greeter mode (greetd), LuaJIT behaviour modules, control socket. |
| `shadowctl` | Control and inspection: `state`, `action focus-next`, `spawn`, `lock`, `refresh 60`, `config-reload`, `lua-reload`. |
| `shadow-term` | The terminal (amber/cyan, HUD strip, command blocks). |
| `sakura` | The text apps: `shadow-config` (live settings matrix), `shadow-net`, `shadow-files`, `shadow-launch`, plus the drawer and the text greeter fallback. |
| `shadow-powerd` | Power daemon (root): platform profile, EPP, RAPL PL1/PL2, charge limits, kernel guard, predictive tuning from compositor telemetry, the 20 % memory ceiling. As `shadow-audiod`: Lunar Lake audio initialiser. |
| `shadow` | System tools. As `shadow-netd`: Wi-Fi self-repair daemon. As `shadow-sync`: state + GitHub sync. As `shadow-healer`: Lua self-correction. Also `setup`, `greeter`, `wifi`, `ai`, `update`. |
| `shadow-sec` | Optional package `shadow-tui-sec`. Security modules you enable one at a time. |

## Install

```bash
git clone <this repo> && cd shadow-tui-restricted
./install.sh                    # builds the split package, installs it to /opt/shadow-tui, sets up your account
./install.sh --greeter --sec    # also the login screen and the optional security package
./install.sh --bootstrap git@github.com:you/shadow-state.git   # new machine: restore your state too
```

After that, pick **Shadow TUI Restricted** at the login screen, or run `shadow-session` from a TTY.

**Upgrading from Shadow Network or NETDIVE.** The package replaces both. The niri fork, the Python engine and the GTK app are removed, and the old services are disabled. `shadow setup` moves configs that the old engine generated (niri, waybar, mako, swaylock, foot, yazi, fuzzel) into `~/.local/state/shadow-tui/retired-*` and keeps your update source, terminal and AI settings.

## Keys

| Keys | Action |
|---|---|
| `Mod+Tab` / `Mod+J` · `Mod+Shift+Tab` / `Mod+K` | rotate the next / previous ring window into the centre (120 ms) |
| `Mod+[` / `Mod+]` | centre width preset 45 / 65 / 85 % |
| `Mod+F` | fullscreen between the HUD bars; again restores the preset |
| `Mod+Return` `Mod+D` `Mod+A` `Mod+E` `Mod+N` | terminal, launcher, drawer, files, network |
| `Mod+Shift+,` | shadow-config |
| `Mod+H` | hide or show the HUD |
| `Mod+Alt+L` | lock |
| `Mod+Shift+Backspace` | revert the healer's last automated fix |
| `Mod+Q` · `Mod+Shift+E` | close · quit the session |

`Mod` is Super. Set `[input] modifier = "alt"` to use Alt instead. Change any bind in `[binds]` of `config.toml`, or remove one with `"none"`. Lua modules can add binds.

**Pointer.** The pointer only reaches centre windows listed in `[input] pointer_apps` (browsers, GIMP, Blender, …) or tagged by a Lua rule. Everywhere else it is ignored, so it costs no wakeups.

## Configuration

Settings live in `~/.config/shadow-state/config.toml`. `shadow setup` writes every default as a comment. Edit the file directly, or use `shadow-config` (`Mod+Shift+,`), which applies each change live and rolls it back if the compositor rejects it. A broken file never stops the desktop: the defaults are used and the HUD shows `CONFIG ERROR`.

The main sections:

- `[layout]`: presets, height and ring geometry;
- `[look]`: amber, cyan, crimson, obsidian, plate/node opacity, blur, grid, scale (1.5) and font;
- `[display]`: `refresh_ac = 120`, `refresh_battery = 60`, and `refresh_policy` (`auto`, `powerd`, `ac` or `battery`);
- `[hud]`, `[lock]` and `[memory]`;
- `[updates]` and `[sync]`.

## Login screen

```bash
sudo pacman -S greetd && sudo shadow greeter enable    # takes effect at the next boot
shadow greeter preview                                 # try it now (inside the session: nested window)
```

greetd runs `/opt/shadow-tui/lib/greeter-wrapper`. That starts `shadow-compositor --greeter`, which falls back to the text greeter (F2 there opens a rescue shell), which falls back to agreety. When you log in, the globe collapses into the floor grid. That final frame is the session compositor's first frame, so the login blends into the desktop instead of cutting to it.

## Power and the NPU

`shadow-powerd status | mode | charge | rapl | memory | kernel | audio | npu`

- On battery the panel runs at 60 Hz; on AC at 120 Hz. The compositor performs the DRM mode change.
- Every focus change sends the 32-byte telemetry record from the spec. The daemon learns how much CPU each app class really uses.
  - Light work on battery: PL1 9 W, EPP `power`, profile `cool`.
  - Heavy work: the firmware limits come back before the load arrives.
- The learner is a per-class statistical table that runs on the CPU in microseconds. The NPU is reserved for the local language model (`shadow ai`, `shadow-healer`).
- **Memory ceiling.** The daemons (system `shadow.slice`) plus the compositor and TUI apps (user `shadow.slice`) are capped at 20 % of RAM in total. Everything you launch runs in `app.slice`.

## Wi-Fi (UC-Secure / eduroam)

`shadow-net` (`Mod+N`), or:

```bash
shadow wifi campus ucalgary --user jane.doe
```

The root daemon `shadow-netd` keeps the card usable and repairs failed connections step by step:

- legacy TLS: `SECLEVEL=1`, then `SECLEVEL=0`, scoped to that network;
- PEAP version toggle with a CA reload;
- MAC randomisation off;
- ARP flush and lease renew;
- connected but no internet: DNS-over-TLS fallback.

Password failures are never retried more than once. The HUD shows the SSID, signal (dBm) and EAP/TLS state. For the iwd backend, `shadow wifi iwd-provision` writes the `.8021x` files.

## Self-correction

Behaviour scripts live in `/var/lib/shadow-tui/modules/*.lua`, a git repository. They run in a sandboxed LuaJIT with no `os`, `io`, `debug` or loaders and a time budget.

When a module throws an error, `shadow-healer`:

1. waits until no person has edited the file for 30 seconds;
2. sends the module and the sanitised error to the local model;
3. checks the reply's AST against a capability whitelist;
4. dry-runs it in a network-less sandbox as `nobody`.

By default a passing fix waits for `sudo shadow-healer approve N`. With `mode = "auto"` in `/etc/shadow-tui/healer.toml` it is committed automatically, and `Mod+Shift+Backspace` undoes it. A fix that fails the checks is discarded and the module goes back to its last known-good commit. The healer never touches binaries. Fixes to the Rust code go through `shadow ai propose` / `review` / `approve`: a full package build with every test, and only when you type APPROVE.

## State on GitHub

```bash
shadow-sync init --github shadow-state     # creates a PRIVATE repo (gh auth login first)
shadow-sync push                           # or: shadow-sync auto on  (daily)
shadow-sync bootstrap git@github.com:you/shadow-state.git   # on a wiped machine
```

The repository holds:

- `config.toml`;
- `npu_weights.bin` (the learned tables);
- `healed_patches/`;
- `secrets.age` (Wi-Fi credentials, encrypted with age to your SSH key);
- package and service lists.

Plaintext secrets are refused by a scanner. The HUD shows the sync state.

## Optional security subsystem

```bash
sudo pacman -U shadow-tui-sec-*.pkg.tar.zst      # or ./install.sh --sec
sudo shadow-sec status
sudo shadow-sec enable usb        # also: zones sandbox audit fss ids verity crypto
shadow-sec report                 # ITSG-33 control mapping (self-assessment, NOT a certification)
shadow-sec zeroize --plan
```

Each module states what it actually enforces. The spec items it cannot honestly deliver are listed in `report` and in `docs/SPEC-TRACE.md`, for example eBPF/XDP programs, keystroke biometrics, FIPS-validated modules and TPM attestation.

## Layout of this repository

```
compositor/   shadow-compositor + shadowctl (Smithay)
lua/          shared LuaJIT sandbox, AST policy, input sanitiser
powerd/       shadow-powerd / shadow-audiod (std-only)
core/         shadow / shadow-netd / shadow-sync / shadow-healer
sec/          shadow-sec (optional package)
term/         shadow-term
tui/          sakura (std-only, built with plain rustc)
modules/      default Lua behaviour modules
system/       units, slices, session, greeter wrapper, hooks, PAM
share/        themes, hardware tables, shell integration
docs/         SPEC-TRACE.md
```

`cargo test --workspace` plus `rustc --test tui/src/main.rs` run the unit tests. `makepkg` runs them too, and refuses to package if any fail.

## Licences

The project is MIT. Third-party crates keep their own licences: Smithay and mlua/LuaJIT are MIT; fontdue is MIT/Apache-2.0; full_moon is MPL-2.0.
