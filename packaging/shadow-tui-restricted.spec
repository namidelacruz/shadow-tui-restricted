# Shadow TUI Restricted — RPM for Fedora (43+) and RHEL / CentOS Stream / Rocky / Alma (9, 10).
# Built in place from a checkout:  packaging/build-rpm.sh OUTDIR   (or ./install.sh, which also
# installs the dependencies). The file layout is shared with the Arch PKGBUILD through
# packaging/install-files.sh.
%{!?shadow_src: %global shadow_src %{_sourcedir}/shadow-tui-restricted}
%global debug_package %{nil}
%global _build_id_links none
%global __brp_check_rpaths %{nil}
# Shadow's own compositor (preview). Leave it out with  --without compositor  — the niri session
# does not need it; packaging/build-rpm.sh does this by itself when only the compositor fails to build
%bcond_without compositor

Name:           shadow-tui-restricted
Version:        9.0.0
Release:        1%{?dist}
Summary:        Keyboard-first tactical desktop on niri: vertex hub, Rust 2D apps, power/NPU daemon, Wi-Fi self-repair
License:        MIT
URL:            https://github.com/namidelacruz/shadow-tui-restricted
ExclusiveArch:  x86_64 aarch64

BuildRequires:  gcc gcc-c++ make git pkgconf-pkg-config pam-devel libxkbcommon-devel
# shadow-compositor (Shadow's own compositor): DRM/KMS, input, seat
BuildRequires:  systemd-devel libinput-devel libseat-devel libdrm-devel mesa-libgbm-devel mesa-libEGL-devel wayland-devel
# cargo/rustc >= 1.89: the distribution's or rustup's (install.sh checks and provides one)

Requires:       mesa-dri-drivers mesa-libEGL mesa-libgbm libinput libseat libxkbcommon pam systemd fontconfig
# the apps draw all text themselves: a monospace font must exist
Requires:       dejavu-sans-mono-fonts
Requires:       NetworkManager wpa_supplicant iproute util-linux kmod git-core openssh-clients wireplumber polkit
# niri is the compositor: Fedora's own package, the yalter/niri COPR on EL10, or built from
# source into /usr/local on EL9 by install.sh — hence a weak dependency
Recommends:     niri
# a polkit authentication agent: without one, actions that need a password fail instead of asking
Recommends:     (mate-polkit or lxqt-policykit or polkit-gnome or lxpolkit)
# X11 apps (older AppImages, Electron, Steam, Wine) on Shadow's own compositor; niri uses it too
Recommends:     xwayland-satellite xorg-x11-server-Xwayland
Recommends:     greetd terminus-fonts-console brightnessctl xdg-desktop-portal-gnome libnotify
Recommends:     alsa-sof-firmware alsa-ucm google-noto-sans-cjk-fonts
Suggests:       age gh iwd
Provides:       shadow-network = %{version}-%{release}
Obsoletes:      shadow-network < 7

%description
Shadow TUI Restricted (CR-SPEC-SHADOW-001) on niri: a vertex wireframe hub (3D field that follows the cursor, central hub,
floating widgets; the side bar stays optional), dedicated Rust 2D
applications (launcher, network, settings, files, lock, terminal), self-resolving per-monitor
scaling with text-size correction, a static console login for greetd, a power daemon with a
weekly learning schedule, Wi-Fi self-repair for 802.1X campus networks, a sandboxed
self-correcting Lua layer and GitHub state sync. Everything lives in /opt/shadow-tui.

%package -n shadow-tui-sec
Summary:        Optional security subsystem for Shadow TUI Restricted (shadow-sec)
Requires:       %{name} = %{version}-%{release}
Recommends:     nftables audit fsverity-utils openssh cryptsetup

%description -n shadow-tui-sec
shadow-sec: USB lockdown while locked, nftables zones, daemon sandboxing, audit rules, sealed
logs, behaviour monitor, fs-verity, PQC SSH, zeroize. Nothing is active until you enable a module.

%prep
# built in place from %{shadow_src}

%build
cd "%{shadow_src}"
export CARGO_TARGET_DIR="%{?shadow_target}%{!?shadow_target:%{_builddir}/target}"
# Fedora's %%build exports RUSTFLAGS meant for its own Rust packages (codegen-units=1, full debug
# info). With no debuginfo package here that only multiplies memory and build time — enough to run a
# laptop out of memory on the compositor — so the workspace's release profile decides instead.
unset RUSTFLAGS
# one compile job per ~2 GB of available memory (SHADOW_BUILD_JOBS overrides)
jobs=${SHADOW_BUILD_JOBS:-$(awk -v n="$(nproc)" '/^MemAvailable/{j=int($2/2097152); if(j<1)j=1; if(j>n)j=n; print j}' /proc/meminfo)}
cargo build --release --locked --workspace -j "$jobs" %{!?with_compositor:--exclude shadow-compositor}

