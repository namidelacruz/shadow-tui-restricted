# CR-SPEC-SHADOW-001: implementation trace

This file traces every requirement of the clean-room functional specification to the code that implements it.

Status:

- ✅ implemented as specified;
- ◐ implemented differently or in part (the note says how);
- ✗ not implemented.

Everything was built and tested in a Linux container, not on the reference laptop. The "Tested here" column says what that means for each item. The last section lists what must be checked on real hardware.

## 1. Clean-room scope

| Requirement | Status | Where / how |
|---|---|---|
| No code from scrolling tiling WMs (niri) or OEM optimisers | ✅ | `compositor/` is written from scratch on the Smithay library, which the spec names. The niri fork, its patch and every `niri msg` call were removed. Smithay's own example compositors (anvil/smallvil, MIT) were read as API documentation for the library. No Dell software was used. |
| Public kernel interfaces only | ✅ | sysfs, DRM/KMS, libinput, ALSA/UCM, powercap, cgroup v2, NetworkManager/iwd. See §3 and §4. |
| Window ergonomics and power tuning from first principles | ✅ | `compositor/src/orbit.rs` holds the geometry and state machine (unit-tested). `powerd/src/predict.rs` holds the predictor. |

## 2. Display server

| Requirement | Status | Where / how | Tested here |
|---|---|---|---|
| Independent Wayland compositor in Rust on Smithay, DRM/KMS + libinput | ✅ | `compositor/src/udev.rs` (DRM/GBM/EGL, libseat, libinput) and `winit.rs` (nested) | nested only (no GPU/DRM device in the container) |
| Keyboard-first: pointer events masked unless the centre window is `requires_pointer` | ✅ | `input.rs`: motion, buttons and scroll are dropped before any client sees them. The tag comes from `[input] pointer_apps` or a Lua rule. | yes (`pointer_live` false for terminals) |
| Damage-tracked rendering, 0 FPS when idle (PSR) | ✅ | Elements have stable ids. A frame is rendered only when something is marked dirty, and DRM page flips are queued only when the damage is not empty. | yes (nested). PSR itself needs the panel. |
| Cylindrical space, centre focal plane, elliptical ring `x=Rx·sinθ, z=−Rz·cosθ`, symmetric θᵢ | ✅ | `orbit.rs` `Geo::node`, `slot_angle`, with pinhole projection | yes + unit tests |
| Spawn claims the centre, previous centre goes to θ₁ | ✅ | `Ring::spawn` | yes |
| Mod+Tab / Mod+J/K rotate with a 120 ms cubic-bezier | ✅ | `transition()` + `orbit::ease` (cubic-bezier 0.2,0,0,1) | yes (screenshot mid-animation) |
| Mod+[ / ] width presets 45/65/85 % with vertical centring | ✅ | `Size::step` | yes |
| Mod+F to 100 % minus the HUD bars; again restores the exact preset | ✅ | `Size::toggle_full` | yes |
| Rendered via wgpu | ◐ | Smithay's GLES renderer with custom pixel shaders. wgpu cannot share Smithay's DRM/GBM buffers without a second GPU context and a copy per frame. | — |
| WGSL shaders | ◐ | The same effects are written in GLSL ES (`shaders.rs`), the shader language Smithay's renderer accepts. | yes |
| Active: obsidian #0A0B0E at 85 %, blur, specular gloss, 1 px amber border | ◐ | Plate shader. The backdrop is procedural, so its blur is computed analytically (a softened copy of the grid) instead of a 2-pass Dual-Kawase over sampled pixels. Nothing else is ever behind the centre window, because ring windows are not drawn from their buffers. | yes |
| Inactive: rendering paused, wireframe box at 40 %, title + PID + ASCII sparkline | ✅ | Ring clients get no frame callbacks (paused). The node shader draws the wireframe; the label shows title, PID, RSS and a CPU sparkline sampled every 2 s. | yes |
| Perspective floor grid, crimson + amber | ✅ | Backdrop shader, analytically anti-aliased | yes |
| Bezel HUD, top and bottom 28 px, stays visible in fullscreen | ✅ | `hud.rs` + render. Top: ring id, focused binary, NPU state, git sync. Bottom: battery % + discharge W, refresh, package °C + profile, DSP status, SSID + dBm + EAP/TLS state. | yes (fake sysfs test + screenshots) |

