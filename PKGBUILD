# Shadow TUI Restricted — local PKGBUILD (build from this directory: makepkg -si, or ./install.sh)
#
# Split package:
#   shadow-tui-restricted   the desktop: shadow-compositor (independent Smithay compositor, no niri),
#                           shadow-term, sakura apps, shadow-powerd, shadow-netd, shadow-healer,
#                           shadow-sync, greeter + lock screen
#   shadow-tui-sec          OPTIONAL security subsystem (shadow-sec): nothing changes until enabled
#
# Isolated runtime: everything lives in /opt/shadow-tui (bin, lib, share) with commands linked into
# /usr/bin. Binaries carry DT_RPATH=/opt/shadow-tui/lib, and the input/keyboard/seat libraries are
# bundled there, so Arch updates to those libraries can't break the session. Mesa, libdrm, glibc,
# PAM and libudev stay the system's: they must match the running kernel and system.
pkgbase=shadow-tui-restricted
pkgname=(shadow-tui-restricted shadow-tui-sec)
pkgver=6.0.0
pkgrel=1
pkgdesc="Shadow TUI Restricted: keyboard-first tactical desktop for Arch (Dell 14 Plus / Lunar Lake) — own Smithay compositor with an orbital ring, bezel HUD, greeter that blends into the session, power/NPU daemon, Wi-Fi self-repair, sandboxed self-healing Lua layer, GitHub state sync"
arch=('x86_64')
url="https://github.com/namidelacruz/shadow-tui-restricted"
license=('MIT')
makedepends=(cargo clang git pkgconf)
options=(!debug !lto)
_src="$startdir"
_bundle=${SHADOW_BUNDLE:-1}

prepare() {
  cd "$_src"
  export RUSTUP_TOOLCHAIN=stable
  cargo fetch --locked --target "$(rustc -vV | sed -n 's/host: //p')"
}

build() {
  cd "$_src"
  export RUSTUP_TOOLCHAIN=stable CARGO_TARGET_DIR="$srcdir/target"
  export RUSTFLAGS="-C link-arg=-Wl,-rpath,/opt/shadow-tui/lib -C link-arg=-Wl,--disable-new-dtags"
  cargo build --frozen --release --workspace
  # sakura: plain rustc, no crates — it has to work when everything else is broken
  rustc --edition 2021 -C opt-level=3 -C strip=symbols -C codegen-units=1 \
        -o "$srcdir/sakura" "$_src/tui/src/main.rs"
}

