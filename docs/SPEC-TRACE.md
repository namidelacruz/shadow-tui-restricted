# CR-SPEC-SHADOW-001: implementation trace (version 7, updated for 10.0)

This file traces each requirement of the clean-room functional specification to the code that implements it.

Target platforms: the spec names Arch Linux. Since 7.1, `install.sh` also installs on Fedora 41+ and RHEL-family 9/10 (RPM packaging, dnf dependencies). Since 10.0 the compositor is Shadow's own only; niri is no longer used.

Status:

- ✅ implemented as specified;
- ◐ implemented differently or in part (the note says how);
- ✗ not implemented;
- ⇄ changed by your instruction for version 7, overriding the spec text.

Your 7.0 instructions were:

- use niri;
- a custom floating side bar instead of waybar;
- multi-monitor support;
- every feature as its own Rust application with a 2D interface;
- a weekly learning schedule;
- a static, ly-style login;
- scaling that resolves itself and corrects the text size.

Version 6, the spec's own Smithay compositor with the orbital ring, stays on the `v6` branch.

Everything was built and tested in a Linux container, including niri 25.08 running nested, not on the reference laptop. The last section lists what still needs real hardware.

## 1. Clean-room scope

| Requirement | Status | Where / how |
|---|---|---|
| Decoupled from external window managers such as niri; no code from them | ⇄ | niri is the compositor, by your instruction. It runs as an unmodified separate program from Arch. Shadow uses only its documented interfaces: `niri msg --json` (`niri/src/lib.rs`) and the KDL config (`niri/src/config.rs`, checked with `niri validate`). niri's documentation was read; its source was not, and none of it is in this repository. |
| Public kernel interfaces only | ✅ | sysfs, DRM EDID/modes (read-only, for the login font), powercap, cgroup v2, ALSA/UCM, NetworkManager/iwd. See §3 and §4. |
| Window ergonomics and power tuning from first principles | ◐ | Center stage, presets and fullscreen are now niri layout settings (§2). The power predictor and learning schedule (`powerd/src/predict.rs`, `schedule.rs`) are Shadow's own. |

## 2. Display server and layout

