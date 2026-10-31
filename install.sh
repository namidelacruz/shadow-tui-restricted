#!/usr/bin/env bash
# ════════════════════════════════════════════════════════════════════════
#  Shadow TUI Restricted — installer for Arch Linux, Fedora and RHEL-family systems
#  (RHEL, CentOS Stream, Rocky Linux, AlmaLinux, Oracle Linux — 9 and 10)
#
#  ./install.sh                         install everything needed, build the package, install it
#  ./install.sh --greeter               also the login screen (greetd + shadow-greet)
#  ./install.sh --sec                   also the optional security package
#  ./install.sh --bootstrap URL         new machine: install, then restore your state repo
#  ./install.sh --dry-run               show every command it would run, change nothing
#  ./install.sh --uninstall [--purge]   remove the packages
#  Options: -y/--yes (no questions) · --no-greeter · --niri-ref TAG (source builds, default v25.08)
#
#  Arch:    pacman + makepkg (the PKGBUILD)
#  Fedora:  dnf + rpmbuild; niri and greetd from Fedora's repositories
#  RHEL 10: CRB + EPEL 10, niri from the yalter/niri COPR, greetd built from source
#  RHEL 9:  CRB + EPEL 9, niri and greetd built from source (no EL9 builds of either exist)
# ════════════════════════════════════════════════════════════════════════
set -Eeuo pipefail
SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
YES=0; GREETER=ask; SEC=0; UNINSTALL=0; PURGE=0; BOOT=""; DRY=0
NIRI_REF=${NIRI_REF:-v25.08}
GREETD_REF=${GREETD_REF:-0.10.3}
RUST_MIN=1.89
a=$'\e[38;2;255;176;0m'; c=$'\e[38;2;0;229;255m'; r=$'\e[38;2;255;42;0m'; z=$'\e[0m'
say()  { printf '%s▌%s %s\n' "$a" "$z" "$*"; }
info() { printf '%s  ·%s %s\n' "$c" "$z" "$*"; }
warn() { printf '%s  ! %s%s\n' "$r" "$*" "$z" >&2; }
die()  { warn "$*"; exit 1; }
ask()  { ((YES || DRY)) && return 0; read -r -p "  $1 [Y/n] " ans; [[ -z "$ans" || "$ans" =~ ^[Yy] ]]; }
# run a command, or only print it with --dry-run
run()  { if ((DRY)); then printf '%s  + %s%s\n' "$c" "$*" "$z"; else "$@"; fi; }
# run a command inside a directory
run_in() { local d=$1; shift; if ((DRY)); then printf '%s  + (in %s) %s%s\n' "$c" "$d" "$*" "$z"; else (cd "$d" && "$@"); fi; }
trap 'printf "%s  ! stopped at line %s — nothing after this step was changed; safe to re-run.%s\n" "$r" "$LINENO" "$z" >&2' ERR
while (($#)); do
  case "$1" in
    -y|--yes) YES=1 ;; --greeter) GREETER=yes ;; --no-greeter) GREETER=no ;; --sec) SEC=1 ;;
    --bootstrap) BOOT="${2:?URL}"; shift ;; --uninstall) UNINSTALL=1 ;; --purge) PURGE=1 ;;
    --dry-run) DRY=1 ;; --niri-ref) NIRI_REF="${2:?TAG}"; shift ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;; *) die "unknown option: $1" ;;
  esac; shift
done
[[ $EUID -ne 0 ]] || die "run as your normal user; sudo is used where needed."

# ── which distribution ──────────────────────────────────────────────────
OSR=${SHADOW_OS_RELEASE:-/etc/os-release}
[[ -r "$OSR" ]] || die "cannot read $OSR"
osval() { sed -n "s/^$1=//p" "$OSR" | head -n1 | tr -d '"'; }
ID=$(osval ID); LIKE=$(osval ID_LIKE); VER=$(osval VERSION_ID); NAME=$(osval PRETTY_NAME)
MAJOR=${VER%%.*}
case " $ID $LIKE " in
  *" arch "*)                                   FAM=arch ;;
  *" fedora "*) if [[ $ID == fedora ]]; then FAM=fedora; else FAM=el; fi ;;
  *" rhel "*|*" centos "*|*" rocky "*|*" almalinux "*|*" ol "*) FAM=el ;;
  *) die "unsupported distribution: $NAME (Arch, Fedora and RHEL-family 9/10 are supported)" ;;
