#!/bin/sh
# Build the Shadow TUI Restricted RPMs from this checkout into OUTDIR (default ./dist).
#   packaging/build-rpm.sh [OUTDIR]          SHADOW_RPM_NOCHECK=1 skips the test suite
set -eu
src=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-$src/dist}
mkdir -p "$out"
out=$(cd "$out" && pwd)
command -v rpmbuild >/dev/null || { echo "rpmbuild not found: sudo dnf install rpm-build" >&2; exit 1; }
command -v cargo >/dev/null || { echo "cargo not found (./install.sh sets up Rust)" >&2; exit 1; }
top=$(mktemp -d "${TMPDIR:-/tmp}/shadow-rpm.XXXXXX")
trap 'rm -rf "$top"' EXIT
set -- -bb "$src/packaging/shadow-tui-restricted.spec" \
  --define "_topdir $top" --define "shadow_src $src" \
  --define "shadow_target ${SHADOW_TARGET:-${XDG_CACHE_HOME:-$HOME/.cache}/shadow-tui/target}"
[ -n "${SHADOW_RPM_NOCHECK:-}" ] && set -- "$@" --nocheck
[ -n "${SHADOW_RPM_NODEPS:-}" ] && set -- "$@" --nodeps
rpmbuild "$@"
find "$top/RPMS" -name '*.rpm' -exec cp {} "$out/" \;
ls -1 "$out"/*.rpm
