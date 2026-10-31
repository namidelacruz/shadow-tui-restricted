#!/usr/bin/env bash
# ════════════════════════════════════════════════════════════════════════
#  Run the real installer in throwaway Fedora / RHEL-compatible containers and report.
#
#    tests/distro-test.sh                       Fedora 43, AlmaLinux 10, Rocky Linux 9
#    tests/distro-test.sh fedora:44 almalinux:9 quay.io/centos/centos:stream10
#    KEEP=1 tests/distro-test.sh …              keep the containers for inspection
#
#  Needs podman (or docker) and network access. Each image gets a fresh container with this
#  checkout copied in; a normal user with sudo runs, in order:
#      ./install.sh --dry-run      the plan
#      ./install.sh --yes          the real install: repositories, dependencies, niri, Rust,
#                                  RPM build with the full test suite, dnf install
#      ./install.sh --check        the post-install verification
#  Logs: test-results/<image>.log, summary at the end. A full run builds Shadow (and on EL9
#  niri) from source: allow 15-40 minutes per image and ~10 GB of disk.
#
#  What a container can't show: the graphical session itself (no GPU / seat / logind), systemd
#  services actually running, greetd at boot, SELinux enforcement. --check marks those as
#  warnings; test them on a real VM or the laptop.
# ════════════════════════════════════════════════════════════════════════
set -uo pipefail
SRC="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENGINE=${ENGINE:-$(command -v podman || command -v docker || true)}
[[ -n $ENGINE ]] || { echo "needs podman or docker" >&2; exit 2; }
IMAGES=("$@")
((${#IMAGES[@]})) || IMAGES=(registry.fedoraproject.org/fedora:43 docker.io/library/almalinux:10 docker.io/rockylinux/rockylinux:9)
OUT="$SRC/test-results"; mkdir -p "$OUT"
declare -A RESULT

for img in "${IMAGES[@]}"; do
  tag=$(echo "$img" | sed 's|.*/||; s|:|-|g')
  log="$OUT/$tag.log"; : > "$log"
  name="shadow-test-$tag-$$"
  echo "▌ $img  (log: $log)"
  "$ENGINE" pull -q "$img" >>"$log" 2>&1 || { RESULT[$img]="FAIL: image could not be pulled"; continue; }
  "$ENGINE" run -d --name "$name" -v "$SRC:/src:ro" "$img" sleep infinity >>"$log" 2>&1 || { RESULT[$img]="FAIL: container did not start"; continue; }
  ex() { "$ENGINE" exec "$name" bash -lc "$1" >>"$log" 2>&1; }
  # a normal user with passwordless sudo, as on a workstation; the checkout copied in
  ex 'dnf -y -q install sudo git findutils procps-ng which hostname util-linux shadow-utils tar >/dev/null &&
      useradd -m tester && echo "tester ALL=(ALL) NOPASSWD: ALL" > /etc/sudoers.d/tester &&
      cp -a /src /home/tester/shadow && rm -rf /home/tester/shadow/target /home/tester/shadow/test-results &&
      chown -R tester: /home/tester/shadow' || { RESULT[$img]="FAIL: container setup (dnf)"; continue; }
  stage() { echo "──── $1" >>"$log"; ex "su - tester -c 'cd ~/shadow && $2'"; }
  r=""
  stage "dry run" "./install.sh --dry-run --no-greeter" || r="FAIL: dry run"
  [[ -z $r ]] && { stage "install" "./install.sh --yes --no-greeter" || r="FAIL: install (see log)"; }
  if [[ -z $r ]]; then
    stage "check" "./install.sh --check"; rc=$?
    fails=$(grep -c '✗' "$log" || true); warns=$(sed -n '/──── check/,$p' "$log" | grep -c '  ~' || true)
    if ((rc == 0)); then r="PASS (${warns} warning(s) — expected in a container: services, NetworkManager)"
    else r="FAIL: --check reported $fails problem(s)"; fi
  fi
  RESULT[$img]=$r
  echo "  $r"
  [[ -n ${KEEP:-} ]] || "$ENGINE" rm -f "$name" >/dev/null 2>&1
done

echo
echo "════ summary"
for img in "${IMAGES[@]}"; do printf '  %-50s %s\n' "$img" "${RESULT[$img]}"; done
grep -q FAIL <(printf '%s\n' "${RESULT[@]}") && exit 1 || exit 0