| Requirement | Status | Where / how | Tested here |
|---|---|---|---|
| Own Wayland compositor on Smithay, DRM/KMS + libinput | ⇄ | niri (which is itself built on Smithay) | niri nested |
| Keyboard-first; pointer masked unless the window needs it | ◐ | niri hides the cursor while typing and after 3 s idle (`cursor { hide-when-typing; hide-after-inactive-ms }`). Pointer events still reach windows; niri has no per-window pointer mask. | config validated |
| 0 FPS when idle (PSR) | ✅ | niri redraws only on damage. Every Shadow app repaints only when its content changes (no frame clock, no timers that draw). shadow-term stops blinking after 10 s, so an idle desktop commits no frames at all. | measured: idle terminal 0 commits |
| Orbital ring in cylindrical space | ⇄ | Replaced by niri's scrolling layout with `center-focused-column "always"`: the focused window always sits center stage and the others line up beside it. `Mod+O` shows all workspaces. | screenshots |
| Spawn claims the center | ✅ | A new window opens next to the focused one, takes focus, and is centered | screenshots |
| Mod+Tab / Mod+J/K cycle focus, 120 ms cubic-bezier | ✅ | `focus-column-right-or-first` / `left-or-last`; animations are `cubic-bezier 0.2 0 0 1`, 120 ms | config validated |
| Mod+[ / ] presets 45/65/85 %, vertically centered | ✅ | `preset-column-widths` 0.45/0.65/0.85 with `switch-preset-column-width(-back)`. Configurable in `[layout] presets`. | validated |
| Mod+F full area minus the bars, again restores the preset | ✅ | `maximize-column`, which respects the bar's reserved space; pressed again it returns to the previous width. `Mod+Shift+F` is true fullscreen. | validated |
| wgpu / WGSL tactical shaders, blur, floor grid | ◐ | 8.0: `shadow-hub` draws a 3D vertex floor grid and an icosphere on the CPU (perspective projection, depth fog). They follow the cursor. The apps use vertex-wireframe glass: chamfered translucent panels, gloss, wire edges, vertex nodes. Still no GPU API in the apps, as you asked (no Vulkan), so no shaders and no blur. | screenshots, nested niri, frame timings |
| Inactive windows paused, wireframe + sparkline | ◐ | niri throttles off-screen windows' frame callbacks. Wireframe nodes and sparklines are on the hub's widgets, not on paused windows. | — |
| Persistent bezel HUD, top and bottom 28 px | ⇄ | 8.0: `shadow-hub`, by your instruction. A static core sits in the middle with radial option nodes and slide-open panels, and draggable floating widgets are wired to it (clock, power, system, network, workspaces). `Mod+Space` summons it over windows. Before 8.0: `shadow-bar`, which is still optional: one floating bar on the right edge, by your instruction. It reserves its space and stays visible in maximize; `Mod+Shift+B` hides it. Top-bar fields: monitor, workspace, focused app, NPU state, sync. Bottom-bar fields: battery % and W, panel Hz, package °C and profile, DSP, SSID/dBm/EAP. Plus Lua widgets and the time. | screenshots, fake sysfs test |
| Multi-monitor | ✅ (new, 10.0) | `shadow-compositor` lights every connected monitor and picks up hotplug (`compositor/src/udev.rs`); `compositor/src/desk.rs` keeps each screen's nine workspaces. New apps open on the second screen, dialogs next to their parent, an unplugged screen's windows move to the panel. Each screen gets its own taskbar. Lid close while docked turns the panel off. | nested split into two screens (`SHADOW_SPLIT_OUTPUTS=2`) + unit tests; not on real monitors |
| Scaling resolves itself; text corrected if not | ✅ (new) | `ui/src/scale.rs` turns EDID density into a quarter-step scale (internal panel 144 dpi, desk monitors 110 dpi reference); `shadow-displayd` applies it through niri. Each app renders at the exact fractional scale (`wp_fractional_scale_v1` + viewporter). If the scale it gets is a quarter step or more off the ideal, it multiplies text sizes to compensate; GTK gets `text-scaling-factor`. | yes: niri moved 1.0 → 1.5 on its own for a 2560×1600 / 301 mm panel; unit tests for Dell and desk monitors |

## 3. Power daemon (`shadow-powerd`)

| Requirement | Status | Where / how |
|---|---|---|
| AC detection via udev netlink | ◐ | `power_supply` is polled every 2 s (std-only daemon). shadow-displayd checks every 5 s, and on every monitor event, for the refresh switch. |
| 120 → 60 Hz on battery | ✅ | `shadow-displayd` switches the panel's mode through `niri msg output … mode` (niri performs the DRM atomic commit), following shadow-powerd's request. `[display] refresh_policy` is `auto`, `ac`, `battery` or `fixed`. |
| ACPI platform_profile, RAPL PL1/PL2, EPP | ✅ | Unchanged (`power.rs`, `rapl.rs`); RAPL never exceeds the firmware values |
| 32-byte telemetry record | ◐ | `shadow-sessiond` sends it on every niri focus change and window open (`desktop/src/telemetry.rs`); `predict.rs` decodes it. `key_cadence_hz` is 0: a niri client cannot see keystrokes, which only the focused app receives. |
| Learn for a week, relearn for a week after an OS update, dynamic after | ✅ (new) | `powerd/src/schedule.rs`. The first 7 days are watch-only (no predicted power changes). After that, predictions drive PL1/EPP/profile, with slow continual adaptation (α 0.05; new apps learn at α 0.3). An OS update means a completed `pacman -Syu` that upgraded packages, or a new kernel; it starts a fresh 7-day relearn. Sustained prediction error starts a 2-day relearn. Progress shows in the bar (`LEARNING d/7 · os update`) and in `shadow-powerd learn`. Unit-tested, and run against a fake system tree. |
| Quantized ONNX model on the NPU | ✗ | The predictor is a per-app statistical table on the CPU. `npu.state` reports `backend=cpu-statistical`. |
| Feed-forward actions | ✅ | `daemon.rs` `telemetry()`, active only after the learning phase |
| shadow-audiod | ✅ | Unchanged |
| 20 % RAM ceiling via cgroup v2 | ◐ | System `shadow.slice` (daemons, 3 % / 4 %) plus user `shadow.slice` (niri, bar, apps, session services: 13 % / 16 %), together ≤ 20 %. Launched programs go to `app.slice`. |
| Cache eviction at memory.high, zram | ✗ | Not implemented in v7. The apps' memory is small: fonts are memory-mapped and parsed lazily, and a terminal uses ~20–30 MB. |

