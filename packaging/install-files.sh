#!/bin/sh
# Lay out Shadow TUI Restricted into a package root. One source of truth for both packages:
#   Arch   PKGBUILD package_*()           -> packaging/install-files.sh "$pkgdir" arch  TARGET SRC [main|sec]
#   RPM    %install in the .spec          -> packaging/install-files.sh %{buildroot} rpm TARGET SRC [main|sec]
# TARGET is the cargo target dir that holds release/, SRC the repository.
set -eu
dest=$1 mode=$2 target=$3 src=$4 part=${5:-main}
o="$dest/opt/shadow-tui"
inst() { install -D "$@"; }

if [ "$part" = sec ]; then
  inst -m755 "$target/release/shadow-sec" "$o/bin/shadow-sec"
  install -d "$dest/usr/bin" && ln -sf /opt/shadow-tui/bin/shadow-sec "$dest/usr/bin/shadow-sec"
  inst -m644 "$src/system/shadow-sec.service" "$dest/usr/lib/systemd/system/shadow-sec.service"
  install -d "$o/share"
  printf '%s\n' /opt/shadow-tui/bin/shadow-sec /usr/bin/shadow-sec /usr/lib/systemd/system/shadow-sec.service > "$o/share/FILES.sec"
  exit 0
fi

install -d "$o/bin" "$o/lib" "$o/share" "$dest/usr/bin"
for b in shadow-bar shadow-launch shadow-net shadow-config shadow-files shadow-lock shadow-update shadow-greet \
         shadow-sessiond shadow-displayd shadow-term shadow-powerd shadow; do
  inst -m755 "$target/release/$b" "$o/bin/$b"
done
inst -m755 "$src/system/shadow-session" "$o/bin/shadow-session"
ln -sf shadow-powerd "$o/bin/shadow-audiod"
for b in shadow-sync shadow-healer shadow-netd shadownet; do ln -sf shadow "$o/bin/$b"; done
for b in "$o"/bin/*; do ln -sf "/opt/shadow-tui/bin/$(basename "$b")" "$dest/usr/bin/$(basename "$b")"; done

inst -m755 "$src/system/greeter-wrapper"         "$o/lib/greeter-wrapper"
inst -m755 "$src/system/wifi-helper"             "$o/lib/wifi-helper"
inst -m755 "$src/system/wifi-device-helper"      "$o/lib/wifi-device-helper"
inst -m755 "$src/packaging/post-install.sh"      "$o/lib/post-install.sh"

cp -r "$src/share/hw" "$src/share/shell" "$o/share/"
install -d "$o/share/modules" && install -m644 "$src"/modules/*.lua "$o/share/modules/"
inst -m644 "$src/README.md"          "$o/share/doc/README.md"
inst -m644 "$src/docs/SPEC-TRACE.md" "$o/share/doc/SPEC-TRACE.md"
inst -m644 "$src/LICENSE"            "$dest/usr/share/licenses/shadow-tui-restricted/LICENSE"
find "$o/share" -type f -exec chmod 644 {} +

s=usr/lib/systemd/system u=usr/lib/systemd/user
for f in shadow-powerd shadow-audiod shadow-netd shadow-healer shadow-wifi-device shadow-update-recover; do
  inst -m644 "$src/system/$f.service" "$dest/$s/$f.service"
done
inst -m644 "$src/system/shadow.slice"        "$dest/$s/shadow.slice"
inst -m644 "$src/system/shadow.user.slice"   "$dest/$u/shadow.slice"
inst -m644 "$src/system/shadow-sync.service" "$dest/$u/shadow-sync.service"
inst -m644 "$src/system/shadow-sync.timer"   "$dest/$u/shadow-sync.timer"
inst -m755 "$src/system/shadow-tui.sleep"    "$dest/usr/lib/systemd/system-sleep/shadow-tui"
inst -m644 "$src/system/org.shadownetwork.wifi.policy" "$dest/usr/share/polkit-1/actions/org.shadownetwork.wifi.policy"
inst -m644 "$src/system/shadow-tui.desktop"  "$dest/usr/share/wayland-sessions/shadow-tui.desktop"
for d in shadow-term shadow-config shadow-files shadow-net shadow-launch shadow-update; do
  inst -m644 "$src/system/$d.desktop" "$dest/usr/share/applications/$d.desktop"
done

# PAM for the lock screen: Arch stacks on system-local-login, Fedora/RHEL on system-auth
if [ "$mode" = rpm ]; then
  inst -m644 /dev/stdin "$dest/etc/pam.d/shadow-lock" <<'EOF'
#%PAM-1.0
# shadow-lock (the session lock screen). Add hardware-token MFA here, e.g.:
#   auth required pam_u2f.so cue
auth       include      system-auth
account    include      system-auth
EOF
else
  inst -m644 "$src/system/pam-shadow-lock" "$dest/etc/pam.d/shadow-lock"
  # pacman hook: refuse kernels known to break this hardware (Arch only)
  inst -m644 "$src/system/shadow-powerd-kernel.hook" "$dest/usr/share/libalpm/hooks/shadow-powerd-kernel.hook"
fi

inst -m644 /dev/stdin "$dest/etc/shadow-tui/netd.toml" <<'EOF'
# shadow-netd (system-wide Wi-Fi self-repair policy)
[wifi]
self_correct = true
allow_unvalidated = false
EOF
inst -m644 /dev/stdin "$dest/etc/shadow-tui/healer.toml" <<'EOF'
# shadow-healer: "approve" = proposals wait for `sudo shadow-healer approve N` (default)
#                "auto"    = a patch that passes the AST policy + sandbox dry run is committed
#                            (Mod+Shift+Backspace reverts it). Only Lua modules, never binaries.
[healer]
mode = "approve"
EOF
inst -m644 /dev/stdin "$dest/etc/shadow-tui/greeter.conf" <<'EOF'
# Shadow TUI Restricted login screen (shadow-greet) — managed by `shadow greeter …`
session=shadow-tui
console_font=auto
# title=SHADOW TUI // RESTRICTED
# warning=<text in the login box>   (empty = none; absent = AUTHORIZED ACCESS ONLY)
EOF
install -dm755 "$dest/var/lib/shadow-tui" "$dest/var/lib/shadow-powerd" "$dest/var/cache/shadow-tui-greeter"

{
  echo "version=${SHADOW_VERSION:-unknown}"
  echo "family=$mode"
  echo "commit=$(git -C "$src" rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "repo=$(git -C "$src" remote get-url origin 2>/dev/null || true)"
  echo "branch=$(git -C "$src" rev-parse --abbrev-ref HEAD 2>/dev/null || true)"
  echo "built=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
} > "$o/share/BUILD_INFO"
# pristine copy of the shipped /etc defaults: updates offer FILE.shadow-new only when the default
# itself changed, never just because you edited yours
(cd "$dest" && find ./etc -type f 2>/dev/null) | while read -r f; do
  install -D -m644 "$dest/$f" "$o/share/etc-defaults/${f#./etc/}"
done
# integrity manifest of the runtime (shadow-sec verity builds on it)
(cd "$o" && find bin lib -type f -exec sha512sum {} + | sort -k2) > "$o/share/MANIFEST.sha512"
# every path this layout installs: `shadow update apply` compares against it to update only
# changed files and to remove files that a newer version no longer ships
(cd "$dest" && find . \( -type f -o -type l \) ! -path ./opt/shadow-tui/share/FILES | sed 's|^\.||' | sort) > "$o/share/FILES"
