# Shadow TUI Restricted

A keyboard-first tactical desktop for Arch Linux, Fedora and RHEL-family systems, built to the clean-room specification CR-SPEC-SHADOW-001 and tuned for the Dell 14 Plus (2560×1600 panel, Lunar Lake, Arc 140V, Intel NPU).

Version 7 runs on **niri** as the compositor. Shadow talks to niri only through its public interfaces (`niri msg --json` and its config file), so niri comes from your distribution and updates on its own. On top sit:

- a floating information bar on the right edge;
- every feature as its own Rust application with a 2D interface;
- scaling that sets itself per monitor and corrects the text size when it can't;
- a static console login in the style of ly;
- a power daemon whose habit model learns for a week, relearns for a week after every OS update, then keeps adapting.

The Wi-Fi self-repair daemon, the sandboxed self-correcting Lua layer and GitHub state sync carry over. The security subsystem is a separate, optional package.

Version 6, with its own Smithay compositor and orbital ring, stays on the `v6` branch. `docs/SPEC-TRACE.md` lists, requirement by requirement, what is implemented as specified, what differs, and where you changed the spec.

## Components

| Component | What it is |
|---|---|
| `shadow-bar` | The floating side bar (layer-shell, right edge). It shows the monitor and scale, workspace, focused app, NPU state, sync, battery and watts, panel Hz, package °C and profile, audio DSP, Wi-Fi SSID/dBm/EAP, Lua widgets and the time. It sits on the laptop panel; when docked it follows the monitor that has focus. `Mod+Shift+B` hides it. |
| `shadow-launch` | Launcher. Its ranking learns what usually follows the app you're in, what you open at this hour, and how often. Each app shows ~RAM and ~W from what it cost before. |
| `shadow-net` | Wi-Fi: networks, UC-Secure and eduroam setup, self-repair, with a live EAP/TLS log. |
| `shadow-config` | Live settings matrix over `config.toml`. Changes apply immediately; if niri rejects one, it is rolled back. |
| `shadow-files` | Three-column file manager (parent · current · preview). |
| `shadow-lock` | Session lock (ext-session-lock): one static screen per monitor, PAM. |
| `shadow-term` | The terminal (amber/cyan, HUD strip, command blocks). |
| `shadow-greet` | Static console login for greetd, with no animation, power buttons or command hints. The console font is sized to the panel. |
| `shadow-sessiond` | Session event hub. It turns niri's event stream into telemetry for shadow-powerd, hosts the Lua layer (window rules, bar widgets, key macros) and records per-app memory. |
| `shadow-displayd` | Monitors: automatic scale per monitor, text-size correction, 120/60 Hz on AC/battery, dock mode on lid close, idle lock and monitor power-off. |
| `shadow-powerd` | Power daemon (root): platform profile, EPP, RAPL PL1/PL2, charge limits, kernel guard, the learning schedule and predictive tuning, and the 20 % memory ceiling. As `shadow-audiod`: the Lunar Lake audio initialiser. |
| `shadow` | System tools. As `shadow-netd`: the Wi-Fi self-repair daemon. As `shadow-sync`: state and GitHub sync. As `shadow-healer`: Lua self-correction. Also `setup` (including `setup niri`), `greeter`, `wifi`, `ai` and `update`. |
| `shadow-sec` | The optional package `shadow-tui-sec`: security modules you enable one at a time. |

Every Shadow app renders on the CPU into shared memory and repaints only when something it shows changes, so an idle desktop draws 0 frames. Each one uses a few MB (the bar about 7 MB, a terminal about 20–30 MB). No app uses a GPU API. niri composites with OpenGL ES, and the whole session is pinned to OpenGL. The Arc 140V runs on Intel's `xe` kernel driver with Mesa `iris`; GTK and Qt apps are kept off Vulkan.

## Install

One installer covers Arch Linux, Fedora and the RHEL family (RHEL, CentOS Stream, Rocky, Alma, Oracle Linux, versions 9 and 10). It installs every dependency, builds a native package (pacman on Arch, RPM elsewhere) and installs it, so updates and removal go through the normal package manager.

```bash
git clone <this repo> && cd shadow-tui-restricted
./install.sh                    # dependencies, build, package, install, account setup
./install.sh --greeter --sec    # also the login screen and the optional security package
./install.sh --dry-run          # print every command it would run, change nothing
./install.sh --bootstrap git@github.com:you/shadow-state.git   # new machine: restore your state too
./install.sh --uninstall
```

What it does on each system:

| | Arch | Fedora 41+ | RHEL family 10 | RHEL family 9 |
|---|---|---|---|---|
| Extra repositories | — | — | CodeReady Builder + EPEL 10 | CodeReady Builder + EPEL 9 |
| niri | `extra/niri` | Fedora's `niri` | `yalter/niri` COPR (falls back to a source build) | built from source (`--niri-ref`, default v25.08) into `/usr/local` |
| Rust ≥ 1.89 | `rust` | `rust` / `cargo`, else rustup | `rust` / `cargo`, else rustup | `rust` / `cargo`, else rustup |
| Package | `makepkg` → `pacman -U` | `rpmbuild` → `dnf install` | `rpmbuild` → `dnf install` | `rpmbuild` → `dnf install` |
| Login screen (`--greeter`) | `greetd`, `terminus-font` | `greetd`, `greetd-selinux`, `terminus-fonts-console` | greetd built from source, run in the display-manager SELinux domain; or keep GDM | same as 10 |

rustup is only used, after asking, when the distribution's Rust is older than 1.89. It installs into `~/.cargo`, and `rustup self uninstall` removes it.

Optional extras the repositories don't carry are skipped with a note: `age`, `gh`, `alsa-sof-firmware`, CJK fonts, the Terminus console font and so on. RHEL-family systems have no Terminus console font, so the login screen keeps the default console font size there.

On RHEL without `--greeter`, GDM stays. Pick **Shadow TUI Restricted** from the gear menu on its login screen.

Other entry points:

- `packaging/build-rpm.sh OUTDIR` builds the RPMs on their own.
- `makepkg` builds the Arch package.
- Both packages lay out their files through `packaging/install-files.sh` and share their install scripts (`packaging/post-install.sh`).
- `shadow update run` later rebuilds whichever kind of package your system uses from your repository.

After that, pick **Shadow TUI Restricted** at the login screen, or run `shadow-session` from a TTY.

Shadow's niri config is generated into `~/.config/shadow-state/niri.kdl`, and the session starts niri with `-c`. A plain niri setup in `~/.config/niri` is never touched.

**Upgrading from 6.x.** The orbital-ring compositor, `shadowctl` and the `sakura` text apps are replaced. Your `config.toml` keeps working: old keys are ignored and new ones have defaults. Run `sudo shadow greeter enable` once to switch the login screen.

## Keys

| Keys | Action |
|---|---|
| `Mod+Tab` / `Mod+J` · `Mod+Shift+Tab` / `Mod+K` | focus the next / previous window; the focused window is always centered (120 ms) |
| `Mod+[` / `Mod+]` | width preset 45 / 65 / 85 % |
| `Mod+F` · `Mod+Shift+F` | full width (the bar stays visible) · true fullscreen |
| `Mod+Return` `Mod+Space` `Mod+E` `Mod+N` `Mod+Comma` | terminal, launcher, files, network, settings |
| `Mod+Ctrl+arrows` · `Mod+Shift+Ctrl+arrows` | focus another monitor · move the window there |
| `Mod+1…9` · `Mod+Shift+1…9` | workspace · move the window to a workspace |
| `Mod+O` | overview |
| `Mod+Shift+B` | hide / show the side bar |
| `Mod+Alt+L` | lock |
| `Mod+Shift+Backspace` | revert the healer's last automated fix |
| `Mod+Q` · `Mod+Shift+E` | close · quit the session |

Media and brightness keys also work on the lock screen. Lua modules can add key macros; the core binds always take precedence. The pointer hides while you type and after 3 s idle.

## Scaling and monitors

`shadow-displayd` gives every monitor the scale that suits its real pixel density as soon as it is connected. It uses quarter steps, with a closer viewing distance assumed for laptop panels.

| Monitor | Scale |
|---|---|
| Dell 14 Plus panel | 1.5 |
| 27" 4K | 1.5 |
| 32" 4K | 1.25 |
| 27" 1440p | 1.0 |
| 24" 1080p | 1.0 |

To pin a different scale, set it in `[display.scale]`, for example `"DP-1" = 1.25`, or in shadow-config → Displays.

If the scale a monitor ends up with still differs from the ideal (a pinned value, an EDID quirk, a compositor that ignores the request), the text size is corrected instead:

- Shadow apps do it themselves.
- GTK apps get `text-scaling-factor`.

Every app renders at the exact fractional scale (`wp_fractional_scale_v1` + `wp_viewporter`), so text stays sharp at 1.25 or 1.5. `shadow-displayd status` shows what was decided for each monitor.

**Dock mode.** Closing the lid with an external monitor on turns the panel off instead of locking, and opening it turns the panel back on. The side bar moves to whichever monitor has focus.

## Configuration

Settings live in `~/.config/shadow-state/config.toml`. `shadow setup` writes every default as a comment. Edit the file, or use `shadow-config` (`Mod+Comma`), which applies each change live.

The sections:

