# Shadow TUI Restricted — local PKGBUILD (build from this directory: makepkg -si, or ./install.sh)
#
# Split package:
#   shadow-tui-restricted   the desktop on niri: vertex hub (shadow-hub; side bar optional), dedicated 2D apps
#                           (shadow-launch, -net, -config, -files, -lock, -term), session services
#                           (shadow-sessiond, shadow-displayd), static console login (shadow-greet),
#                           shadow-powerd (7-day learning schedule), shadow-netd, shadow-healer, shadow-sync
#   shadow-tui-sec          OPTIONAL security subsystem (shadow-sec): nothing changes until enabled
#
# Isolated runtime: everything lives in /opt/shadow-tui (bin, lib, share) with commands linked into
# /usr/bin. Binaries carry DT_RPATH=/opt/shadow-tui/lib, and the keyboard library is bundled there.
# Every Shadow app renders on the CPU into shared memory (no GPU API in the apps); niri does the
# compositing with OpenGL ES. Mesa, libdrm, glibc and PAM stay the system's.
# niri itself comes from Arch (extra/niri): Shadow talks to it only through `niri msg` and its
# config file, so a niri update can't break Shadow's binaries.
pkgbase=shadow-tui-restricted
pkgname=(shadow-tui-restricted shadow-tui-sec)
pkgver=8.3.0
pkgrel=1
pkgdesc="Shadow TUI Restricted: keyboard-first tactical desktop for Arch (Dell 14 Plus / Lunar Lake) on niri — vertex wireframe hub, dedicated Rust 2D apps, self-resolving scaling across monitors, static console login, power/NPU daemon with a weekly learning schedule, Wi-Fi self-repair, sandboxed self-healing Lua layer, GitHub state sync"
arch=('x86_64')
url="https://github.com/namidelacruz/shadow-tui-restricted"
license=('MIT')
makedepends=(cargo clang git pkgconf libinput libdrm seatd wayland)
options=(!debug !lto)
_src="$startdir"
_bundle=${SHADOW_BUNDLE:-1}

prepare() {
  # fail-safe: no building from a folder like "shadow-tui-restricted (1)" (see install.sh)
  case "$_src" in *[!A-Za-z0-9._/+-]*)
    error "this folder can't be built from: $_src"
    error "rename it so its path has only letters, digits and . _ - + /  (e.g. drop \" (1)\")"
    return 1 ;;
  esac
  cd "$_src"
  export RUSTUP_TOOLCHAIN=stable
  cargo fetch --locked --target "$(rustc -vV | sed -n 's/host: //p')"
}

build() {
  cd "$_src"
  export RUSTUP_TOOLCHAIN=stable CARGO_TARGET_DIR="$srcdir/target"
  export RUSTFLAGS="-C link-arg=-Wl,-rpath,/opt/shadow-tui/lib -C link-arg=-Wl,--disable-new-dtags"
  cargo build --frozen --release --workspace
}

check() {
  cd "$_src"
  export RUSTUP_TOOLCHAIN=stable CARGO_TARGET_DIR="$srcdir/target"
  cargo test --frozen --release --workspace
  for m in "$_src"/modules/*.lua; do "$srcdir/target/release/shadow" healer check "$m"; done
  if command -v niri >/dev/null; then
    "$srcdir/target/release/shadow-niri-gen" > "$srcdir/niri-check.kdl" && niri validate -c "$srcdir/niri-check.kdl"
  fi
  for s in wifi-helper wifi-device-helper greeter-wrapper shadow-session shadow-tui.sleep; do sh -n "$_src/system/$s"; done
  sh -n "$_src/packaging/install-files.sh" && sh -n "$_src/packaging/post-install.sh"
}

_bundle_libs() {
  # keyboard library (the apps' only non-libc runtime dependency); never mesa/glibc/pam
  local lib p
  for lib in libxkbcommon.so.0; do
    p=$(ldconfig -p 2>/dev/null | awk -v l="$lib" '$1==l {print $NF; exit}')
    [ -n "$p" ] && install -Dm755 "$(readlink -f "$p")" "$pkgdir/opt/shadow-tui/lib/$lib"
  done
}

package_shadow-tui-restricted() {
  depends=(niri mesa libinput libdrm seatd libxkbcommon pam systemd fontconfig ttf-jetbrains-mono-nerd
           networkmanager wpa_supplicant iproute2 util-linux kmod git openssh polkit wireplumber brightnessctl)
  optdepends=('xwayland-satellite: X11 apps on niri and shadow-compositor' 'xorg-xwayland: X11 apps'
    
    'mate-polkit: password prompts for privileged actions (any polkit agent works; ./install.sh installs one)'
    'greetd: the Shadow login screen (shadow greeter enable)'
    'terminus-font: login screen text sized to the panel (ter-v24n on 2560x1600)'
    'xdg-desktop-portal-gnome: screen sharing and file pickers under niri'
    'noto-fonts-cjk: CJK text in the Shadow apps (loaded only when needed)'
    'age: encrypted Wi-Fi credentials in the synced state (secrets.age)'
    'github-cli: shadow-sync init --github (private repository)'
    'iwd: alternative Wi-Fi backend (shadow wifi iwd-provision)'
    'linux-lts: fallback kernel kept by the kernel guard'
    'sof-firmware: Intel Lunar Lake audio firmware'
    'alsa-ucm-conf: audio profiles for SoundWire laptops'
    'openvino-model-server: local model on the NPU for shadow ai / shadow-healer'
    'mako: desktop notifications (layer-shell)'
    'libnotify: notify-send for Lua macro notifications'
    'shadow-tui-sec: the optional security subsystem'
  )
  provides=(shadow-network)
  conflicts=(shadow-network netdive)
  replaces=(shadow-network netdive)
  install=shadow-tui-restricted.install
  backup=(etc/shadow-tui/greeter.conf etc/shadow-tui/netd.toml etc/shadow-tui/healer.toml)

  # file layout shared with the RPM: packaging/install-files.sh
  SHADOW_VERSION="$pkgver-$pkgrel" sh "$_src/packaging/install-files.sh" "$pkgdir" arch "$srcdir/target" "$_src" main
  [ "$_bundle" = 1 ] && _bundle_libs
  # the manifest must include bundled libraries
  (cd "$pkgdir/opt/shadow-tui" && find bin lib -type f -exec sha512sum {} + | sort -k2) > "$pkgdir/opt/shadow-tui/share/MANIFEST.sha512"
}

package_shadow-tui-sec() {
  pkgdesc="Shadow TUI Restricted optional security subsystem (shadow-sec): USB lockdown while locked, nftables zones, daemon sandboxing, audit rules, sealed logs, behaviour monitor, fs-verity, PQC SSH, zeroize"
  depends=(shadow-tui-restricted)
  optdepends=('nftables: shadow-sec enable zones' 'audit: shadow-sec enable audit'
              'fsverity-utils: shadow-sec enable verity' 'openssh: post-quantum hybrid key exchange (shadow-sec enable crypto)'
              'cryptsetup: shadow-sec zeroize')
  sh "$_src/packaging/install-files.sh" "$pkgdir" arch "$srcdir/target" "$_src" sec
}
