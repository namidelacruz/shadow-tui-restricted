#!/usr/bin/env bash
# ════════════════════════════════════════════════════════════════════════
#  Shadow TUI Restricted — installer for Arch Linux, Fedora and RHEL-family systems
#  (RHEL, CentOS Stream, Rocky Linux, AlmaLinux, Oracle Linux — 9 and 10)
#
#  ./install.sh                         install everything needed, build the package, install it
#  ./install.sh --greeter               also the login screen (greetd + shadow-greet)
#  ./install.sh --sec                   also the optional security package (then: what this OS
#                                       supports, and the security console to enable modules)
#  ./install.sh --bootstrap URL         new machine: install, then restore your state repo
#  ./install.sh --dry-run               show every command it would run, change nothing
#  ./install.sh --check                 only verify an existing installation (pass / warn / fail)
#  ./install.sh --update-repo URL       the repository updates come from (required; default: this
#                                       checkout's origin). A private repo gets a read-only deploy key.
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
YES=0; GREETER=ask; SEC=0; UNINSTALL=0; PURGE=0; BOOT=""; DRY=0; CHECK=0; UPDATE_REPO=""
NIRI_REF=${NIRI_REF:-v25.08}
GREETD_REF=${GREETD_REF:-0.10.3}
RUST_MIN=1.89
NIRI_MIN=25.08          # the niri release the generated config is validated against
DISK_MIN_GB=5           # cargo build of Shadow; +5 more when niri / greetd are built from source
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
on_err() { printf '%s  ! failed (line %s): %s\n  ! nothing after this step was changed; safe to re-run.%s\n' "$r" "$1" "$2" "$z" >&2; }
trap 'on_err "$LINENO" "$BASH_COMMAND"' ERR
while (($#)); do
  case "$1" in
    -y|--yes) YES=1 ;; --greeter) GREETER=yes ;; --no-greeter) GREETER=no ;; --sec) SEC=1 ;;
    --bootstrap) BOOT="${2:?URL}"; shift ;; --uninstall) UNINSTALL=1 ;; --purge) PURGE=1 ;;
    --dry-run) DRY=1 ;; --check) CHECK=1 ;; --niri-ref) NIRI_REF="${2:?TAG}"; shift ;;
    --update-repo) UPDATE_REPO="${2:?URL}"; shift ;;
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

# A polkit authentication agent: it shows the password prompt for privileged actions (Wi-Fi fixes,
# reboot past an inhibitor, network changes that need it). niri has none of its own.
POLKIT_AGENTS=(/usr/libexec/polkit-mate-authentication-agent-1 /usr/lib/mate-polkit/polkit-mate-authentication-agent-1
  /usr/bin/lxqt-policykit-agent /usr/libexec/polkit-gnome-authentication-agent-1
  /usr/lib/polkit-gnome/polkit-gnome-authentication-agent-1 /usr/bin/lxpolkit /usr/libexec/xfce-polkit
  /usr/libexec/kf6/polkit-kde-authentication-agent-1 /usr/lib/polkit-kde-authentication-agent-1)
have_polkit_agent() { local a; for a in "${POLKIT_AGENTS[@]}"; do [[ -x $a ]] && return 0; done; return 1; }
ensure_polkit_agent() {
  ((DRY)) && { run sudo "$@" mate-polkit; return 0; }
  have_polkit_agent && { info "polkit agent present"; return 0; }
  local p
  for p in mate-polkit lxqt-policykit polkit-gnome lxpolkit; do
    if sudo "$@" "$p" >/dev/null 2>&1; then info "polkit agent installed: $p"; return 0; fi
  done
  warn "no polkit authentication agent could be installed: actions that need a password will fail (install mate-polkit or lxqt-policykit)"
}