esac
[[ $ID == fedora ]] && FAM=fedora
if [[ $FAM == el && ! $MAJOR =~ ^(9|10)$ ]]; then die "$NAME: only RHEL-family 9 and 10 are supported"; fi
if [[ $FAM == fedora && ${MAJOR:-0} -lt 41 ]]; then warn "$NAME: Fedora 41 or newer is recommended (niri is packaged there)"; fi

printf '%s\n   S H A D O W   T U I   //   R E S T R I C T E D\n%s\n' "$a" "$z"
info "$NAME → $( [[ $FAM == arch ]] && echo "pacman/makepkg" || echo "dnf/rpmbuild" )$( ((DRY)) && echo "  (dry run: nothing is changed)" )"
cache="${XDG_CACHE_HOME:-$HOME/.cache}/shadow-tui"; pkgcache="$cache/packages"
((DRY)) || mkdir -p "$pkgcache"

have() { command -v "$1" >/dev/null 2>&1; }
ver_ge() { [[ "$(printf '%s\n%s\n' "$2" "$1" | sort -V | head -n1)" == "$2" ]]; }

# ── uninstall ───────────────────────────────────────────────────────────
if ((UNINSTALL)); then
  purge=(); ((PURGE)) && purge=(--purge)
  have shadow && run shadow setup --remove "${purge[@]}" || true
  for pk in shadow-tui-sec shadow-tui-restricted; do
    if [[ $FAM == arch ]]; then
      pacman -Qq $pk &>/dev/null && ask "Remove $pk?" && run sudo pacman -R --noconfirm $pk
    else
      rpm -q $pk &>/dev/null && ask "Remove $pk?" && run sudo dnf remove -y $pk
    fi
  done
  [[ -x /usr/local/bin/niri || -x /usr/local/bin/greetd ]] && info "niri / greetd built from source stay in /usr/local/bin (remove by hand if you no longer need them)"
  say "Done."; exit 0
fi