## 3. Power daemon (`shadow-powerd`)

| Requirement | Status | Where / how |
|---|---|---|
| AC detection via udev netlink | ◐ | `power_supply` is polled every 2 s (std-only daemon, no netlink socket). Resume and suspend are covered by the sleep hook. |
| 120 → 60 Hz on battery via a DRM atomic mode change | ✅ | The compositor's `udev::apply_refresh` calls `DrmOutput::use_mode`. `[display] refresh_ac / refresh_battery`; policy `auto` or `powerd`. |
| ACPI platform_profile quiet/cool/balanced/performance | ✅ | Carried over from the Dell layer, which resolves profile names per firmware |
| RAPL PL1/PL2 (µW) | ✅ | `rapl.rs`. Clamps down (9 W cool/low-power, 12 W quiet) and restores the firmware values, which are never exceeded. Unit-tested. |
| EPP power / balance_power / performance | ✅ | Existing EPP logic |
| 32-byte telemetry record over a Unix socket | ✅ | `compositor/src/telemetry.rs` encodes, `powerd/src/predict.rs` decodes. Same field order as the spec. Unit-tested on both sides. |
| Quantized ONNX model on the NPU (`/dev/accel/accel0`) | ✗ | The predictor is a per-app-class statistical table of observed CPU use, which runs in microseconds on the CPU. `shadow-powerd npu` reports NPU readiness, and the HUD shows `backend=cpu-statistical`. Waking the NPU for a lookup this small would cost more energy than it saves. |
| Feed-forward: light work → PL1 9 W + EPP power + cool; heavy → pre-elevate | ✅ | `daemon.rs` `telemetry()` sets a time-limited hint; RAPL follows the mode |
| `shadow-audiod`: SOF/SoundWire probe, topology check, UCM unmute, re-check on resume | ✅ | `audio.rs` (from the Dell layer), run as `shadow-audiod` at boot and from the sleep hook. Writes `audio.state` for the HUD. |
| 20 % RAM ceiling via cgroup v2 (`memory.high` 16 %, `memory.max` 20 %) | ◐ | Split between the system `shadow.slice` (daemons: 3 % high / 4 % max, also written from MemTotal by powerd) and the user `shadow.slice` (compositor + TUI apps: 13 % / 16 %), together ≤ 20 %. User programs go to `app.slice`. Commands typed in shadow-term stay outside the ceiling unless `[memory] shell_in_slice = true`, because a compile capped at 16 % of RAM would defeat the "80 % for user work" goal. `shadow-powerd memory` reports both. |
| At memory.high: GPU cache eviction, Lua GC, NPU compaction | ◐ | The compositor watches `memory.events` and evicts its text cache and runs a Lua GC |
| zram compression of scrollback / NPU cache | ✗ | Not implemented |

## 4. Wi-Fi (`shadow-netd`)

| Requirement | Status | Where / how |
|---|---|---|
| UC-Secure / eduroam 802.1X profiles via NetworkManager or iwd | ✅ | `core/src/wifi.rs`: NM profiles, plus `shadow wifi iwd-provision` writes `/var/lib/iwd/*.8021x` (PEAP, MSCHAPv2, CA cert, ServerDomainMask) |
| Immutable trust store in `/etc/shadow-tui/certs/` | ◐ | Used automatically when `ca-bundle.pem` is present, otherwise the system bundle. No institution CA files ship with the package, because they can't be verified from here. |
| `SECLEVEL=1` scoped to the supplicant | ✅ | Per-connection `802-1x.openssl-ciphers` (only that network). `SECLEVEL=0` is the next step if 1 is not enough. |
| TLS alert → toggle PEAPv0/v1 + reload CA | ✅ | Fix `peap-v1` |
| Association denied / DHCP timeout → MAC randomisation off, `ip neigh flush`, renew | ✅ | Fixes `dhcp-mac`, `dhcp-renew` |
| Connected, no internet → DoT/DoH fallback or gateway ping | ✅ | `net_recover()`: gateway ping, then ARP flush + renew, or switch to DNS-over-TLS (Quad9/Cloudflare) through resolved |
| Live EAP/TLS state for the HUD | ✅ | `publish()` writes `/run/shadow/net.state` |
| Same self-repair ladder as before | ✅ | Parity runs against the earlier version: identical except the intended extra steps |