%check
cd "%{shadow_src}"
export CARGO_TARGET_DIR="%{?shadow_target}%{!?shadow_target:%{_builddir}/target}"
# Fedora's %%build exports RUSTFLAGS meant for its own Rust packages (codegen-units=1, full debug
# info). With no debuginfo package here that only multiplies memory and build time — enough to run a
# laptop out of memory on the compositor — so the workspace's release profile decides instead.
unset RUSTFLAGS
# one compile job per ~2 GB of available memory (SHADOW_BUILD_JOBS overrides)
jobs=${SHADOW_BUILD_JOBS:-$(awk -v n="$(nproc)" '/^MemAvailable/{j=int($2/2097152); if(j<1)j=1; if(j>n)j=n; print j}' /proc/meminfo)}
cargo test --release --locked --workspace -j "$jobs" %{!?with_compositor:--exclude shadow-compositor}
for m in modules/*.lua; do "$CARGO_TARGET_DIR/release/shadow" healer check "$m"; done
for s in system/wifi-helper system/wifi-device-helper system/greeter-wrapper system/shadow-session system/shadow-tui.sleep packaging/install-files.sh packaging/post-install.sh; do sh -n "$s"; done
if command -v niri >/dev/null; then
  "$CARGO_TARGET_DIR/release/shadow-niri-gen" > "%{_builddir}/niri-check.kdl" && niri validate -c "%{_builddir}/niri-check.kdl"
fi

%install
export CARGO_TARGET_DIR="%{?shadow_target}%{!?shadow_target:%{_builddir}/target}"
SHADOW_VERSION=%{version}-%{release} sh "%{shadow_src}/packaging/install-files.sh" "%{buildroot}" rpm "$CARGO_TARGET_DIR" "%{shadow_src}" main
sh "%{shadow_src}/packaging/install-files.sh" "%{buildroot}" rpm "$CARGO_TARGET_DIR" "%{shadow_src}" sec

%post
if [ "$1" -eq 1 ]; then /opt/shadow-tui/lib/post-install.sh install; else /opt/shadow-tui/lib/post-install.sh upgrade; fi

%preun
if [ "$1" -eq 0 ]; then /opt/shadow-tui/lib/post-install.sh pre-remove; fi

%postun
if [ "$1" -eq 0 ]; then
  systemctl daemon-reload >/dev/null 2>&1 || true
  echo "==> Your state (~/.config/shadow-state) and /var/lib/shadow-tui/modules are kept."
fi

%post -n shadow-tui-sec
systemctl daemon-reload >/dev/null 2>&1 || true

%files
%license /usr/share/licenses/shadow-tui-restricted/LICENSE
%dir /opt/shadow-tui
%dir /opt/shadow-tui/bin
/opt/shadow-tui/bin/shadow*
%exclude /opt/shadow-tui/bin/shadow-sec
/opt/shadow-tui/lib
/opt/shadow-tui/share
/usr/bin/shadow*
%exclude /usr/bin/shadow-sec
/usr/lib/systemd/system/shadow-powerd.service
/usr/lib/systemd/system/shadow-audiod.service
/usr/lib/systemd/system/shadow-netd.service
/usr/lib/systemd/system/shadow-healer.service
/usr/lib/systemd/system/shadow-wifi-device.service
/usr/lib/systemd/system/shadow-update-recover.service
/usr/lib/systemd/system/shadow.slice
/usr/lib/systemd/user/shadow.slice
/usr/lib/systemd/user/shadow-sync.service
/usr/lib/systemd/user/shadow-sync.timer
/usr/lib/systemd/user/shadow-update-auto.service
/usr/lib/systemd/user/shadow-update-auto.timer
/usr/lib/systemd/user/timers.target.wants/shadow-update-auto.timer
/usr/lib/systemd/system-sleep/shadow-tui
/usr/share/polkit-1/actions/org.shadownetwork.wifi.policy
/usr/share/wayland-sessions/shadow-tui.desktop
%if %{with compositor}
/usr/share/wayland-sessions/shadow-tui-own.desktop
%endif
/usr/share/applications/shadow-*.desktop
%config(noreplace) /etc/pam.d/shadow-lock
%config(noreplace) /etc/pam.d/shadow-passtest
%dir /etc/shadow-tui
%config(noreplace) /etc/shadow-tui/netd.toml
%config(noreplace) /etc/shadow-tui/healer.toml
%config(noreplace) /etc/shadow-tui/greeter.conf
%dir /var/lib/shadow-tui
%dir /var/lib/shadow-powerd
%dir /var/cache/shadow-tui-greeter

%files -n shadow-tui-sec
/opt/shadow-tui/bin/shadow-sec
/usr/bin/shadow-sec
/usr/lib/systemd/system/shadow-sec.service

%changelog
* Thu Nov 05 2026 Shadow TUI Restricted <noreply@shadow-tui.local> - 9.0.0-1
- the glyph desktop on Shadow's own compositor (now the default session; niri stays selectable):
  gold line work on Vitruvian proportions, edge widget columns (shadow-glyph) with the side bar's
  quick settings merged in, apps tiling between them, minimised apps as square tiles in a
  scrollable strip / grid, the hidden hub (Mod+Space)
- kinetic unfolding / tessellation for windows opening, closing, minimising and restoring, from
  where they came from (a column row, a grid tile, the hub)
- contour-line background with travelling light (not mouse-reactive; small redraw regions,
  slower on battery, none while fullscreen)
- Shadow Glyph: an original display typeface; an original glyph icon set
- settings, fix center, default apps, config and crash log as a web app (PWA) over a local API
  (shadow-webd: 127.0.0.1, per-session key, Host / Origin checks); shadow-settings opens it
- login screen: boot animation, the desktop switch (other installed desktops too), TERMINAL UI
  emergency button, suspend / restart / shut down, mouse support; logout animation
- removed: the 8.x vertex hub (shadow-hub)

* Mon Nov 02 2026 Shadow TUI Restricted <noreply@shadow-tui.local> - 8.3.0-1
- shadow-compositor: full mouse and touchpad support — the hub, menus, windows and popups, click a
  ring node to bring it forward, Mod+scroll turns the ring; its own cursor shapes (cursor-shape-v1
  and client cursors), hidden while typing; tap to click, natural scroll, pointer speed in [input];
  pointer lock and relative motion for 3D apps; [input] pointer = "apps" keeps keyboard-first
- shadow-compositor: FIELD TRAINING, a hotkey tutorial on first login (Mod+Shift+/ or Mod+F1 again)
- shadow-compositor: every Shadow app works on it — DEFAULTS terminal / file manager, niri actions
  the apps send, output details and scale, an event stream for the bar and session daemon,
  xdg-activation, idle notification, X11 apps through xwayland-satellite (recommended package)
- install.sh --gui: an installer window — the full desktop, or apps only (per user in ~/.local,
  no session or system services); --apps "…", --remove-apps, a text menu without a display

* Mon Nov 02 2026 Shadow TUI Restricted <noreply@shadow-tui.local> - 8.2.2-1
- hub: the core no longer shows the time (the clock widget does); it reads SHADOW / MENU
  (CLOSE while the menu is open)

* Mon Nov 02 2026 Shadow TUI Restricted <noreply@shadow-tui.local> - 8.2.1-1
- fail-safe: install.sh, build-rpm.sh and the PKGBUILD refuse a checkout folder whose path has
  spaces, brackets or other shell characters (a second download: "(1)", "- Copy") and print the
  exact command to rename it; nothing is changed. --check and --uninstall still work from anywhere
- build: paths quoted in the spec, so a checkout folder with spaces or brackets in its name
  (e.g. "shadow-tui-restricted (1)") no longer stops %%build with a shell syntax error
- build: Fedora's RUSTFLAGS no longer applied (full debug info, one codegen unit); compile jobs
  follow available memory; Shadow's own compositor can be left out (--without compositor)
- build-rpm.sh: full log in ~/.local/state/shadow-tui/build-rpm.log, the compiler's error shown on
  failure, retried with one job when out of memory, or without the compositor preview when only it fails
* Mon Nov 02 2026 Shadow TUI Restricted <noreply@shadow-tui.local> - 8.2.0-1
- hub speed: layer compositing by spans, field quality (high / battery 30 fps / low half-res)
  chosen by power source and hardware, shorter easing, widget redraws only on change
- hardware profile (CPU, memory, GPU driver, battery, VM, WSL): quality, opaque windows and no
  blur without 3D rendering; any machine, not just the Dell 14 Plus
- shadow-craft: the Craft App Suite (ArtCraft) — install / update / remove / launch, SHA-256 checked
- crash reports: one plain-text file in ~/shadow-reports with personal details replaced
- shadow-compositor (preview): Shadow's own compositor hosts the hub and apps; niri-compatible
  answers; chosen at login or [session] compositor; fallback when niri won't start; WSL detection
* Mon Nov 02 2026 Shadow TUI Restricted <noreply@shadow-tui.local> - 8.1.1-1
- fix: shadow-hub crashed at start on a 2560x1600 panel at scale 1.5 (buffer 2561x1601 plus
  pool alignment padding); full-monitor surfaces now use the panel's own 2560x1600 pixels and
  apps always get exactly w x h pixels
* Mon Nov 02 2026 Shadow TUI Restricted <noreply@shadow-tui.local> - 8.1.0-1
- shadow-prefs: default file manager, terminal and background; crash log (interrupted update,
  refused niri config, hub fallback, waiting defaults, core dumps, panics, journal errors) with
  details and a saved report
- fix center DESKTOP page: solid background colour on/off, hub or side bar, hub check
- niri 26.04: blur behind the glass windows and the summoned hub, left out on older niri
- empty XDG_* directories count as unset
* Mon Nov 02 2026 Shadow TUI Restricted <noreply@shadow-tui.local> - 8.0.2-1
- shadow-hub switch / use-bar: change desktop in the running session and for later logins;
  the updater uses the same command
- app search: the app's own name outweighs matches in other apps' descriptions (hub, flyout,
  launcher)
* Mon Nov 02 2026 Shadow TUI Restricted <noreply@shadow-tui.local> - 8.0.1-1
- shadow-hub runs under a supervisor: restarts after a crash, falls back to the side bar and
  wallpaper after 3 crashes (notification + ~/.local/state/shadow-tui/hub.log); shadow-hub doctor
- shadow setup niri: compatibility config when niri rejects the newer optional lines, instead
  of keeping the whole old config (which kept the 7.x desktop)
- niri: transparent workspaces, so the wallpaper in the backdrop is visible outside the overview
- updates that bring the hub start it at once; panel clamp panic on narrow monitors
* Mon Nov 02 2026 Shadow TUI Restricted <noreply@shadow-tui.local> - 8.0.0-1
- vertex wireframe UI: chamfered translucent glass ([look] window_opacity, chamfer), wire edges,
  vertex nodes, segmented meters; launcher and updater stay opaque
- shadow-hub: 3D vertex field that follows the cursor, central hub with radial nodes and
  slide-open panels, draggable widgets wired to the core, Mod+Space summon overlay
- the side bar and the wallpaper program stay available ([hub] enabled = false, Mod+Shift+B)
* Sun Nov 01 2026 Shadow TUI Restricted <noreply@shadow-tui.local> - 7.5.0-1
- fix center (shadow-fix, shadow fix): app repairs + tasks, triage after updates, update console
- shadow-passfix: password / login repair with a lockout-free password test (shadow-passtest PAM)
- shadow-wallpaper: wallpapers per monitor + picker + generated default
- side bar POWER dropdown; smart charging with a firmware-write guard
* Sun Nov 01 2026 Shadow TUI Restricted <noreply@shadow-tui.local> - 7.4.1-1
- permissions: polkit agent started by the session (Requires polkit, Recommends an agent),
  brightness via logind, RAPL power published by shadow-powerd, authorise-to-kill in the task
  manager, clear messages for sudo/polkit refusals, wheel membership check
* Sun Nov 01 2026 Shadow TUI Restricted <noreply@shadow-tui.local> - 7.4.0-1
- side bar: APPS / SETTINGS / SYSTEM buttons with flyout panels, battery time left
- shadow-tasks task manager (CPU, memory, NPU, GPU, power, I/O, processes)
- fluid HUD restyle of every 2D app and the login console
- shadow-sec: per-module OS precheck; security console with password + confirmation
* Sat Oct 31 2026 Shadow TUI Restricted <noreply@shadow-tui.local> - 7.3.0-1
- required update source with a private-repository policy: read-only deploy key or credential
  helper (no stored tokens), pinned host keys, private check, no history rewrites, optional
  signed commits; isolated, hardened git for every update fetch; shadow update setup / policy
* Sat Oct 31 2026 Shadow TUI Restricted <noreply@shadow-tui.local> - 7.2.1-1
- update failsafe: no desktop session starts while an update writes files; interrupted updates
  are finished or reversed at boot (shadow-update-recover.service) before any login
* Sat Oct 31 2026 Shadow TUI Restricted <noreply@shadow-tui.local> - 7.2.0-1
- shadow-update: on-demand updates that replace only changed files, with backup and undo
- launcher opens as a normal opaque window; mouse support in all apps
* Sat Oct 31 2026 Shadow TUI Restricted <noreply@shadow-tui.local> - 7.1.0-1
- Fedora / RHEL packaging; installer for Arch, Fedora and RHEL-family systems