# Failsafe while the Shadow package itself is installed: the same marker the updater uses, so no
# Shadow desktop session can start on half-written files, and shutdown/suspend are held off. If the
# install is cut off, shadow-update-recover.service completes or reverses it at the next boot.
UPDATE_MARK=/var/lib/shadow-tui/update-in-progress
guarded_install() {   # guarded_install PKGFILE... -- COMMAND...
  local pkgs=() inh=()
  while [[ $1 != -- ]]; do pkgs+=("$1"); shift; done; shift
  if ((DRY)); then run sudo "$@"; return; fi
  if sudo test -e "$UPDATE_MARK"; then
    warn "an earlier update did not finish — recovering it first"
    if [[ -x /opt/shadow-tui/bin/shadow ]]; then sudo /opt/shadow-tui/bin/shadow update recover || die "recovery failed; see the message above"
    else sudo rm -f "$UPDATE_MARK"; fi
  fi
  sudo install -d -m755 /var/lib/shadow-tui
  printf 'kind=package\npid=%s\nboot=%s\nstarted=%s\nbackup=\npackage=%s\n' "$$" \
    "$(cat /proc/sys/kernel/random/boot_id 2>/dev/null)" "$(date +%s)" "${pkgs[0]}" | sudo tee "$UPDATE_MARK" >/dev/null
  sync
  have systemd-inhibit && inh=(systemd-inhibit --what=shutdown:sleep:handle-lid-switch --who="Shadow TUI install" --why="installing system files; wait for it to finish" --mode=block)
  local rc=0
  sudo "${inh[@]}" "$@" || rc=$?
  sync
  if ((rc == 0)); then
    sudo rm -f "$UPDATE_MARK"
  elif [[ -x /opt/shadow-tui/bin/shadow ]]; then
    # nothing written / everything written: cleared; half written: stays locked, explains how to fix
    sudo /opt/shadow-tui/bin/shadow update recover || warn "the desktop stays locked until this is fixed: re-run ./install.sh"
  else
    sudo rm -f "$UPDATE_MARK"
  fi
  return "$rc"
}
# The package file of NAME that this run built (newer than MARKER): never an older build left in
# the cache, never a .sig, never a debug package.
built_pkg() {
  local marker=$1 name=$2 ext=$3 f
  f=$(find "$pkgcache" -maxdepth 1 -type f -newer "$marker" -name "$name-[0-9]*$ext" ! -name '*.sig' ! -name '*-debug*' ! -name '*.src.rpm' -printf '%T@ %p\n' 2>/dev/null | sort -rn | head -n1 | cut -d' ' -f2-)
  [[ -n $f ]] || die "the build finished but produced no $name package in $pkgcache"
  printf '%s\n' "$f"
}
ver_ge() { [[ "$(printf '%s\n%s\n' "$2" "$1" | sort -V | head -n1)" == "$2" ]]; }
export PATH="/opt/shadow-tui/bin:/usr/local/bin:$PATH"

# ── failsafes before anything is changed ────────────────────────────────
free_gb() { df -Pk "${1:-$HOME}" 2>/dev/null | awk 'NR==2 {print int($4/1048576)}'; }
need_disk() {
  local want=$1 got; got=$(free_gb "$HOME")
  [[ -z $got ]] && return 0
  ((got >= want)) || die "only ${got} GB free in $HOME; building needs about ${want} GB (cargo build cache in ~/.cache/shadow-tui)"
}
preflight() {
  say "Pre-flight checks"
  case "$(uname -m)" in x86_64|aarch64) ;; *) die "unsupported CPU architecture $(uname -m) (x86_64 or aarch64)";; esac
  if [[ -e /run/ostree-booted ]]; then
    die "this is an image-based system (Fedora Atomic / Silverblue / Kinoite): packages are layered with rpm-ostree, which this installer does not do"
  fi
  have sudo || die "sudo is required"
  if ((!DRY)); then sudo -v || die "sudo access is required to install packages"; fi
  # sudo and polkit both treat the "wheel" group as administrators on Fedora, RHEL and Arch:
  # without it, password prompts in Shadow (updates, security console, Wi-Fi fixes) are refused
  if ! id -nG | tr ' ' '\n' | grep -qx wheel; then
    warn "your account ($(id -un)) is not in the 'wheel' group: password prompts for administrator actions will be refused"
    info "fix: sudo usermod -aG wheel $(id -un)   then log out and back in"
  fi
  have git || [[ $FAM == arch ]] || ((DRY)) || dnf_install git
  need_disk "$DISK_MIN_GB"
  if [[ $FAM == el && $ID == rhel ]] && have subscription-manager && ((!DRY)); then
    sudo subscription-manager identity >/dev/null 2>&1 || die "this RHEL system is not registered, so CodeReady Builder (needed for the -devel packages) is unavailable: sudo subscription-manager register"
  fi
  info "architecture $(uname -m), $(free_gb "$HOME") GB free, sudo OK"
}