## 4. Wi-Fi (`shadow-netd`)

| Requirement | Status | Where / how |
|---|---|---|
| UC-Secure / eduroam 802.1X profiles via NetworkManager or iwd | ✅ | `core/src/wifi.rs`: NM profiles, plus `shadow wifi iwd-provision` writes `/var/lib/iwd/*.8021x` (PEAP, MSCHAPv2, CA cert, ServerDomainMask) |
| Immutable trust store in `/etc/shadow-tui/certs/` | ◐ | Used automatically when `ca-bundle.pem` is present, otherwise the system bundle. No institution CA files ship with the package, because they can't be verified from here. |
| `SECLEVEL=1` scoped to the supplicant | ✅ | Per-connection `802-1x.openssl-ciphers` (only that network). `SECLEVEL=0` is the next step if 1 is not enough. |
| TLS alert → toggle PEAPv0/v1 + reload CA | ✅ | Fix `peap-v1` |
| Association denied / DHCP timeout → MAC randomisation off, `ip neigh flush`, renew | ✅ | Fixes `dhcp-mac`, `dhcp-renew` |
| Connected, no internet → DoT/DoH fallback or gateway ping | ✅ | `net_recover()`: gateway ping, then ARP flush + renew, or switch to DNS-over-TLS (Quad9/Cloudflare) through resolved |
| Live EAP/TLS state | ✅ | `publish()` writes `/run/shadow/net.state` for the side bar; `shadow-net` streams each operation's EAP/TLS phases live |
| Same self-repair ladder as before | ✅ | Parity runs against the earlier version: identical except the intended extra steps |

## 5. Self-learning UI and self-repairing code

| Requirement | Status | Where / how |
|---|---|---|
| Immutable Rust core + mutable LuaJIT modules in `/var/lib/shadow-tui/modules` | ✅ | `lua/` (sandbox); `desktop/src/luahost.rs` in `shadow-sessiond`, with the API rule, hud, on, bind, spawn, action, notify, state. Window rules apply through niri actions when a window opens; widgets show in the side bar; key macros become niri binds (`shadow setup niri`). |
| Launcher ranking learns | ✅ | `apps/src/desktop.rs`: a Markov table of what follows the focused app, plus hour of day, plus frequency. RAM and W estimates come from measured RSS (shadow-sessiond) and learned CPU share (shadow-powerd). |
| Trigger only on explicit errors; 30 s human-edit gate; sandbox dry run; git commit/revert; Mod+Shift+Backspace | ✅ / ◐ | Unchanged from 6.x (`core/src/healer.rs`). The 30 s gate uses the file's modification time, not inotify. `shadow-sessiond reload` loads approved fixes. |
| Automatic commit | ◐ | `approve` by default; `auto` is optional. Binaries are never patched. |

## 6. Version independence and sync