- `[layout]`: presets, default width, gaps, transition time.
- `[look]`: colours, opacity, font, text size.
- `[display]`: refresh policy, refresh rates and `[display.scale]` pins.
- `[bar]`: width.
- `[input]`: XKB layout and options, key repeat.
- `[session]`: lock and monitor-off timeouts.
- `[memory]`, `[power]`, `[updates]` and `[sync]`.

Layout, look and input become niri's config through `shadow setup niri`. niri validates the result before it replaces the running config; a rejected change is not applied.

## Login screen

```bash
sudo pacman -S greetd terminus-font && sudo shadow greeter enable   # takes effect at the next boot
shadow greeter preview                                              # try it in any terminal (password: shadow)
```

greetd runs `/opt/shadow-tui/lib/greeter-wrapper`, which starts `shadow-greet` on the console. It shows one static box with two fields, login and password, and an AUTHORIZED ACCESS ONLY line. It has no clock, animation, session menu, power buttons or command hints.

The console font is chosen from the panel's EDID, so the box is the same physical size on any screen: `ter-v24n` on the 2560×1600 panel, `ter-v16n` on a 1080p screen. Set `console_font` in `/etc/shadow-tui/greeter.conf` to `off` or to a font name to override it. If the greeter can't start, greetd's own `agreety` prompt appears instead.

## Power and the NPU

`shadow-powerd status | mode | learn | charge | rapl | memory | kernel | audio | npu`

**Learning schedule.**

- **First week.** For the first 7 days the model only watches and fits quickly. Power stays on the firmware defaults plus the AC/battery policy, and the bar shows `LEARNING d/7`.
- **After the week.** Its predictions drive the power settings:
  - light work on battery: PL1 9 W, EPP `power`, profile `cool`;
  - heavy work: the firmware limits come back before the load arrives.
- **Dynamic self-learning.** Learning continues slowly, and apps it hasn't seen still learn fast.
- **OS update.** A completed `pacman -Syu` or a new kernel starts a fresh 7-day relearn (`reason: os update`). Installing a single package does not.
- **Drift.** If predictions keep missing (error above 25 % over 40+ samples), a 2-day relearn starts.
- **Commands.** `shadow-powerd learn` shows where it is; `shadow-powerd learn --relearn` starts a fresh week.

**How it works.** Every focus change in niri sends the spec's 32-byte telemetry record. The model is a per-app statistical table that runs on the CPU in microseconds. The NPU is reserved for the local language model (`shadow ai`, `shadow-healer`).

**Refresh rate.** The panel runs at 60 Hz on battery and 120 Hz on AC. `shadow-displayd` switches the mode through niri.

**Memory ceiling.** niri and the Shadow apps (user `shadow.slice`) plus the daemons (system `shadow.slice`) are capped at 20 % of RAM in total. Everything you launch runs in `app.slice`.

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

Password failures are never retried more than once. The side bar shows the SSID, signal (dBm) and EAP/TLS state, and `shadow-net` streams each connection's EAP/TLS phases live. For the iwd backend, `shadow wifi iwd-provision` writes the `.8021x` files.

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

Plaintext secrets are refused by a scanner. The side bar shows the sync state.

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
ui/        shadow-ui: the shared 2D toolkit (windows, layer panels, lock surfaces, fractional scale, text correction)
bar/       shadow-bar
apps/      shadow-launch, shadow-net, shadow-config, shadow-files, shadow-lock
desktop/   shadow-sessiond, shadow-displayd
niri/      niri IPC client (niri msg --json), event-stream model, config generator
greet/     shadow-greet (static console login, no dependencies)
term/      shadow-term
powerd/    shadow-powerd / shadow-audiod (learning schedule in schedule.rs)
core/      shadow / shadow-netd / shadow-sync / shadow-healer
lua/       shared LuaJIT sandbox, AST policy, input sanitiser
sec/       shadow-sec (optional package)
modules/   default Lua behaviour modules
system/    units, slices, session, greeter wrapper, hooks, PAM, desktop entries
packaging/ RPM spec, build-rpm.sh, install-files.sh (shared file layout), post-install.sh (shared scriptlets)
share/     hardware tables, shell integration
docs/      SPEC-TRACE.md
```

`cargo test --workspace` runs the unit tests. `makepkg` runs them too, validates the generated niri config with `niri validate`, and refuses to package if anything fails.

## Licences

The project is MIT. Third-party crates keep their own licences:

- smithay-client-toolkit, mlua/LuaJIT and ab_glyph: MIT;
- fontdue: MIT/Apache-2.0;
- full_moon: MPL-2.0.

niri (GPL-3.0) is a separate program that Shadow runs and configures; none of its code is part of this repository.