# Where a package comes from when it can't be found (for the error message)
repo_hint() {
  case "$1" in
    *-devel|rpm-build) [[ $FAM == el ]] && echo "CodeReady Builder (crb)" || echo "Fedora base/updates" ;;
    libseat*|libdisplay-info*|terminus-fonts*|age|seatd) [[ $FAM == el ]] && echo "EPEL $MAJOR" || echo "Fedora base/updates" ;;
    niri) [[ $FAM == el ]] && echo "the yalter/niri COPR (EL10) or a source build (EL9)" || echo "Fedora base/updates" ;;
    *) [[ $FAM == el ]] && echo "BaseOS / AppStream" || echo "Fedora base/updates" ;;
  esac
}

# Required packages: verify ALL of them can be installed before installing any; name what's missing
dnf_require() {
  if ((DRY)); then dnf_install "$@"; return; fi
  local avail=() missing=() p
  mapfile -t avail < <($DNF -q repoquery --latest-limit 1 --queryformat '%{name}\n' "$@" 2>/dev/null | sort -u)
  for p in "$@"; do
    [[ $p == http* || $p == */* ]] && continue
    printf '%s\n' "${avail[@]}" | grep -qx "$p" || rpm -q --whatprovides "$p" &>/dev/null || missing+=("$p")
  done
  if ((${#missing[@]})); then
    warn "required packages not available from your enabled repositories:"
    for p in "${missing[@]}"; do warn "   $p   (normally from $(repo_hint "$p"))"; done
    [[ $FAM == el ]] && warn "check: sudo dnf repolist   (crb and epel must be enabled)"
    die "nothing was installed in this step — fix the repositories and re-run ./install.sh"
  fi
  dnf_install "$@"
}

# a built or installed binary must run and find all its libraries
check_binary() {
  local f=$1
  ((DRY)) && return 0
  [[ -x $f ]] || die "$f was not produced"
  if ldd "$f" 2>/dev/null | grep -q "not found"; then
    die "$f is missing libraries: $(ldd "$f" | grep 'not found' | awk '{print $1}' | tr '\n' ' ')"
  fi
}

niri_version() { niri --version 2>/dev/null | awk '{print $2}' || true; }
niri_ok() {
  local v; v=$(niri_version)
  [[ -n $v ]] || return 1
  # packaged git snapshots (0.0.git…) can't be compared; the config check after setup decides
  [[ $v == 0.0.git* ]] && return 0
  ver_ge "$v" "$NIRI_MIN"
}

# keep the 3 newest builds of each package in the cache (shadow update rollback uses the previous)
prune_cache() {
  ((DRY)) && return 0
  local n
  for n in shadow-tui-restricted shadow-tui-sec; do
    find "$pkgcache" -maxdepth 1 -type f -name "$n-[0-9]*" ! -name '*.sig' -printf '%T@ %p\n' 2>/dev/null | sort -rn | tail -n +4 | cut -d' ' -f2- |
      while IFS= read -r f; do rm -f "$f" "$f.sig"; done
  done
}

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
  ensure_polkit_agent pacman -S --needed --noconfirm
  say "Building (side bar, apps, terminal, daemons — first build ~5-10 min)"
  pacman -Qq shadow-network &>/dev/null && info "the old shadow-network package is replaced"
  local marker; marker=$(mktemp "${TMPDIR:-/tmp}/shadow-build.XXXXXX")
  run_in "$SRC" env PKGDEST="$pkgcache" makepkg -s -f --noconfirm
  local pkgs=()
  if ((DRY)); then pkgs=("$pkgcache/shadow-tui-restricted-<version>.pkg.tar.zst"); ((SEC)) && pkgs+=("$pkgcache/shadow-tui-sec-<version>.pkg.tar.zst")
  else
    pkgs=("$(built_pkg "$marker" shadow-tui-restricted .pkg.tar.zst)")
    ((SEC)) && pkgs+=("$(built_pkg "$marker" shadow-tui-sec .pkg.tar.zst)")
  fi
  rm -f "$marker"
  guarded_install "${pkgs[@]}" -- pacman -U --noconfirm "${pkgs[@]}"
  prune_cache
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
  say "Building niri $NIRI_REF from source (no suitable package for $NAME)"
  need_disk $((DISK_MIN_GB + 5))
  dnf_require gcc clang git pkgconf-pkg-config systemd-devel mesa-libgbm-devel mesa-libEGL-devel \
    libxkbcommon-devel wayland-devel libinput-devel dbus-devel libseat-devel pipewire-devel \
    pango-devel cairo-gobject-devel libdisplay-info-devel
  local d="$cache/niri-src"
  if [[ -d "$d/.git" ]]; then run git -C "$d" fetch --depth 1 origin tag "$NIRI_REF"; run git -C "$d" checkout -q "$NIRI_REF"
  else run git clone -q --depth 1 --branch "$NIRI_REF" https://github.com/YaLTeR/niri "$d"; fi
  run_in "$d" cargo build --release --locked
  check_binary "$d/target/release/niri"
  run sudo install -Dm755 "$d/target/release/niri" /usr/local/bin/niri
  run sudo install -Dm755 "$d/resources/niri-session" /usr/local/bin/niri-session
  run sudo install -Dm644 "$d/resources/niri.service" /etc/systemd/user/niri.service
  run sudo install -Dm644 "$d/resources/niri-shutdown.target" /etc/systemd/user/niri-shutdown.target
  run sudo sed -i 's|/usr/bin/niri|/usr/local/bin/niri|g' /etc/systemd/user/niri.service
  run sudo install -Dm644 "$d/resources/niri-portals.conf" /usr/local/share/xdg-desktop-portal/niri-portals.conf
  command -v restorecon >/dev/null && run sudo restorecon -R /usr/local/bin /etc/systemd/user || true
  ((DRY)) || /usr/local/bin/niri --version >/dev/null || die "the niri that was just built does not run"
}

ensure_niri() {
  say "Compositor (niri)"
  if have niri && niri_ok; then info "niri $(niri_version) already installed"; return; fi
  if have niri; then warn "the installed niri ($(niri_version)) is older than $NIRI_MIN, which Shadow's config needs"; fi
  case "$FAM/$MAJOR" in
    fedora/*) run sudo $DNF install -y niri || run sudo $DNF upgrade -y niri || true ;;
    el/10) { run sudo $DNF copr enable -y yalter/niri && { run sudo $DNF install -y niri || run sudo $DNF upgrade -y niri; }; } || warn "the yalter/niri COPR did not work here" ;;
  esac
  ((DRY)) && [[ $FAM/$MAJOR != el/9 ]] && return
  if [[ $FAM/$MAJOR == el/9 ]] || ! niri_ok; then
    [[ $FAM/$MAJOR == el/9 ]] || warn "no package gives niri >= $NIRI_MIN here (have: $(niri_version || true)) — building it from source"
    ask "Build niri $NIRI_REF from source into /usr/local (about 10 minutes)?" || die "Shadow needs niri >= $NIRI_MIN"
    build_niri_from_source
    hash -r
    ((DRY)) || niri_ok || die "niri still reports $(niri_version) — check that /usr/local/bin comes first in PATH"
  fi
  info "niri $(niri_version)"
}

build_greetd_from_source() {
  say "Building greetd $GREETD_REF from source (not packaged for $NAME)"
  dnf_require gcc git pam-devel policycoreutils-python-utils
  if have getenforce && [[ $(getenforce) == Enforcing ]] && ! have semanage && ((!DRY)); then
    warn "SELinux is enforcing and semanage is unavailable: a greetd without its SELinux label could not log you in"
    return 1
  fi
  local d="$cache/greetd-src"
  if [[ -d "$d/.git" ]]; then run git -C "$d" fetch --depth 1 origin tag "$GREETD_REF"; run git -C "$d" checkout -q "$GREETD_REF"
  else run git clone -q --depth 1 --branch "$GREETD_REF" https://git.sr.ht/~kennylevinsen/greetd "$d"; fi
  run_in "$d" cargo build --release --locked
  check_binary "$d/target/release/greetd"
  check_binary "$d/target/release/agreety"
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
  if ((!DRY)) && have getenforce && [[ $(getenforce) != Disabled ]] && [[ $(stat -c %C /usr/local/bin/greetd 2>/dev/null) != *xdm_exec_t* ]]; then
    warn "greetd did not get the display-manager SELinux label"
    return 1
  fi
  run sudo systemctl daemon-reload
}

rpm_install() {
  have dnf || ((DRY)) || die "dnf not found"
  [[ $FAM == el ]] && el_repos
  say "Build tools"
  dnf_require gcc gcc-c++ make git curl pkgconf-pkg-config pam-devel libxkbcommon-devel rpm-build
  ensure_rust
  # the spec's own BuildRequires, exactly as rpmbuild will check them
  run sudo $DNF builddep -y "$SRC/packaging/shadow-tui-restricted.spec" || warn "dnf builddep could not read the spec; using the list above"
  ensure_niri
  say "Runtime dependencies"
  dnf_require mesa-dri-drivers mesa-libEGL libxkbcommon pam systemd fontconfig dejavu-sans-mono-fonts \
    NetworkManager wpa_supplicant iproute util-linux kmod git-core openssh-clients wireplumber pipewire
  # shellcheck disable=SC2086  # $DNF is "dnf" or "dnf5"
  ensure_polkit_agent $DNF install -y
  dnf_install_available brightnessctl xdg-desktop-portal-gnome libnotify alsa-sof-firmware alsa-ucm \
    jetbrains-mono-fonts-all google-noto-sans-cjk-fonts age gh iwd \
    terminus-fonts-console pipewire-pulseaudio
  say "Building the RPMs (side bar, apps, terminal, daemons — first build ~5-10 min, tests included)"
  rpm -q shadow-network &>/dev/null && info "the old shadow-network package is replaced"
  local marker; marker=$(mktemp "${TMPDIR:-/tmp}/shadow-build.XXXXXX")
  run "$SRC/packaging/build-rpm.sh" "$pkgcache"
  local rpms=()
  if ((DRY)); then rpms=("$pkgcache/shadow-tui-restricted-<version>.rpm"); ((SEC)) && rpms+=("$pkgcache/shadow-tui-sec-<version>.rpm")
  else
    rpms=("$(built_pkg "$marker" shadow-tui-restricted .rpm)")
    ((SEC)) && rpms+=("$(built_pkg "$marker" shadow-tui-sec .rpm)")
  fi
  rm -f "$marker"
  say "Installing"
  # shellcheck disable=SC2086  # $DNF is "dnf" or "dnf5"
  guarded_install "${rpms[@]}" -- $DNF install -y "${rpms[@]}"
  prune_cache
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
      # a failure here must not end the installation: keep the current login manager instead
      local rc=0
      set +e; ( trap - ERR; set -e; build_greetd_from_source ); rc=$?; set -e
      if ((rc)); then warn "greetd could not be set up — your current login manager (GDM) stays"; return 1; fi
    fi
    dnf_install_available terminus-fonts-console
  fi
}

# ════════════════════════════════════════════════════════════════════════
#  Verification: does the installed system have everything it needs?
# ════════════════════════════════════════════════════════════════════════
verify() {
  local fails=0 warns=0
  pass() { printf '%s  ✓%s %s\n' "$c" "$z" "$*"; }
  wrn()  { printf '%s  ~%s %s\n' "$a" "$z" "$*"; warns=$((warns + 1)); }
  bad()  { printf '%s  ✗ %s%s\n' "$r" "$*" "$z"; fails=$((fails + 1)); }
  say "Verifying the installation"
  local v
  if [[ $FAM == arch ]]; then v=$(pacman -Q shadow-tui-restricted 2>/dev/null | awk '{print $2}' || true)
  else v=$(rpm -q --queryformat '%{VERSION}-%{RELEASE}' shadow-tui-restricted 2>/dev/null || true); fi
  [[ -n $v && $v != *"not installed"* ]] && pass "package shadow-tui-restricted $v" || bad "package shadow-tui-restricted is not installed"
  local b missing=()
  for b in shadow shadow-bar shadow-launch shadow-net shadow-config shadow-files shadow-lock shadow-update shadow-flyout shadow-tasks shadow-fix shadow-passfix shadow-wallpaper shadow-hub shadow-greet \
           shadow-sessiond shadow-displayd shadow-term shadow-powerd; do
    f=/opt/shadow-tui/bin/$b
    if [[ ! -x $f ]]; then missing+=("$b"); elif ldd "$f" 2>/dev/null | grep -q "not found"; then missing+=("$b(libraries)"); fi
  done
  ((${#missing[@]})) && bad "programs missing or broken: ${missing[*]}" || pass "all 12 programs present, libraries resolved"
  if have niri; then
    if niri_ok; then pass "niri $(niri_version) ($(command -v niri))"; else bad "niri $(niri_version) is older than $NIRI_MIN — re-run ./install.sh"; fi
    local cfg="${XDG_CONFIG_HOME:-$HOME/.config}/shadow-state/niri.kdl"
    if [[ -f $cfg ]]; then
      if niri validate -c "$cfg" >/dev/null 2>&1; then pass "niri accepts the generated config ($cfg)"
      else bad "niri rejects $cfg: $(niri validate -c "$cfg" 2>&1 | grep -v '^$' | tail -n1)"; fi
    else wrn "no niri config yet — run: shadow setup"; fi
  else bad "niri is not installed (the compositor) — re-run ./install.sh"; fi
  [[ -f /usr/share/wayland-sessions/shadow-tui.desktop ]] && pass "login session entry present" || bad "/usr/share/wayland-sessions/shadow-tui.desktop missing"
  if [[ -f /etc/pam.d/shadow-lock ]]; then
    local stack; stack=$(awk '$2=="include"{print $3; exit}' /etc/pam.d/shadow-lock)
    [[ -z $stack || -f /etc/pam.d/$stack ]] && pass "lock screen PAM (stack: ${stack:-own})" || bad "/etc/pam.d/shadow-lock includes $stack, which does not exist — the lock screen could not unlock"
  else bad "/etc/pam.d/shadow-lock missing — the lock screen could not unlock"; fi
  local u off=()
  for u in shadow-powerd shadow-netd shadow-healer shadow-update-recover; do systemctl is-enabled -q "$u.service" 2>/dev/null || off+=("$u"); done
  ((${#off[@]})) && wrn "services not enabled: ${off[*]} (sudo systemctl enable --now ${off[*]/%/.service})" || pass "system services enabled"
  have_polkit_agent && pass "polkit authentication agent installed (password prompts work)" || bad "no polkit authentication agent: privileged actions fail instead of asking for a password — install mate-polkit"
  id -nG | tr ' ' '\n' | grep -qx wheel && pass "$(id -un) is an administrator (wheel)" || wrn "$(id -un) is not in 'wheel': administrator password prompts will be refused (sudo usermod -aG wheel $(id -un))"
  if shadow update policy --offline >/dev/null 2>&1; then pass "update source set up and within policy"
  else bad "update source missing or outside policy (required): shadow update policy   →   shadow update setup"; fi
  if [[ -e /var/lib/shadow-tui/update-in-progress ]]; then
    bad "an update is in progress or was interrupted: the desktop is locked until it finishes (sudo shadow update recover)"
  else pass "no unfinished update"; fi
  local font; font=$(fc-match -f '%{file}' monospace 2>/dev/null || true)
  [[ -n $font && -f $font ]] && pass "monospace font: $(basename "$font")" || bad "no monospace font found (Shadow's apps cannot draw text): install dejavu-sans-mono-fonts / ttf-dejavu"
  ls /usr/share/glvnd/egl_vendor.d/*mesa*.json >/dev/null 2>&1 && pass "Mesa EGL (OpenGL) present" || wrn "Mesa EGL vendor file not found — niri needs Mesa's EGL"
  systemctl is-active -q NetworkManager 2>/dev/null && pass "NetworkManager running" || wrn "NetworkManager is not running (shadow-net and the Wi-Fi repair need it)"
  if [[ $FAM != arch ]] && have getenforce && [[ $(getenforce) != Disabled ]]; then
    local mis; mis=$(restorecon -n -R -v /opt/shadow-tui 2>/dev/null | wc -l || true)
    [[ ${mis:-0} -eq 0 ]] && pass "SELinux labels of /opt/shadow-tui correct ($(getenforce))" || wrn "$mis files under /opt/shadow-tui have the wrong SELinux label: sudo restorecon -R /opt/shadow-tui"
  fi
  if grep -qs shadow-tui /etc/greetd/config.toml; then
    have greetd && pass "greetd installed ($(command -v greetd))" || bad "the login screen is enabled but greetd is missing — sudo shadow greeter disable"
    local gu; gu=$(sed -n 's/^user *= *"\(.*\)"/\1/p' /etc/greetd/config.toml | head -n1)
    id -u "$gu" >/dev/null 2>&1 && pass "login screen user: $gu" || bad "greetd runs as \"$gu\", which does not exist"
    shadow-greet --self-test >/dev/null 2>&1 && pass "login screen self-test" || bad "shadow-greet --self-test fails: $(shadow-greet --self-test 2>&1 | head -n1)"
    if [[ $(command -v greetd) == /usr/local/bin/greetd ]] && have getenforce && [[ $(getenforce) != Disabled ]]; then
      [[ $(stat -c %C /usr/local/bin/greetd 2>/dev/null) == *xdm_exec_t* ]] && pass "greetd SELinux label (xdm_exec_t)" || bad "greetd lacks its SELinux label: sudo semanage fcontext -a -t xdm_exec_t /usr/local/bin/greetd && sudo restorecon /usr/local/bin/greetd"
    fi
    systemctl is-enabled -q greetd.service 2>/dev/null && pass "greetd enabled (from the next boot)" || wrn "greetd.service is not enabled"
  fi
  if ((fails)); then
    warn "$fails problem(s) must be fixed before Shadow will work properly (see ✗ above); $warns warning(s)"
    return 1
  fi
  if ((warns)); then say "All required checks passed ($warns warning(s) above)."; else say "All checks passed."; fi
}

if ((CHECK)); then trap - ERR; verify; exit $?; fi

preflight
case $FAM in
  arch) arch_install ;;
  *) rpm_install ;;
esac

say "Your account"
run shadow setup --yes
# Required: the update source, with its private-repository policy (read-only deploy key, pinned
# host key, private check). The wizard prints the deploy key to add; nothing secret is stored.
say "Update source (required)"
src_url=$UPDATE_REPO
if [[ -z $src_url ]]; then
  src_url=$(git -C "$SRC" remote get-url origin 2>/dev/null || true)
  # never carry a token from a clone URL into the configuration
  [[ $src_url =~ ^https?://[^/]*@ ]] && { warn "this checkout's origin URL contains credentials — not used"; src_url=""; }
fi
src_branch=$(git -C "$SRC" rev-parse --abbrev-ref HEAD 2>/dev/null || echo main)
[[ $src_branch == HEAD ]] && src_branch=main
setup_args=(); [[ -n $src_url ]] && setup_args+=("$src_url" --branch "$src_branch")
((YES)) && setup_args+=(--yes)
if ((DRY)); then run shadow update setup "${setup_args[@]}"
elif ! shadow update setup "${setup_args[@]}"; then
  warn "the update source is not ready yet: shadow-update stays locked until it is (Mod+U → \"Set up update source\", or: shadow update setup)"
fi
if [[ -n "$BOOT" ]]; then
  say "Restoring your state from $BOOT"
  yes=(); ((YES)) && yes=(--yes)
  run shadow-sync bootstrap "$BOOT" "${yes[@]}"
fi
if [[ "$GREETER" != no ]] && { [[ "$GREETER" == yes ]] || ask "Use the Shadow login screen (greetd; next boot)?"; }; then
  say "Login screen"
  if { [[ $FAM == arch ]] && arch_greeter; } || { [[ $FAM != arch ]] && rpm_greeter; }; then
    run sudo shadow greeter enable || warn "shadow greeter enable failed — the login screen is unchanged (shadow greeter status explains)"
  else
    info "login screen unchanged"
  fi
fi
if ((SEC)); then
  # optional security modules: what this OS supports first; enabling is always an explicit,
  # password-confirmed step in the security console — never automatic, not even with --yes
  say "Security subsystem: what this system supports"
  if ((DRY)); then run shadow-sec precheck
  else
    shadow-sec precheck || true
    if ((!YES)) && [[ -t 0 ]] && ask "Open the security console to enable modules now (each asks for your password)?"; then
      shadow-sec console || true
    else
      info "enable modules later: shadow-sec console   (or side bar → SYSTEM → Security console)"
    fi
  fi
fi
verified=0
if ((DRY)); then info "(dry run: verification skipped)"; elif verify; then verified=1; fi
((verified || DRY)) && say "Installed." || warn "Installed, but the checks above found problems. After fixing them: ./install.sh --check"
cat <<EOF

  ${c}Start${z}     pick "Shadow TUI Restricted" at login, or run ${a}shadow-session${z} on a TTY
  ${c}Stage${z}     Mod+Tab/J/K cycle · Mod+[ / ] width 45/65/85 % · Mod+F full width · Mod+Q close
  ${c}Apps${z}      Mod+Return terminal · Mod+Space launcher · Mod+E files · Mod+N network · Mod+Alt+L lock
  ${c}Monitors${z}  Mod+Ctrl+arrows focus · Mod+Shift+Ctrl+arrows move · scale sets itself per monitor
  ${c}Bar${z}       Mod+Shift+B hide/show (right edge; follows focus when docked)
  ${c}Settings${z}  Mod+Comma  (shadow-config: live, rolled back if niri rejects it)
  ${c}Update${z}    Mod+U  (shadow-update: on demand; only changed files)   source: shadow update policy
EOF
