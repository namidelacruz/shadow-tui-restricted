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
| `shadow-bar` | The floating side bar (layer-shell, right edge), with **APPS · SETTINGS · SYSTEM** buttons at the top. It shows battery % with time left or time to full (smoothed), and the monitor and scale, workspace, focused app, NPU state, sync, battery and watts, panel Hz, package °C and profile, audio DSP, Wi-Fi SSID/dBm/EAP, Lua widgets and the time. It sits on the laptop panel; when docked it follows the monitor that has focus. `Mod+Shift+B` hides it. |
| `shadow-flyout` | The panels behind the side bar's **APPS**, **SETTINGS** and **SYSTEM** buttons. Each opens beside its button; the same button closes it again.<br>• **Apps:** search and launch.<br>• **Settings:** brightness, volume, power mode, Wi-Fi, displays, all settings.<br>• **System:** task manager, update, security console, lock, log out, suspend, reboot, shut down; log out, reboot and shut down need a second press.<br>`Mod+A` opens Apps. Tab switches page; Esc, or moving focus away, closes it. |
| `shadow-tasks` | Task manager (`Ctrl+Shift+Esc`).<br>• **Cards:** CPU (total, per core, frequency), memory (RAM, swap, memory pressure), NPU (busy % from the Intel NPU driver, frequency, learning state), GPU (busy % from xe/i915 idle residency, frequency), power (battery draw, package power, time left, temperature), I/O (network, disk, pressure).<br>• **Processes:** sort with c/m/p, filter with /, end with k (SIGTERM) or K (SIGKILL), each confirmed first. |
| `shadow-launch` | Launcher, opened as its own normal window (tiled, opaque), not an overlay over what's on screen. Click or press Enter to launch. Its ranking learns what usually follows the app you're in, what you open at this hour, and how often. Each app shows ~RAM and ~W from what it cost before. |
| `shadow-net` | Wi-Fi: networks, UC-Secure and eduroam setup, self-repair, with a live EAP/TLS log. |
| `shadow-config` | Live settings matrix over `config.toml`. Changes apply immediately; if niri rejects one, it is rolled back. |
| `shadow-files` | Three-column file manager (parent · current · preview). |
| `shadow-update` | Updater (`Mod+U`, or "Update" in the launcher). It checks only when you click **Check for updates**, and installs only when you click **Update**. Only the files that changed are replaced. See [Updates](#updates). |
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
./install.sh --check            # verify an existing installation (✓ / ~ / ✗ with fixes)
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

**Safeguards.** The installer stops before changing anything that would leave a half-working system:

- **Pre-flight.** It checks the CPU architecture and refuses image-based Fedora Atomic / Silverblue. It also checks sudo access, at least 5 GB free (10 GB when niri or greetd are built from source), and on RHEL that the system is registered (CodeReady Builder needs it).
- **Required packages.** All of them are checked against your enabled repositories before any is installed. A missing one is named, with the repository that normally provides it.
- **Build requirements.** These are installed straight from the RPM recipe (`dnf builddep`), exactly as `rpmbuild` will check them.
- **niri version.** niri must be at least 25.08, the release the generated config is validated against. An older package is upgraded, or replaced by a source build if you agree.
- **Source builds.** Every binary built from source must run and resolve all its libraries before it is installed.
- **greetd on RHEL.** A failed greetd build never ends the installation; your current login manager stays. Under enforcing SELinux, a greetd that couldn't get its display-manager label is not enabled, because it could not log you in.
- **Verification.** The installation ends with a check, which you can also run any time with `./install.sh --check`. It looks at:
  - the package and all 12 programs;
  - niri and whether it accepts your generated config;
  - the session entry;
  - the lock screen's PAM stack;
  - services, fonts, Mesa EGL and NetworkManager;
  - SELinux labels;
  - when enabled, the login screen (greetd, its user, its self-test, its SELinux label).

  Each item shows ✓ / ~ / ✗ with the command that fixes it.

Optional extras the repositories don't carry are skipped with a note: `age`, `gh`, `alsa-sof-firmware`, CJK fonts, the Terminus console font and so on. RHEL-family systems have no Terminus console font, so the login screen keeps the default console font size there.

On RHEL without `--greeter`, GDM stays. Pick **Shadow TUI Restricted** from the gear menu on its login screen.

**Testing on Fedora / RHEL.** `tests/distro-test.sh` runs the real installer in throwaway containers (podman or docker): Fedora 43, AlmaLinux 10 and Rocky Linux 9 by default, or any images you name. In each one it does a dry run, the full install and `--check`, then prints a pass/fail summary. Logs go to `test-results/`.

Containers can't show the graphical session, running services, greetd at boot or SELinux enforcement. Test those in a VM: RHEL itself (the free developer subscription works), Rocky or Alma, with `./install.sh --greeter` and then a login.

Other entry points:

- `packaging/build-rpm.sh OUTDIR` builds the RPMs on their own.
- `makepkg` builds the Arch package.
- Both packages lay out their files through `packaging/install-files.sh` and share their install scripts (`packaging/post-install.sh`).
- Later updates: the `shadow-update` app replaces only the files that changed (see [Updates](#updates)); `shadow update run` still rebuilds and reinstalls the whole package.

After that, pick **Shadow TUI Restricted** at the login screen, or run `shadow-session` from a TTY.

Shadow's niri config is generated into `~/.config/shadow-state/niri.kdl`, and the session starts niri with `-c`. A plain niri setup in `~/.config/niri` is never touched.

**Upgrading from 6.x.** The orbital-ring compositor, `shadowctl` and the `sakura` text apps are replaced. Your `config.toml` keeps working: old keys are ignored and new ones have defaults. Run `sudo shadow greeter enable` once to switch the login screen.

## Keys

| Keys | Action |
|---|---|
| `Mod+Tab` / `Mod+J` · `Mod+Shift+Tab` / `Mod+K` | focus the next / previous window; the focused window is always centered (120 ms) |
| `Mod+[` / `Mod+]` | width preset 45 / 65 / 85 % |
| `Mod+F` · `Mod+Shift+F` | full width (the bar stays visible) · true fullscreen |
| `Mod+Return` `Mod+Space` `Mod+E` `Mod+N` `Mod+Comma` `Mod+U` | terminal, launcher, files, network, settings, updates |
| `Mod+A` · `Ctrl+Shift+Esc` | apps flyout beside the bar · task manager |
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

## Updates

### Update source (required)

Updates come only from one repository you set up. Until it is set up and meets the policy below, nothing can be checked, fetched or built.

`./install.sh` runs the setup:
- it uses `--update-repo URL`, or this checkout's origin by default;
- later, run `shadow update setup` again, or press **Set up update source** in the Update app.

The setup does four things:

1. It creates a dedicated SSH deploy key, `~/.config/shadow-state/update-deploy-key` (mode 600).
2. It shows you the key so you can add it to the repository **read-only**. With `gh` logged in it can do this for you: `gh repo deploy-key add` adds keys read-only unless `--allow-write` is given. Otherwise: repository → Settings → Deploy keys → Add, and leave "Allow write access" unchecked.
3. It pins the server's SSH host key. GitHub's published keys are built in. Any other server's fingerprint must be confirmed, interactively or with `--host-fingerprint SHA256:…`.
4. It saves the repository, branch and auth method (no secret), then checks the policy.

The policy (`shadow update policy` shows it; every fetch enforces it):

| Rule | Default |
|---|---|
| Only ssh or https. http:// and git:// are refused. A local path is allowed as a mirror. | always |
| No token or password in the URL or in `config.toml`. `credential.helper=store` (a plain-text token file) is refused too. | always |
| Authentication: read-only deploy key (SSH), or your credential helper for https (`gh auth setup-git`, libsecret). Use a fine-grained token with read-only *Contents* access for this one repository. | `auth = "deploy-key"` |
| Host key pinned; an unknown or changed key fails closed. | always (ssh) |
| The repository must be private: a credential-free probe that succeeds means it's public, and updates are refused. | `require_private = true` |
| History may not be rewritten: the new head must descend from the installed commit. | `allow_history_rewrite = false` |
| Every new commit is signed by a key in `update-allowed-signers` (git's SSH allowed-signers format). | `require_signed = false` |

Git runs isolated for updates:
- No system or global git config and no `GIT_CONFIG_*` variables apply, so a `url.insteadOf` rewrite can't silently redirect the source.
- Hooks, submodules and file:// are off (file:// stays on for a local mirror).
- Every object received is fsck-checked, TLS verification is forced on, and nothing prompts.

Fetching and building run as you. The administrator step only copies files that are already built.

### Checking and installing

Nothing checks for updates in the background. Open **Update** (`Mod+U`, or from the launcher) and:

- **Check for updates** fetches your repository (Settings → Updates). It lists the new commits, the changed source files and which programs they affect.
- **Update** does the following:
  1. It rebuilds incrementally: cargo recompiles only the crates that changed, then runs their tests.
  2. It lays the result out exactly as the package would.
  3. It compares that, byte for byte, with what is installed.
  4. It replaces only the files that differ. Files a newer version no longer ships are removed.
  5. It asks for your password once, if sudo needs it.
  6. Each file is swapped atomically, and a backup of everything touched goes to `/var/lib/shadow-tui/update-backup` (the last 3 are kept).
  7. Only the services and session programs whose binaries changed are restarted.
- **Undo last update** restores that backup.

Safeguards:

- **Config files in `/etc` are never overwritten.** When a shipped default changes, the new version is placed next to yours as `FILE.shadow-new`. If you've edited your copy and the default didn't change, nothing is offered.
- **New dependencies need the package manager.** If an update adds or drops a package dependency, the updater refuses it and tells you to run `./install.sh`.
- **Failures change nothing.** If the build or the tests fail, nothing is installed. If writing a file fails partway through, every file already changed is put back.
- **The desktop can't start on a half-updated system.** While an update, undo or package install is writing files, `/var/lib/shadow-tui/update-in-progress` exists. It is removed only once everything is on disk.
  - While it exists, the login screen refuses to start a session ("system update in progress — login paused"), and so does `shadow-session` from a TTY. A session that is already running keeps running.
  - Shutdown, suspend and lid-suspend are held off for the duration (`systemd-inhibit`).
  - The marker is also a lock, so two updates can't run at once.
  - If an update is cut off (power loss, crash, forced reboot), `shadow-update-recover.service` repairs it at the next boot, before the login screen and before any login (TTY and ssh included). It reverses a file-level update from its backup journal. It finishes an interrupted package install from the package file, then checks the result against `MANIFEST.sha512`.
  - If recovery can't succeed, the desktop stays locked. Log in on a text console and run `sudo shadow update recover` or `./install.sh`. The Update app offers **Recover interrupted update** too.
  - `./install.sh`, `shadow update run` and `shadow update rollback` install their packages under the same guard.

The same from a terminal: `shadow update check-files`, `shadow update apply`, `sudo shadow update undo`, `sudo shadow update recover`, `shadow update in-progress`.

pacman/rpm keep listing the version you last installed with `./install.sh`. The installed build itself is recorded in `/opt/shadow-tui/share/BUILD_INFO` (`version+gitCOMMIT`). The next `./install.sh` brings the package database back in line.

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

## Look

The 2D apps share a "fluid HUD" style:
- deep gradient backdrops with faint scan lines;
- rounded glass panels with soft glows;
- angled amber→violet title tags and light-beam dividers;
- pill buttons, gradient meters and sparklines.

Everything is drawn on the CPU with anti-aliased shapes, and app windows stay fully opaque.

`[look]` options: `radius` (corner radius, 0–24), `violet`, `amber`, `cyan`, `obsidian`, `opacity` (side bar only), `font`, `font_pt`.

The login console uses rounded box-drawing and a gradient beam.

`SHADOW_SNAPSHOT=out.ppm <app>` renders one frame of any app without a compositor, for screenshots.

## Optional security subsystem

```bash
./install.sh --sec                # installs shadow-tui-sec, shows what this OS supports, offers the console
shadow-sec precheck               # per module: can it be enabled on this system, and why not
shadow-sec console                # the security console (also: side bar → SYSTEM, or the launcher)
shadow-sec report                 # ITSG-33 control mapping (self-assessment, NOT a certification)
shadow-sec zeroize --plan
```

**Precheck.** A module can only be enabled after its precheck passes on this OS. Examples:

| Module | Checked |
|---|---|
| usb | kernel USB authorisation, Thunderbolt security level |
| zones | nftables tools, kernel nf_tables; notes when firewalld or ufw is also active |
| sandbox | systemd 247 or newer |
| audit | audit tools; kernel booted without `audit=0`; auditd running |
| fss | systemd built with Forward Secure Sealing |
| verity | fsverity-utils; `/opt/shadow-tui` on ext4 with the verity feature, btrfs or f2fs. XFS (the RHEL default) has no fs-verity. |
| crypto | OpenSSH with ML-KEM or sntrup hybrid key exchange |

The precheck also shows the platform's state:
- SELinux / AppArmor;
- kernel lockdown;
- Secure Boot;
- TPM 2.0;
- LUKS disk encryption.

`shadow-sec enable` itself refuses a module whose precheck fails, so nothing is ever half-configured.

**Enabling** is done from the security console, or by `./install.sh --sec`, which opens the console. Never automatically, not even with `--yes`. In the console:
- the module's precheck is shown first;
- you type the module's name to confirm;
- the change runs through sudo with the cached credential cleared (`sudo -k`), so your password is asked every time.

Every enable and disable is logged with the user who made it, in `/var/lib/shadow-sec/events.log`.

Enabling `usb`, `ids` or `zones` also enables the `shadow-sec` service. That service restores the nftables zones at every boot; before this, they were lost on reboot.

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
tests/     distro-test.sh: the installer in Fedora / Alma / Rocky containers
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