| Requirement | Status | Where / how |
|---|---|---|
| Self-contained hashed `/opt/shadow-tui` | ◐ | Shadow's binaries live there with `MANIFEST.sha512` on Arch and on Fedora/RHEL (RPM, the same file layout from `packaging/install-files.sh`). On Arch they also carry DT_RPATH and a bundled libxkbcommon; the RPM uses the system library. niri comes from Arch (`depends=niri`), and Shadow talks to it only over IPC and the config file, so a niri update cannot break Shadow's binaries. Mesa, glibc and PAM stay the system's. |
| State directory, `shadow-sync push`, `bootstrap` | ✅ | Unchanged. The state also carries `niri.kdl` (generated), `macros.list`, `launcher.markov` and `apps.ram`. |

## 7. Applications (each its own Rust 2D app)

| App | Status | Notes |
|---|---|---|
| shadow-greet | ⇄ | Static console login for greetd, in the style of ly, by your instruction (the spec asked for a 3D animated one). No power buttons, no command hints. The Terminus console font is picked from the panel's EDID density. Disk/keychain unlock ✗. |
| shadow-term | ◐ | Unchanged terminal, with fractional scale, lazy fonts and quiet idle. Not alacritty_terminal; no sixel. |
| shadow-net | ✅ | 2D app: devices, networks, saved connections, UC-Secure and eduroam setup, repair, a live EAP/TLS log |
| shadow-config | ✅ | 2D live settings matrix: 9 pages including per-monitor scale and the learning schedule; rolled back if niri rejects a change |
| shadow-files | ◐ | 2D three-column file manager in the style of yazi (not a yazi build) |
| shadow-launch | ✅ | Intent-ranked (Markov + time of day + frequency), with ~RAM / ~W per app |
| shadow-lock | ✅ | ext-session-lock on every monitor, PAM on a worker thread |
| shadow-bar | ✅ (new) | See §2. Optional from 8.0 |
| shadow-hub | ✅ (new, 8.0) | The desktop: reactive vertex field, central hub, widgets, summon overlay. See §2 and the README |

## 8–9. Security and compliance (optional `shadow-tui-sec` package)

Following your instruction, the security and compliance parts are a separate, optional command and package. Nothing is active until `shadow-sec enable MODULE`.

| Requirement | Status | `shadow-sec` module / note |
|---|---|---|
| Keystroke dynamics on the NPU | ✗ | — |
| eBPF syscall telemetry, 5 ms severing | ◐ | `ids`: /proc + audit-based detection of reverse shells, memfd (fileless) execution and foreign ptrace → SEC-ALERT in the side bar, optional SIGSTOP. No eBPF programs and no timing guarantee. |
| Automatic seccomp/Landlock synthesis | ◐ | `sandbox`: static systemd sandboxing for the daemons (not synthesised) |
| Prompt-injection defence for the healer | ✅ | Built into the core: `sanitize_external`, data fencing, AST capability whitelist, sandbox dry run (§5). Not optional. |
| fs-verity + TPM-signed weights + golden image | ◐ | `verity`: fs-verity (SHA-512) on `/opt/shadow-tui` binaries, checked by `verify`. No TPM signing and no golden image. |
| Per-client Wayland isolation | ◐ | Now niri's: keyboard input goes only to the focused window and the session lock covers every monitor. niri does offer screencopy/screencast (through xdg-desktop-portal with a user prompt), unlike the v6 compositor. |
| Landlock + seccomp per daemon, CFI, hardened_malloc | ◐ | `sandbox` (systemd). No CFI or hardened_malloc. |
| Evil-twin / downgrade detection | ◐ | Core: strict CA + domain-suffix validation, and EAP downgrade fixes are opt-in (`allow_unvalidated`). No BSSID/OUI anomaly tracking. |
| XDP micro-firewall | ◐ | `zones`: nftables with scan signatures dropped at prerouting/raw (not XDP) |
| USBGuard + Thunderbolt DMA lockdown while locked | ◐ | `usb`: kernel `authorized_default=0` while the session is locked (shadow-lock running); devices plugged in meanwhile are authorised after unlock. Thunderbolt security level is reported, not changed. |
| FIPS 140-3 modules, AES-256 / SHA-384 only | ✗ | Reported as not implemented. LUKS2 cipher reported by `verify`. |
| PQC (ML-KEM + ML-DSA) for sync/SSH/TLS | ◐ | `crypto`: OpenSSH ML-KEM-768/X25519 hybrid key exchange preferred, TLS 1.3 for git. No ML-DSA. |
| Network zones PAZ/OZ/MZ | ◐ | `zones`: host firewall + no network for powerd/healer (MZ). No netns per zone. |
| Smart card / FIDO2 MFA | ◐ | PAM-based: add `pam_u2f` / `pam_pkcs11` to `/etc/pam.d/shadow-lock` and greetd (documented in `report`) |
| Session lock, drop keys | ◐ | `shadow-lock` (ext-session-lock, every monitor). Keys are not dropped from the page cache. |
| auditd AU-2/AU-9 + sealed logs | ✅ | `audit`, `fss` (journald Forward Secure Sealing; `verify` checks) |
| UEFI Secure Boot with own keys, UKI, PCR-sealed LUKS | ◐ | Status reported by `verify`. Key enrolment is left to sbctl/systemd-ukify. |
| Encrypted swap, no hibernation | ◐ | Reported by `verify` |
| Zeroize (key chord + token, < 2 s) | ◐ | `shadow-sec zeroize --plan / --execute`: typed confirmation ×2, optional USB token serial, `cryptsetup erase` of every LUKS2 header + shred of learned weights. Not wired to a login-screen key chord. |
| Compliance claims (CAF/DND/CSE) | — | `shadow-sec report` maps controls to ITSG-33 families as a self-assessment and states that this is not a certification |

