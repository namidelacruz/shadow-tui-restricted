#!/usr/bin/env bash
# ════════════════════════════════════════════════════════════════════════
#  Shadow TUI Restricted — build + install as one pacman package (Arch Linux)
#
#  ./install.sh                         build, install, set up your account
#  ./install.sh --greeter               also enable the login screen (greetd)
#  ./install.sh --sec                   also install the optional security package
#  ./install.sh --bootstrap URL         new machine: install, then restore your state repo
#  ./install.sh --uninstall [--purge]   undo everything
# ════════════════════════════════════════════════════════════════════════
set -Eeuo pipefail
SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
YES=0; GREETER=ask; SEC=0; UNINSTALL=0; PURGE=0; BOOT=""
a=$'\e[38;2;255;176;0m'; c=$'\e[38;2;0;229;255m'; r=$'\e[38;2;255;42;0m'; z=$'\e[0m'
say()  { printf '%s▌%s %s\n' "$a" "$z" "$*"; }
info() { printf '%s  ·%s %s\n' "$c" "$z" "$*"; }
die()  { printf '%s  ! %s%s\n' "$r" "$*" "$z" >&2; exit 1; }
ask()  { ((YES)) && return 0; read -r -p "  $1 [Y/n] " ans; [[ -z "$ans" || "$ans" =~ ^[Yy] ]]; }
trap 'printf "%s  ! stopped at line %s — nothing after this step was changed; safe to re-run.%s\n" "$r" "$LINENO" "$z" >&2' ERR
while (($#)); do
  case "$1" in
    -y|--yes) YES=1 ;; --greeter) GREETER=yes ;; --no-greeter) GREETER=no ;; --sec) SEC=1 ;;
    --bootstrap) BOOT="${2:?URL}"; shift ;; --uninstall) UNINSTALL=1 ;; --purge) PURGE=1 ;;
    -h|--help) sed -n '2,10p' "$0"; exit 0 ;; *) die "unknown option: $1" ;;
  esac; shift
done
[[ $EUID -ne 0 ]] || die "run as your normal user; sudo is used where needed."
command -v pacman >/dev/null || die "this targets Arch Linux (pacman not found)."
printf '%s\n   S H A D O W   T U I   //   R E S T R I C T E D\n%s\n' "$a" "$z"

if ((UNINSTALL)); then
  command -v shadow >/dev/null && shadow setup --remove $( ((PURGE)) && echo --purge ) || true
  for pk in shadow-tui-sec shadow-tui-restricted; do
    pacman -Qq $pk &>/dev/null && ask "Remove $pk?" && sudo pacman -R --noconfirm $pk
  done
  say "Done."; exit 0
fi

say "Build tools"
need=(); for p in base-devel rust clang git; do pacman -Qq $p &>/dev/null || { [ $p = rust ] && pacman -Qq rustup &>/dev/null; } || need+=($p); done
((${#need[@]})) && { info "need: ${need[*]}"; ask "Install them (sudo pacman -S --needed)?" || die "cannot build"; sudo pacman -S --needed --noconfirm "${need[@]}"; }

say "Building (side bar, apps, terminal, daemons — first build ~5-10 min)"
pacman -Qq niri &>/dev/null || { info "niri is the compositor (extra/niri)"; ask "Install niri (sudo pacman -S)?" && sudo pacman -S --needed --noconfirm niri; }
pacman -Qq shadow-network &>/dev/null && info "the old shadow-network package (niri fork, Python engine) is replaced"
pkgcache="${XDG_CACHE_HOME:-$HOME/.cache}/shadow-tui/packages"; mkdir -p "$pkgcache"
( cd "$SRC" && PKGDEST="$pkgcache" makepkg -s -f --noconfirm )
pkgs=("$pkgcache"/shadow-tui-restricted-[0-9]*.pkg.tar.*)
((SEC)) && pkgs+=("$pkgcache"/shadow-tui-sec-[0-9]*.pkg.tar.*)
sudo pacman -U --noconfirm "${pkgs[@]}"

say "Your account"
shadow setup --yes
if origin="$(git -C "$SRC" remote get-url origin 2>/dev/null)" && [[ -n "$origin" ]]; then
  shadow update set-repo "$origin" "$(git -C "$SRC" rev-parse --abbrev-ref HEAD 2>/dev/null || echo main)" || true
fi
if [[ -n "$BOOT" ]]; then
  say "Restoring your state from $BOOT"
  shadow-sync bootstrap "$BOOT" $( ((YES)) && echo --yes )
fi
if [[ "$GREETER" != no ]] && { [[ "$GREETER" == yes ]] || ask "Use the Shadow login screen (greetd; next boot)?"; }; then
  sudo pacman -S --needed --noconfirm greetd terminus-font
  sudo shadow greeter enable
fi
say "Installed."
cat <<EOF

  ${c}Start${z}     pick "Shadow TUI Restricted" at login, or run ${a}shadow-session${z} on a TTY
  ${c}Stage${z}     Mod+Tab/J/K cycle · Mod+[ / ] width 45/65/85 % · Mod+F full width · Mod+Q close
  ${c}Apps${z}      Mod+Return terminal · Mod+Space launcher · Mod+E files · Mod+N network · Mod+Alt+L lock
  ${c}Monitors${z}  Mod+Ctrl+arrows focus · Mod+Shift+Ctrl+arrows move · scale sets itself per monitor
  ${c}Bar${z}       Mod+Shift+B hide/show (right edge; follows focus when docked)
  ${c}Settings${z}  Mod+Comma  (shadow-config: live, rolled back if niri rejects it)
  ${c}State${z}     shadow-sync init --github shadow-state && shadow-sync push
  ${c}Healer${z}    shadow-healer status · Mod+Shift+Backspace reverts the last automated fix
  ${c}Security${z}  optional: ./install.sh --sec, then sudo shadow-sec status
EOF