# ════════════════════════════════════════════════════════════════════════
#  Arch Linux
# ════════════════════════════════════════════════════════════════════════
arch_install() {
  have pacman || ((DRY)) || die "pacman not found"
  say "Build tools"
  local need=() p
  for p in base-devel rust clang git; do
    pacman -Qq $p &>/dev/null || { [ $p = rust ] && pacman -Qq rustup &>/dev/null; } || need+=("$p")
  done
  ((${#need[@]})) && { info "need: ${need[*]}"; ask "Install them (sudo pacman -S --needed)?" || die "cannot build"; run sudo pacman -S --needed --noconfirm "${need[@]}"; }
  say "Compositor and runtime"
  pacman -Qq niri &>/dev/null || { info "niri is the compositor (extra/niri)"; ask "Install niri?" && run sudo pacman -S --needed --noconfirm niri; }
  say "Building (side bar, apps, terminal, daemons — first build ~5-10 min)"
  pacman -Qq shadow-network &>/dev/null && info "the old shadow-network package is replaced"
  run_in "$SRC" env PKGDEST="$pkgcache" makepkg -s -f --noconfirm
  local pkgs=("$pkgcache"/shadow-tui-restricted-[0-9]*.pkg.tar.*)
  ((SEC)) && pkgs+=("$pkgcache"/shadow-tui-sec-[0-9]*.pkg.tar.*)
  run sudo pacman -U --noconfirm "${pkgs[@]}"
}

arch_greeter() {
  run sudo pacman -S --needed --noconfirm greetd terminus-font
}

# ════════════════════════════════════════════════════════════════════════
#  Fedora / RHEL family
# ════════════════════════════════════════════════════════════════════════
DNF=dnf
dnf_install() { run sudo $DNF install -y "$@"; }

# install only the names the enabled repositories actually carry (optional extras)
dnf_install_available() {
  local want=("$@") have_list=() p
  if ((DRY)); then printf '%s  + (optional, if available) dnf install %s%s\n' "$c" "${want[*]}" "$z"; return 0; fi
  mapfile -t have_list < <($DNF -q repoquery --available --latest-limit 1 --queryformat '%{name}\n' "${want[@]}" 2>/dev/null | sort -u)
  local missing=()
  for p in "${want[@]}"; do printf '%s\n' "${have_list[@]}" | grep -qx "$p" || rpm -q "$p" &>/dev/null || missing+=("$p"); done
  ((${#missing[@]})) && info "not in your repositories (skipped): ${missing[*]}"
  ((${#have_list[@]})) && run sudo $DNF install -y "${have_list[@]}" || true
}

el_repos() {
  say "Repositories: CodeReady Builder + EPEL $MAJOR"
  dnf_install dnf-plugins-core
  if [[ $ID == rhel ]] && have subscription-manager; then
    run sudo subscription-manager repos --enable "codeready-builder-for-rhel-$MAJOR-$(uname -m)-rpms" \
      || warn "could not enable CodeReady Builder (is the system registered?) — some build dependencies may be missing"
  else
    run sudo $DNF config-manager --set-enabled crb 2>/dev/null || run sudo $DNF config-manager setopt crb.enabled=1 || true
  fi
  if ! rpm -q epel-release &>/dev/null; then
    if [[ $ID == rhel ]]; then
      dnf_install "https://dl.fedoraproject.org/pub/epel/epel-release-latest-$MAJOR.noarch.rpm"
    else
      dnf_install epel-release
    fi
  fi
}

ensure_rust() {
  local v
  v=$(rustc --version 2>/dev/null | awk '{print $2}' || true)
  if [[ -n $v ]] && ver_ge "$v" "$RUST_MIN" && have cargo; then info "Rust $v"; return; fi
  if [[ -z $v ]]; then
    dnf_install rust cargo || true
    v=$(rustc --version 2>/dev/null | awk '{print $2}' || true)
    if [[ -n $v ]] && ver_ge "$v" "$RUST_MIN"; then info "Rust $v (distribution)"; return; fi
  fi
  [[ -x "$HOME/.cargo/bin/rustup" ]] && { run "$HOME/.cargo/bin/rustup" update stable; export PATH="$HOME/.cargo/bin:$PATH"; return; }
  if [[ -n $v ]]; then info "Rust $v is older than the $RUST_MIN this build needs"; else info "Rust is not installed"; fi
  ask "Install the current stable Rust for your user with rustup (~/.cargo, removable with rustup self uninstall)?" || die "cannot build without Rust >= $RUST_MIN"
  if ((DRY)); then run "curl https://sh.rustup.rs | sh -s -- -y --profile minimal"; else
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
  fi
  export PATH="$HOME/.cargo/bin:$PATH"
}

build_niri_from_source() {
  say "Building niri $NIRI_REF from source (no package for $NAME)"
  dnf_install gcc clang git pkgconf-pkg-config systemd-devel mesa-libgbm-devel mesa-libEGL-devel \
    libxkbcommon-devel wayland-devel libinput-devel dbus-devel libseat-devel pipewire-devel \
    pango-devel cairo-gobject-devel libdisplay-info-devel
  local d="$cache/niri-src"
  if [[ -d "$d/.git" ]]; then run git -C "$d" fetch --depth 1 origin tag "$NIRI_REF"; run git -C "$d" checkout -q "$NIRI_REF"
  else run git clone -q --depth 1 --branch "$NIRI_REF" https://github.com/YaLTeR/niri "$d"; fi
  run_in "$d" cargo build --release --locked
  run sudo install -Dm755 "$d/target/release/niri" /usr/local/bin/niri
  run sudo install -Dm755 "$d/resources/niri-session" /usr/local/bin/niri-session
  run sudo install -Dm644 "$d/resources/niri.service" /etc/systemd/user/niri.service
  run sudo install -Dm644 "$d/resources/niri-shutdown.target" /etc/systemd/user/niri-shutdown.target
  run sudo sed -i 's|/usr/bin/niri|/usr/local/bin/niri|g' /etc/systemd/user/niri.service
  run sudo install -Dm644 "$d/resources/niri-portals.conf" /usr/local/share/xdg-desktop-portal/niri-portals.conf
  command -v restorecon >/dev/null && run sudo restorecon -R /usr/local/bin /etc/systemd/user || true
}

ensure_niri() {
  say "Compositor (niri)"
  if have niri; then info "niri $(niri --version 2>/dev/null | awk '{print $2}') already installed"; return; fi
  case "$FAM/$MAJOR" in
    fedora/*) dnf_install niri ;;
    el/10)
      if run sudo $DNF copr enable -y yalter/niri && dnf_install niri; then :; else
        warn "the yalter/niri COPR did not work here — building from source instead"; build_niri_from_source; fi ;;
    el/9) build_niri_from_source ;;
  esac
}

build_greetd_from_source() {
  say "Building greetd $GREETD_REF from source (not packaged for $NAME)"
  dnf_install gcc git pam-devel policycoreutils-python-utils
  local d="$cache/greetd-src"
  if [[ -d "$d/.git" ]]; then run git -C "$d" fetch --depth 1 origin tag "$GREETD_REF"; run git -C "$d" checkout -q "$GREETD_REF"
  else run git clone -q --depth 1 --branch "$GREETD_REF" https://git.sr.ht/~kennylevinsen/greetd "$d"; fi
  run_in "$d" cargo build --release --locked
  run sudo install -Dm755 "$d/target/release/greetd" /usr/local/bin/greetd
  run sudo install -Dm755 "$d/target/release/agreety" /usr/local/bin/agreety
  # RHEL PAM stack for logins through greetd
  if ((DRY)); then run "write /etc/pam.d/greetd and /etc/systemd/system/greetd.service"; else
  sudo tee /etc/pam.d/greetd >/dev/null <<'EOF'
#%PAM-1.0
# greetd (built from source by Shadow TUI Restricted's install.sh)
auth       substack     password-auth
auth       include      postlogin
account    required     pam_nologin.so
account    include      password-auth
password   include      password-auth
session    required     pam_selinux.so close
session    required     pam_loginuid.so
session    required     pam_selinux.so open
session    optional     pam_keyinit.so force revoke
session    required     pam_namespace.so
session    include      password-auth
session    include      postlogin
EOF
  sudo tee /etc/systemd/system/greetd.service >/dev/null <<'EOF'
[Unit]
Description=greetd login manager (built by Shadow TUI Restricted)
After=systemd-user-sessions.service plymouth-quit-wait.service getty@tty1.service
Wants=systemd-user-sessions.service
Conflicts=getty@tty1.service

[Service]
Type=simple
ExecStart=/usr/local/bin/greetd
IgnoreSIGPIPE=no
SendSIGHUP=yes
TimeoutStopSec=30s
KeyringMode=shared
Restart=always
RestartSec=1
StartLimitBurst=5
StartLimitIntervalSec=30

[Install]
Alias=display-manager.service
EOF
  fi
  # SELinux: run greetd in the display-manager domain, like GDM, so login sessions get their
  # normal user context
  if have semanage; then
    run sudo semanage fcontext -a -t xdm_exec_t /usr/local/bin/greetd 2>/dev/null || run sudo semanage fcontext -m -t xdm_exec_t /usr/local/bin/greetd
    run sudo restorecon -v /usr/local/bin/greetd /usr/local/bin/agreety /etc/pam.d/greetd /etc/systemd/system/greetd.service
  fi
  run sudo systemctl daemon-reload
}

rpm_install() {
  have dnf || ((DRY)) || die "dnf not found"
  [[ $FAM == el ]] && el_repos
  say "Build tools"
  dnf_install gcc gcc-c++ make git curl pkgconf-pkg-config pam-devel libxkbcommon-devel rpm-build
  ensure_rust
  ensure_niri
  say "Runtime dependencies"
  dnf_install mesa-dri-drivers mesa-libEGL libxkbcommon pam systemd fontconfig NetworkManager \
    wpa_supplicant iproute util-linux kmod git-core wireplumber pipewire
  dnf_install_available brightnessctl xdg-desktop-portal-gnome libnotify alsa-sof-firmware alsa-ucm \
    dejavu-sans-mono-fonts jetbrains-mono-fonts-all google-noto-sans-cjk-fonts age gh iwd \
    terminus-fonts-console pipewire-pulseaudio
  say "Building the RPMs (side bar, apps, terminal, daemons — first build ~5-10 min, tests included)"
  rpm -q shadow-network &>/dev/null && info "the old shadow-network package is replaced"
  run "$SRC/packaging/build-rpm.sh" "$pkgcache"
  local rpms=()
  if ((DRY)); then rpms=("$pkgcache/shadow-tui-restricted-<version>.rpm"); ((SEC)) && rpms+=("$pkgcache/shadow-tui-sec-<version>.rpm")
  else
    rpms=("$(ls -t "$pkgcache"/shadow-tui-restricted-[0-9]*.rpm | head -n1)")
    ((SEC)) && rpms+=("$(ls -t "$pkgcache"/shadow-tui-sec-[0-9]*.rpm | head -n1)")
  fi
  say "Installing"
  dnf_install "${rpms[@]}"
}

rpm_greeter() {
  if [[ $FAM == fedora ]]; then
    dnf_install greetd greetd-selinux
    dnf_install_available terminus-fonts-console
  else
    if ! have greetd; then
      info "greetd is not packaged for $NAME; it can be built from source (SELinux: run as the display-manager domain)."
      info "Otherwise keep GDM and pick \"Shadow TUI Restricted\" from the gear menu on its login screen."
      ask "Build and install greetd $GREETD_REF?" || return 1
      build_greetd_from_source
    fi
    dnf_install_available terminus-fonts-console
  fi
}

# ════════════════════════════════════════════════════════════════════════
case $FAM in
  arch) arch_install ;;
  *) rpm_install ;;
esac

export PATH="/opt/shadow-tui/bin:$PATH"
say "Your account"
run shadow setup --yes
if origin="$(git -C "$SRC" remote get-url origin 2>/dev/null)" && [[ -n "$origin" ]]; then
  run shadow update set-repo "$origin" "$(git -C "$SRC" rev-parse --abbrev-ref HEAD 2>/dev/null || echo main)" || true
fi
if [[ -n "$BOOT" ]]; then
  say "Restoring your state from $BOOT"
  yes=(); ((YES)) && yes=(--yes)
  run shadow-sync bootstrap "$BOOT" "${yes[@]}"
fi
if [[ "$GREETER" != no ]] && { [[ "$GREETER" == yes ]] || ask "Use the Shadow login screen (greetd; next boot)?"; }; then
  say "Login screen"
  if { [[ $FAM == arch ]] && arch_greeter; } || { [[ $FAM != arch ]] && rpm_greeter; }; then
    run sudo shadow greeter enable
  else
    info "login screen unchanged"
  fi
fi
say "Installed."
cat <<EOF

  ${c}Start${z}     pick "Shadow TUI Restricted" at login, or run ${a}shadow-session${z} on a TTY
  ${c}Stage${z}     Mod+Tab/J/K cycle · Mod+[ / ] width 45/65/85 % · Mod+F full width · Mod+Q close
  ${c}Apps${z}      Mod+Return terminal · Mod+Space launcher · Mod+E files · Mod+N network · Mod+Alt+L lock
  ${c}Monitors${z}  Mod+Ctrl+arrows focus · Mod+Shift+Ctrl+arrows move · scale sets itself per monitor
  ${c}Bar${z}       Mod+Shift+B hide/show (right edge; follows focus when docked)
  ${c}Settings${z}  Mod+Comma  (shadow-config: live, rolled back if niri rejects it)
  ${c}Update${z}    shadow update run   (rebuilds the $( [[ $FAM == arch ]] && echo "pacman package" || echo RPM ) from your repo)
EOF