## What was verified, and what still needs the laptop

Tested in the container:

- unit tests in every crate (138 in 10.0: the niri config generator's and the side bar's went with them, the screens / workspaces / title bar / shell API / updater ones are new);
- 10.0.0 (nested in headless sway, the window split into two screens):
  - **Two screens:** two terminals opened on the second screen and tiled there, with bilingual title bars; the first screen stayed free. Screenshots: `docs/screenshots/10.0-*`.
  - **Shell:** a WebKitGTK taskbar on each screen; start, widgets and quick settings flyouts opened on the focused screen and closed with the same key. Clicking the pinned terminal on the second screen's taskbar started it there. Clicking that screen's workspace pill switched only that screen (the terminal stayed on workspace 1, its taskbar button still marked running).
  - **Title bars:** □ maximized the window (the other tile hidden under it); ✕ folded it shut and closed it.
  - **Automatic update pipeline:** a local mirror staged with `shadow update stage --low` (fetch, verify, idle-priority build, tests). The first run hit a failing test and refused to install, as it should.
  - **Not tested:** real monitors and hotplug, the DRM path with two CRTCs, the notification's Install button (needs a notification daemon), and WebKitGTK on Fedora/Arch.
- (9.x and earlier) niri 25.08 nested at 2560×1600, with the generated config;
- `shadow-displayd` setting scale 1.5 on its own;
- the side bar following niri's events;
- launcher, settings, files, network, lock and terminal all rendering at 1.5 (screenshots);
- the login screen previewed in a terminal;
- 9.0.0 (the glyph desktop, nested in headless sway, 1600×1000 and 1920×1200):
  - **Desktop:** screenshots in `docs/screenshots/9.0-*`.
    - The edge columns reserve their width, and apps tile between them (1–3 apps checked).
    - Minimizing moves an app into the grid (strip or full grid) with its last frame, glyph and name; the grid scrolling was moved with the `grid-scroll` action.
    - The hidden hub draws and closes.
    - The contour background and travelling lights render.
  - **Kinetic unfolding:** checked frame by frame at 2.4 s for open (from a corner origin) and for minimize. Unit tests: the wave starts at the origin and completes, closing folds toward it, the cells tile the rect exactly, and hinges face the origin. Not run on hardware.
  - **Shadow Glyph** (font) and the glyph icons: rendered sheets. Unit tests: every glyph has ink, none touches the edge, and apps map to glyphs.
  - **Settings service:** checked over HTTP:
    - no key or a wrong key → 401;
    - a foreign Host → 403;
    - a cross-site Origin → 403;
    - a form post → 400;
    - out-of-schema values and invalid TOML refused;
    - a valid setting written;
    - the cookie is HttpOnly and SameSite=Strict;
    - the key file is mode 600.

    The web app ran in headless Chromium: it unlocked from the fragment and removed the key from the address bar, and its settings page and tessellated page change render.
  - **Login screen:** in the nested compositor, it boots in and shows the card, the desktop switch (four fake Wayland sessions and an X11 one) and the power row. Not tested: a real greetd login, and suspend, restart or shut down.
  - **Not tested here:** the touchpad on hardware, a real browser installing the PWA, and the niri session with the glyph shell.