check() {
  cd "$_src"
  export RUSTUP_TOOLCHAIN=stable CARGO_TARGET_DIR="$srcdir/target"
  cargo test --frozen --release --workspace
  rustc --edition 2021 --test -o "$srcdir/sakura-tests" "$_src/tui/src/main.rs"
  "$srcdir/sakura-tests"
  "$srcdir/sakura" greeter --self-test --theme tactical >/dev/null
  for m in "$_src"/modules/*.lua; do "$srcdir/target/release/shadow" healer check "$m"; done
  for s in wifi-helper wifi-device-helper greeter-wrapper shadow-session shadow-tui.sleep; do sh -n "$_src/system/$s"; done
}

_bundle_libs() {
  # input + keyboard + seat stack; never mesa/libdrm/glibc/pam/libudev
  local lib p
  for lib in libinput.so.10 libevdev.so.2 libmtdev.so.1 libwacom.so.9 libgudev-1.0.so.0 libseat.so.1 libxkbcommon.so.0; do
    p=$(ldconfig -p 2>/dev/null | awk -v l="$lib" '$1==l {print $NF; exit}')
    [ -n "$p" ] && install -Dm755 "$(readlink -f "$p")" "$pkgdir/opt/shadow-tui/lib/$lib"
  done
}

package_shadow-tui-restricted() {
  depends=(mesa libinput seatd libxkbcommon pam systemd fontconfig ttf-jetbrains-mono-nerd
           networkmanager wpa_supplicant iproute2 util-linux kmod git wireplumber brightnessctl)
  optdepends=(
    'greetd: the Shadow login screen (shadow greeter enable)'
    'age: encrypted Wi-Fi credentials in the synced state (secrets.age)'
    'github-cli: shadow-sync init --github (private repository)'
    'iwd: alternative Wi-Fi backend (shadow wifi iwd-provision)'
    'linux-lts: fallback kernel kept by the kernel guard'
    'sof-firmware: Intel Lunar Lake audio firmware'
    'alsa-ucm-conf: audio profiles for SoundWire laptops'
    'openvino-model-server: local model on the NPU for shadow ai / shadow-healer'
    'mako: desktop notifications (layer-shell)'
    'shadow-tui-sec: the optional security subsystem'
  )
  provides=(shadow-network)
  conflicts=(shadow-network netdive)
  replaces=(shadow-network netdive)
  install=shadow-tui-restricted.install
  backup=(etc/shadow-tui/greeter.conf etc/shadow-tui/netd.toml etc/shadow-tui/healer.toml)

  local o="$pkgdir/opt/shadow-tui" b
  install -d "$o/bin" "$o/lib" "$o/share" "$pkgdir/usr/bin"
  for b in shadow-compositor shadowctl shadow-term shadow-powerd shadow; do
    install -Dm755 "$srcdir/target/release/$b" "$o/bin/$b"
  done
  install -Dm755 "$srcdir/sakura" "$o/bin/sakura"
  install -Dm755 "$_src/system/shadow-session" "$o/bin/shadow-session"
  ln -s shadow-powerd "$o/bin/shadow-audiod"
  for b in shadow-sync shadow-healer shadow-netd shadownet; do ln -s shadow "$o/bin/$b"; done
  for b in shadow-config shadow-net shadow-files shadow-launch; do ln -s sakura "$o/bin/$b"; done
  for b in "$o"/bin/*; do ln -s "/opt/shadow-tui/bin/$(basename "$b")" "$pkgdir/usr/bin/$(basename "$b")"; done

  install -Dm755 "$_src/system/greeter-wrapper"    "$o/lib/greeter-wrapper"
  install -Dm755 "$_src/system/wifi-helper"        "$o/lib/wifi-helper"
  install -Dm755 "$_src/system/wifi-device-helper" "$o/lib/wifi-device-helper"
  [ "$_bundle" = 1 ] && _bundle_libs

  cp -r "$_src/share/themes" "$_src/share/hw" "$_src/share/shell" "$o/share/"
  install -d "$o/share/modules" && install -m644 "$_src"/modules/*.lua "$o/share/modules/"
  install -Dm644 "$_src/README.md"          "$o/share/doc/README.md"
  install -Dm644 "$_src/docs/SPEC-TRACE.md" "$o/share/doc/SPEC-TRACE.md"
  install -Dm644 "$_src/LICENSE"            "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
  find "$o/share" -type f -exec chmod 644 {} +

  # services, slices, hooks
  local s=usr/lib/systemd/system u=usr/lib/systemd/user
  for f in shadow-powerd shadow-audiod shadow-netd shadow-healer shadow-wifi-device; do
    install -Dm644 "$_src/system/$f.service" "$pkgdir/$s/$f.service"
  done
  install -Dm644 "$_src/system/shadow.slice"        "$pkgdir/$s/shadow.slice"
  install -Dm644 "$_src/system/shadow.user.slice"   "$pkgdir/$u/shadow.slice"
  install -Dm644 "$_src/system/shadow-sync.service" "$pkgdir/$u/shadow-sync.service"
  install -Dm644 "$_src/system/shadow-sync.timer"   "$pkgdir/$u/shadow-sync.timer"
  install -Dm755 "$_src/system/shadow-tui.sleep"    "$pkgdir/usr/lib/systemd/system-sleep/shadow-tui"
  install -Dm644 "$_src/system/shadow-powerd-kernel.hook" "$pkgdir/usr/share/libalpm/hooks/shadow-powerd-kernel.hook"
  install -Dm644 "$_src/system/org.shadownetwork.wifi.policy" "$pkgdir/usr/share/polkit-1/actions/org.shadownetwork.wifi.policy"
  install -Dm644 "$_src/system/pam-shadow-lock"     "$pkgdir/etc/pam.d/shadow-lock"
  install -Dm644 "$_src/system/shadow-tui.desktop"  "$pkgdir/usr/share/wayland-sessions/shadow-tui.desktop"
  for d in shadow-term shadow-config shadow-files shadow-net shadow-drawer; do
    install -Dm644 "$_src/system/$d.desktop" "$pkgdir/usr/share/applications/$d.desktop"
  done
  install -Dm644 /dev/stdin "$pkgdir/etc/shadow-tui/netd.toml" <<'EOF'
# shadow-netd (system-wide Wi-Fi self-repair policy)
[wifi]
self_correct = true
allow_unvalidated = false
EOF
  install -Dm644 /dev/stdin "$pkgdir/etc/shadow-tui/healer.toml" <<'EOF'
# shadow-healer: "approve" = proposals wait for `sudo shadow-healer approve N` (default)
#                "auto"    = a patch that passes the AST policy + sandbox dry run is committed
#                            (Mod+Shift+Backspace reverts it). Only Lua modules, never binaries.
[healer]
mode = "approve"
EOF
  install -Dm644 /dev/stdin "$pkgdir/etc/shadow-tui/greeter.conf" <<'EOF'
# Shadow TUI Restricted login screen — managed by `shadow greeter …`
# warning=<text above the login fields>   (empty = none; absent = default notice)
# default_user=
# default_session=shadow
EOF
  install -dm755 "$pkgdir/var/lib/shadow-tui" "$pkgdir/var/lib/shadow-powerd"
  install -dm755 "$pkgdir/var/cache/shadow-tui-greeter"

  {
    echo "version=$pkgver-$pkgrel"
    echo "commit=$(git -C "$_src" rev-parse HEAD 2>/dev/null || echo unknown)"
    echo "repo=$(git -C "$_src" remote get-url origin 2>/dev/null)"
    echo "branch=$(git -C "$_src" rev-parse --abbrev-ref HEAD 2>/dev/null)"
    echo "built=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  } > "$o/share/BUILD_INFO"
  # integrity manifest of the runtime (shadow-sec verity builds on it)
  (cd "$o" && find bin lib -type f -exec sha512sum {} + | sort -k2) > "$o/share/MANIFEST.sha512"
}

package_shadow-tui-sec() {
  pkgdesc="Shadow TUI Restricted optional security subsystem (shadow-sec): USB lockdown while locked, nftables zones, daemon sandboxing, audit rules, sealed logs, behaviour monitor, fs-verity, PQC SSH, zeroize"
  depends=(shadow-tui-restricted)
  optdepends=('nftables: shadow-sec enable zones' 'audit: shadow-sec enable audit'
              'fsverity-utils: shadow-sec enable verity' 'openssh: post-quantum hybrid key exchange (shadow-sec enable crypto)'
              'cryptsetup: shadow-sec zeroize')
  install -Dm755 "$srcdir/target/release/shadow-sec" "$pkgdir/opt/shadow-tui/bin/shadow-sec"
  install -d "$pkgdir/usr/bin" && ln -s /opt/shadow-tui/bin/shadow-sec "$pkgdir/usr/bin/shadow-sec"
  install -Dm644 "$_src/system/shadow-sec.service" "$pkgdir/usr/lib/systemd/system/shadow-sec.service"
}
