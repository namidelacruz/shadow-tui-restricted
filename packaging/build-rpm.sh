#!/bin/sh
# Build the Shadow TUI Restricted RPMs from this checkout into OUTDIR (default ./dist).
#   packaging/build-rpm.sh [OUTDIR]          SHADOW_RPM_NOCHECK=1 skips the test suite
# The whole rpmbuild output is kept in ~/.local/state/shadow-tui/build-rpm.log. When the build fails
# the compiler's own error is shown, and two known causes are retried by themselves:
#   - the compiler was killed for lack of memory   -> once more with one compile job
set -eu
src=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-$src/dist}
mkdir -p "$out"
out=$(cd "$out" && pwd)
# fail-safe: rpmbuild pastes these paths into shell scripts, so a folder like
# "shadow-tui-restricted (1)" or "… - Copy" is refused before anything is built
for p in "$src" "${SHADOW_TARGET:-${XDG_CACHE_HOME:-$HOME/.cache}/shadow-tui/target}"; do
  case "$p" in *[!A-Za-z0-9._/+-]*)
    echo "  ! can't build from: $p" >&2
    echo "  ! rename it so its path has only letters, digits and . _ - + /  (e.g. drop \" (1)\"), then run again" >&2
    exit 2 ;;
  esac
done
command -v rpmbuild >/dev/null || { echo "rpmbuild not found: sudo dnf install rpm-build" >&2; exit 1; }
command -v cargo >/dev/null || { echo "cargo not found (./install.sh sets up Rust)" >&2; exit 1; }
top=$(mktemp -d "${TMPDIR:-/tmp}/shadow-rpm.XXXXXX")
trap 'rm -rf "$top"' EXIT
logdir=${XDG_STATE_HOME:-$HOME/.local/state}/shadow-tui
mkdir -p "$logdir"
log=$logdir/build-rpm.log
: > "$log"

set -- -bb "$src/packaging/shadow-tui-restricted.spec" \
  --define "_topdir $top" --define "shadow_src $src" \
  --define "shadow_target ${SHADOW_TARGET:-${XDG_CACHE_HOME:-$HOME/.cache}/shadow-tui/target}"
[ -n "${SHADOW_RPM_NOCHECK:-}" ] && set -- "$@" --nocheck
[ -n "${SHADOW_RPM_NODEPS:-}" ] && set -- "$@" --nodeps

# rpmbuild to the terminal and the log; the status of rpmbuild itself, not of tee
attempt() {
  rm -rf "$top/RPMS"
  { rpmbuild "$@" 2>&1; echo $? > "$top/rc"; } | tee -a "$log"
  return "$(cat "$top/rc")"
}
# the compiler's error messages from the last attempt
errors() { sed -n "$(grep -n '^##### attempt' "$log" | tail -n1 | cut -d: -f1),\$p" "$log" | grep -E -A14 '^error(\[|:)' | head -n 80; }
show_errors() {
  e=$(errors)
  printf '\n  ! the build failed. The compiler said:\n'
  if [ -n "$e" ]; then printf '%s\n' "$e" | sed 's/^/    /'
  else tail -n 30 "$log" | sed 's/^/    /'; fi
  printf '  ! full log: %s\n' "$log"
}

n=0
try() { n=$((n + 1)); printf '##### attempt %s: %s\n' "$n" "$*" >> "$log"; attempt "$@"; }
ok=0
if try "$@"; then ok=1
else
  e=$(errors)
  # killed by the kernel for lack of memory
  if printf '%s' "$e" | grep -qE 'SIGKILL|signal: 9|Killed|memory allocation of|Cannot allocate memory'; then
    printf '\n  ~ the compiler ran out of memory — building again with one job at a time (slower)\n'
    export SHADOW_BUILD_JOBS=1
    try "$@" && ok=1 || e=$(errors)
  fi
fi
if [ "$ok" = 0 ]; then show_errors; exit 1; fi

find "$top/RPMS" -name '*.rpm' -exec cp {} "$out/" \;
ls -1 "$out"/*.rpm