- 8.3.0 (Shadow's own compositor, nested in headless sway at 1600×1000):
  - **Mouse:** hover and click reach the hub, the tutorial panel and windows. A click on a ring node brought the terminal to the centre. The compositor draws its own cursor (arrow, hand, I-beam, reticle, busy, not-allowed), or the app's cursor surface when it sets one. Clicks were driven through a test-only control command (`test-click`, enabled only by `SHADOW_TEST_INPUT`): wlrctl's virtual pointer disappears after every call, so its clicks never arrive. Motion was also checked with wlrctl. Not tested on hardware: libinput touchpad settings, pointer lock and relative motion.
  - **Default apps:** under the compositor, all eleven app windows open: terminal, launcher, network, settings, files, tasks, fix, prefs, Craft, wallpaper picker and update. So do the summoned hub, its APPS panel and the flyout (screenshots). The DEFAULTS terminal and file manager drive `Mod+Return`/`Mod+E` (unit test). `shadow setup niri` reloads the running compositor.
  - **niri compatibility:** the compositor now answers the actions the apps send (focus, width, fullscreen, close, `set-column-width`, `focus-window`), full output details, and `output … scale`. The event stream is replaced by polling turned into niri's events (unit test with the bar's model).
  - **FIELD TRAINING:** it opened on first login, advanced on the real action, skipped and closed by mouse, and wrote its marker. Unit tests: steps follow rebinds and removed binds, the lock step is swallowed, and keys print as keycaps.
  - **X11 apps:** `xwayland-satellite` is started when installed. It isn't installed in this container, so this is untested.
  - **Installer:** the window was driven by mouse inside the compositor (APPS ONLY, untick two, INSTALL) and handed back `apps shadow-term shadow-files shadow-launch`. A real apps-only install by a normal user (Arch flow, package manager stubbed) built Files and Craft into `~/.local`, Files ran from there, and `--remove-apps` left nothing behind. The text menu and the Fedora path were dry-run.
- 8.2.0:
  - **Hub speed:** measured per frame at 2560×1600 while the mouse moves, best of 5 runs on this 2-thread container.
    - 8.1.1: 11.8 ms.
    - 8.2, mains power: 8.3 ms at 60 fps.
    - 8.2, battery: 7.4 ms at 30 fps.
  - **Wallpaper and UI at your setup** (niri 26.04, 2560×1600, scale 1.5), no hub crashes:
    - an image through the hub;
    - the wallpaper program with the side bar (it was blank before 8.1.1);
    - the vertex field.
  - **Shadow's own compositor** (`shadow-compositor`, nested):
    - it starts the hub;
    - `doctor` finds it;
    - it answers version, workspaces and focused-output the way niri does;
    - the summoned hub and an app window work.
    - Not done yet (8.2): XWayland, multiple monitors, ext-session-lock, the event stream — 8.3 adds X11 through xwayland-satellite and the event stream. Not tested yet: its DRM/KMS mode on hardware, and WSL itself. WSL detection is unit-tested on a fake tree, and the session script's WSL branch is syntax-checked only.
  - **Craft manager:** unit tests for asset choice by exact name and repository, a refused checksum mismatch, install, update, remove and the SHA256SUMS fallback. The GitHub API is unreachable from this container, so a real download wasn't run.
  - **Crash reports:** redaction is unit-tested, and the file is written by the console command.
  - **Hardware profile:** tiers are unit-tested for a laptop, a netbook, a desktop and WSL.
