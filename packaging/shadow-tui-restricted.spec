# Shadow TUI Restricted — RPM for Fedora (43+) and RHEL / CentOS Stream / Rocky / Alma (9, 10).
# Built in place from a checkout:  packaging/build-rpm.sh OUTDIR   (or ./install.sh, which also
# installs the dependencies). The file layout is shared with the Arch PKGBUILD through
# packaging/install-files.sh.
%{!?shadow_src: %global shadow_src %{_sourcedir}/shadow-tui-restricted}
%global debug_package %{nil}
%global _build_id_links none
%global __brp_check_rpaths %{nil}

Name:           shadow-tui-restricted
Version:        7.2.0
Release:        1%{?dist}
Summary:        Keyboard-first tactical desktop on niri: side bar, Rust 2D apps, power/NPU daemon, Wi-Fi self-repair
License:        MIT
URL:            https://github.com/namidelacruz/shadow-tui-restricted
ExclusiveArch:  x86_64 aarch64

BuildRequires:  gcc gcc-c++ make git pkgconf-pkg-config pam-devel libxkbcommon-devel
# cargo/rustc >= 1.89: the distribution's or rustup's (install.sh checks and provides one)

Requires:       mesa-dri-drivers mesa-libEGL libxkbcommon pam systemd fontconfig
# the apps draw all text themselves: a monospace font must exist
Requires:       dejavu-sans-mono-fonts
Requires:       NetworkManager wpa_supplicant iproute util-linux kmod git-core wireplumber
# niri is the compositor: Fedora's own package, the yalter/niri COPR on EL10, or built from
# source into /usr/local on EL9 by install.sh — hence a weak dependency
Recommends:     niri
Recommends:     greetd terminus-fonts-console brightnessctl xdg-desktop-portal-gnome libnotify
Recommends:     alsa-sof-firmware alsa-ucm google-noto-sans-cjk-fonts
Suggests:       age gh iwd
Provides:       shadow-network = %{version}-%{release}
Obsoletes:      shadow-network < 7

%description
Shadow TUI Restricted (CR-SPEC-SHADOW-001) on niri: a floating side bar, dedicated Rust 2D
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
cd %{shadow_src}
export CARGO_TARGET_DIR=%{?shadow_target}%{!?shadow_target:%{_builddir}/target}
cargo build --release --locked --workspace

%check
cd %{shadow_src}
export CARGO_TARGET_DIR=%{?shadow_target}%{!?shadow_target:%{_builddir}/target}
cargo test --release --locked --workspace
for m in modules/*.lua; do "$CARGO_TARGET_DIR/release/shadow" healer check "$m"; done
for s in system/wifi-helper system/wifi-device-helper system/greeter-wrapper system/shadow-session system/shadow-tui.sleep packaging/install-files.sh packaging/post-install.sh; do sh -n "$s"; done
if command -v niri >/dev/null; then
  "$CARGO_TARGET_DIR/release/shadow-niri-gen" > %{_builddir}/niri-check.kdl && niri validate -c %{_builddir}/niri-check.kdl
fi

%install
export CARGO_TARGET_DIR=%{?shadow_target}%{!?shadow_target:%{_builddir}/target}
SHADOW_VERSION=%{version}-%{release} sh %{shadow_src}/packaging/install-files.sh %{buildroot} rpm "$CARGO_TARGET_DIR" %{shadow_src} main
sh %{shadow_src}/packaging/install-files.sh %{buildroot} rpm "$CARGO_TARGET_DIR" %{shadow_src} sec

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
/usr/lib/systemd/system/shadow.slice
/usr/lib/systemd/user/shadow.slice
/usr/lib/systemd/user/shadow-sync.service
/usr/lib/systemd/user/shadow-sync.timer
/usr/lib/systemd/system-sleep/shadow-tui
/usr/share/polkit-1/actions/org.shadownetwork.wifi.policy
/usr/share/wayland-sessions/shadow-tui.desktop
/usr/share/applications/shadow-*.desktop
%config(noreplace) /etc/pam.d/shadow-lock
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
* Sat Oct 31 2026 Shadow TUI Restricted <noreply@shadow-tui.local> - 7.2.0-1
- shadow-update: on-demand updates that replace only changed files, with backup and undo
- launcher opens as a normal opaque window; mouse support in all apps
* Sat Oct 31 2026 Shadow TUI Restricted <noreply@shadow-tui.local> - 7.1.0-1
- Fedora / RHEL packaging; installer for Arch, Fedora and RHEL-family systems