## 5. Self-learning UI and self-repairing code

| Requirement | Status | Where / how |
|---|---|---|
| Immutable Rust core + mutable LuaJIT modules in `/var/lib/shadow-tui/modules` | ✅ | `lua/` (sandbox), `compositor/src/luahost.rs` (API: rule, hud, on, bind, spawn, action, notify, state) |
| Drawer ranking and network repair in Lua | ◐ | Drawer ranking and the repair ladder stay in Rust (sakura is std-only; the repair ladder is the tested code path). Lua covers window rules, HUD widgets, key macros and focus events. |
| Trigger only on explicit errors | ✅ | `healer.rs` reads `lua-errors.log` (runtime exceptions, refused or broken modules) |
| Confidence gate: no human edit in the last 30 s | ◐ | Modification time of the module (not inotify) |
| Local SLM patch, sandbox dry run, git commit or revert to last known-good | ✅ | Local or LAN endpoint only. Dry run: `unshare --net` + `setpriv` to nobody + LuaJIT sandbox with a time budget. |
| Transactional rollback, Mod+Shift+Backspace | ✅ | `shadow-healer revert` (only reverts the healer's own commits). Git CLI rather than libgit2. |
| Automatic commit | ◐ | Mode `auto` exists. The default is `approve`, which keeps your earlier decision that fixes need approval. Core binaries are never patched by the healer. |

## 6. Version independence and sync

| Requirement | Status | Where / how |
|---|---|---|
| Self-contained hashed `/opt/shadow-tui` | ◐ | Binaries, shaders (built in) and data live in `/opt/shadow-tui`, with `MANIFEST.sha512` and DT_RPATH=`/opt/shadow-tui/lib`. The input, keyboard and seat libraries are bundled. Not Nix. Mesa, libdrm, glibc, PAM and libudev stay the system's because they must match the kernel. ONNX libraries and SOF topologies are not bundled. |
| `~/.config/shadow-state`: config.toml, npu_weights.bin, healed_patches/, secrets.age | ✅ | `core/src/sync.rs`. `npu_weights.bin` bundles the learned tables (powerd classes, launcher time-of-day). |
| `shadow-sync push` to a private GitHub repo | ✅ | `init --github` creates it `--private`. A secret scanner blocks plaintext secrets. |
| `shadow-sync bootstrap URL` | ✅ | Clones, decrypts `secrets.age` (age with the SSH key or a passphrase), recreates Wi-Fi profiles, rebuilds the runtime from the recorded source, installs packages and services, restores state |

## 7. Applications

| App | Status | Notes |
|---|---|---|
| shadow-greet | ✅ | `shadow-compositor --greeter` (greetd). Same renderer and backdrop as the session; its last frame equals the session's first frame. The text greeter and agreety are fallbacks. Disk/keychain unlock and direct DRM-master handoff ✗: greetd starts the session after the greeter exits, and the matching frames hide the switch. |
| shadow-term | ◐ | Existing terminal, re-themed amber/cyan. Not alacritty_terminal, no sixel. |
| shadow-net | ✅ | `sakura net`: networks, device fix, campus setup, repair. Shows EAP state through the HUD rather than per handshake phase. |
| shadow-config | ✅ | `sakura settings`: live matrix over config.toml, applied with `shadowctl config-reload`, rolled back if rejected, mouse support |
| shadow-files | ◐ | Own yazi-like file manager (not a yazi build) |
| shadow-launch | ◐ | Ranks by frequency and time of day (CPU statistics). No live RAM/W estimates per app. |

## 8–9. Security and compliance (optional `shadow-tui-sec` package)

Following your instruction, the security and compliance parts are a separate, optional command and package. Nothing is active until `shadow-sec enable MODULE`.

| Requirement | Status | `shadow-sec` module / note |
|---|---|---|
| Keystroke dynamics on the NPU | ✗ | — |
| eBPF syscall telemetry, 5 ms severing | ◐ | `ids`: /proc + audit-based detection of reverse shells, memfd (fileless) execution and foreign ptrace → SEC-ALERT in the HUD, optional SIGSTOP. No eBPF programs and no timing guarantee. |
| Automatic seccomp/Landlock synthesis | ◐ | `sandbox`: static systemd sandboxing for the daemons (not synthesised) |
| Prompt-injection defence for the healer | ✅ | Built into the core: `sanitize_external`, data fencing, AST capability whitelist, sandbox dry run (§5). Not optional. |
| fs-verity + TPM-signed weights + golden image | ◐ | `verity`: fs-verity (SHA-512) on `/opt/shadow-tui` binaries, checked by `verify`. No TPM signing and no golden image. |
| Per-client Wayland isolation | ✅ | Core: keys go only to the centre client, selections only to the focused client, and no screencopy, input-capture or clipboard-manager protocols exist. No "portal tokens". |
| Landlock + seccomp per daemon, CFI, hardened_malloc | ◐ | `sandbox` (systemd). No CFI or hardened_malloc. |
| Evil-twin / downgrade detection | ◐ | Core: strict CA + domain-suffix validation, and EAP downgrade fixes are opt-in (`allow_unvalidated`). No BSSID/OUI anomaly tracking. |
| XDP micro-firewall | ◐ | `zones`: nftables with scan signatures dropped at prerouting/raw (not XDP) |
| USBGuard + Thunderbolt DMA lockdown while locked | ◐ | `usb`: kernel `authorized_default=0` while the session is locked; devices plugged in meanwhile are authorised after unlock. Thunderbolt security level is reported, not changed. |
| FIPS 140-3 modules, AES-256 / SHA-384 only | ✗ | Reported as not implemented. LUKS2 cipher reported by `verify`. |
| PQC (ML-KEM + ML-DSA) for sync/SSH/TLS | ◐ | `crypto`: OpenSSH ML-KEM-768/X25519 hybrid key exchange preferred, TLS 1.3 for git. No ML-DSA. |
| Network zones PAZ/OZ/MZ | ◐ | `zones`: host firewall + no network for powerd/healer (MZ). No netns per zone. |
| Smart card / FIDO2 MFA | ◐ | PAM-based: add `pam_u2f` / `pam_pkcs11` to `/etc/pam.d/shadow-lock` and greetd (documented in `report`) |
| Session lock, drop keys | ◐ | Core lock screen. Keys are not dropped from the page cache. |
| auditd AU-2/AU-9 + sealed logs | ✅ | `audit`, `fss` (journald Forward Secure Sealing; `verify` checks) |
| UEFI Secure Boot with own keys, UKI, PCR-sealed LUKS | ◐ | Status reported by `verify`. Key enrolment is left to sbctl/systemd-ukify. |
| Encrypted swap, no hibernation | ◐ | Reported by `verify` |
| Zeroize (key chord + token, < 2 s) | ◐ | `shadow-sec zeroize --plan / --execute`: typed confirmation ×2, optional USB token serial, `cryptsetup erase` of every LUKS2 header + shred of learned weights. Not wired to a greeter key chord. |
| Compliance claims (CAF/DND/CSE) | — | `shadow-sec report` maps controls to ITSG-33 families as a self-assessment and states that this is not a certification |

## What was verified, and what still needs the laptop

Tested in the container:

- unit tests in every crate;
- the compositor running nested inside a headless compositor, driven with `shadowctl` and screenshotted: ring, animation, presets, fullscreen, HUD, lock, greeter, handoff;
- the shadow-config TUI in a pseudo-terminal;
- Wi-Fi repair scenarios and device faults against a fake NetworkManager, compared with the earlier version;
- the healer with a fake local model, including a hostile reply;
- powerd prediction and RAPL against a fake sysfs.

Not tested, because it needs the Dell 14 Plus:

- the DRM/KMS backend and the 120↔60 Hz mode change;
- libinput;
- the greeter at boot under greetd, and the visual handoff;
- PAM unlock;
- RAPL, platform_profile and EPP on Lunar Lake;
- SOF audio;
- the NPU;
- real 802.1X networks;
- cgroup limits under systemd;
- every `shadow-sec` module, all of which change a real system.