- 8.1.0 (on a real niri 26.04 build, nested, the version you run):
  - The hub works on it: field following the pointer, ring and panels, typing on the desktop, glass windows, the summoned hub with the windows behind it blurred. `shadow-hub doctor` and `switch` work too.
  - niri 26.04 accepts the generated config:
    - with blur;
    - with the solid colour off;
    - with foot as the terminal;
    - with `nautilus --new-window` as the file manager.
  - niri 25.08 rejects the blur lines. `shadow setup niri` then writes the config without them and names niri's reason (`unexpected node \`background-effect\``).
  - Preferences and the crash log run in it. Unit tests cover:
    - the crash log reading seeded logs: fallback, refused config, hub exit, a panic named by its program;
    - the terminal flags;
    - the solid-colour and default-app binds;
    - command sanitising.
  - Found and fixed: an empty `XDG_STATE_HOME`/`XDG_CONFIG_HOME`/`XDG_DATA_HOME`/`XDG_CACHE_HOME` was taken as a path (relative to the current folder). That affected the config directory of every app, the logs and the trash. Empty now counts as unset.
- 8.0.2 (the 8.0.1 fixes checked end to end in nested niri 25.08, plus what was still untested):
  - `shadow-hub switch` from a 7.x session (side bar + wallpaper running, the 7.x niri config) gives: hub and supervisor running, bar and wallpaper stopped, `Mod+Space` bound to the hub in the rewritten config. niri reloaded the new config live without errors (its log shows one more config load). `shadow-hub use-bar` switches back. The updater now runs that same command, so its switch is the tested one.
  - Keyboard on the desktop works: one click on the empty desktop, then typing "foot", opens APPS with the search filled in. That gives background-layer `on-demand` focus under niri.
  - This found a ranking bug: "Yazi (foot)" came before Foot. The app's own name now outweighs a match elsewhere, in the hub, the flyout and the launcher.
  - Dragging a widget is a unit test: grab, move, release, saved to `[hub.widgets]`, read back. It is not a compositor test, because wlrctl can't hold a button down.
- 8.0.1 (real-device failure paths, checked in nested niri 25.08):
  - Crashes: a crash of the hub is restarted, and 3 crashes switch to the side bar + wallpaper with a notification and a log entry. Logging out or `pkill` is not treated as a crash.
  - Older niri: a stand-in niri that rejects `overview {}` now gets a compatibility config that still starts the hub. Before, the whole new config was kept out and the 7.x desktop stayed.
  - Wallpaper: with niri's opaque workspace colour, the wallpaper program (placed in the backdrop) was invisible outside the overview. Measured: the screen was obsidian before the fix and showed the wallpaper after it. Workspaces are now transparent, and the obsidian moves to the overview backdrop.
  - A panel clamp that panicked on monitors narrower than the panel.
- 8.0 (vertex wireframe UI, shadow-hub):
  - real niri 25.08, nested at 1600×1000 with the generated config (`niri validate` passes, including `overview { backdrop-color }`):
    - the hub starts from `spawn-at-startup`;
    - the field follows a virtual pointer (wlr-virtual-pointer);
    - clicking the core opens the ring, and clicking POWER opens its panel;
    - `shadow-hub summon settings` shows over a terminal window;
    - the overview keeps the hub out of the backdrop.
  - Testing in the nested niri found and fixed a hang: a panel whose chips didn't fit yet, mid-animation, looped forever. Every page is now rendered at seven animation stages as a check.
  - Unit tests: icosphere shape, easing settles, the field draws, widget positions parse and clamp, polygon fill and the wire primitives, and the hub binds and startup in the niri config.
  - Headless snapshots of the hub (desktop, POWER, LOOK, summon APPS), the wallpaper picker and a glass app window.
  - CPU, measured in the container: a frame while the field moves takes about 5 ms at 1600×1000 and about 12 ms at 2560×1600. A full hub redraw takes about 25 ms and 60 ms. Idle in nested niri, the hub uses about 0.5 % of one core.
  - Not run here:
    - real hardware;
    - dragging a widget with a real pointer (covered by a unit test since 8.0.2).
- 7.5:
  - smart-charging guard (unit tests): only firmware-listed modes, no-op detection, read-back, 6-writes-per-day budget, 7-day AC history, smart choice;
  - side-bar POWER dropdown, wallpaper picker, generated default wallpaper and fix center, all rendered headless and checked visually;
  - the wallpaper's image modes and PNG decoding (unit tests);
  - fix-center JSON/JSONC/user.js edits that keep comments and other keys, and the launcher Exec rewrite including Flatpak (unit tests);
  - `shadow fix scan`, `apply all-user` and `run` on a scratch home with Discord and Firefox configs;
  - `shadow-passfix`'s password test against real PAM (pam_unix): wrong, right and expired passwords told apart;
  - faillock policy and counter parsing (unit tests).
  - Not run here: the wallpaper on a real compositor, Dell firmware writes, gnome-keyring, and real Discord, VS Code or Steam installs.
- 7.4.1 permissions:
  - unit tests for the brightness logic and backlight choice;
  - polkit agent detection checked with a stand-in agent;
  - installer dry runs on Fedora, Arch and RHEL 9 install an agent and warn about wheel;
  - `rpmspec` accepts the either-agent Recommends.
  - The actual prompts need a real session with an agent; not run here.
- 7.4, tested in the container:
  - canvas unit tests: rounded anti-aliased shapes, gradients;
  - headless snapshots (`SHADOW_SNAPSHOT`) of the launcher, side bar (fake battery: "5h03 left"), the three flyouts and the task manager (simulated NPU/GPU counters), all checked visually;
  - task-manager parsers and NPU/GPU sysfs readers (unit tests);
  - battery time-left (unit test);
  - security precheck in the container: unsupported modules correctly reported, and `enable` refuses them;
  - the security console enabled `sandbox` after precheck and confirmation, and logged it (as root here; as a normal user it stopped at the sudo password prompt).
- the update source policy (7.3), with unit tests:
  - URL rules and secret detection;
  - hardened, isolated git;
  - the pinned GitHub keys match GitHub's published fingerprints;
  - a force-push is refused;
  - unsigned commits are refused and signed ones accepted.
- the update source policy end to end:
  - with no source, nothing is checked;
  - the setup wizard against GitHub created the deploy key, pinned the host keys and refused a URL with a token;
  - a local mirror updated 1 file;
  - a rewritten branch was refused.
  - In this sandbox an authenticating proxy answers every GitHub HTTPS request, so the "private" probe can't give a real answer here.
- the update failsafe (7.2.1):
  - unit tests cover a write failure partway through (fully reversed, unlocked), a power cut simulated with a stale marker (blocked until recovered, then restored) and the lock against concurrent updates;
  - `shadow-session` refused to start while the marker existed;
  - the greeter refuses a login while the marker exists (unit test);
- file-level updates (7.2) end to end, against a scratch git remote and system root: only the changed files were replaced, `/etc` went to `.shadow-new`, undo worked and a dependency change was refused;
- the learning schedule run against a fake system tree: first week, then an OS-update relearn;
- the healer and Wi-Fi tests carried over from 6.x.

Not tested, because it needs the Dell 14 Plus:

- niri on DRM with the Arc 140V (xe + Mesa iris);
- real EDID sizes;
- docking with an external monitor and the lid switch;
- the 120↔60 Hz mode change;
- the login screen on a real console under greetd, and setfont;
- PAM;
- RAPL, platform_profile and EPP on Lunar Lake;
- SOF audio;
- the NPU;
- real 802.1X networks;
- cgroup limits under systemd;
- every `shadow-sec` module.
