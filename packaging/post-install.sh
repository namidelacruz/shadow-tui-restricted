#!/bin/sh
# Package scriptlets shared by the Arch package (.install) and the RPM (%post/%preun/%postun):
#   post-install.sh install | upgrade | pre-remove | post-remove
units="shadow-powerd.service shadow-audiod.service shadow-netd.service shadow-healer.service shadow-wifi-device.service"

seed_modules() {
  # behaviour modules live in a git repo the healer manages; seed it once, never overwrite
  if [ ! -d /var/lib/shadow-tui/modules ]; then
    mkdir -p /var/lib/shadow-tui/modules
    cp /opt/shadow-tui/share/modules/*.lua /var/lib/shadow-tui/modules/
    /opt/shadow-tui/bin/shadow-healer status >/dev/null 2>&1 || true   # creates the baseline commit
  fi
}

retire_old() {
  # Shadow Network / NETDIVE pieces that would now conflict
  for u in shadow-opt.service shadownet-wifi-device.service shadownet-wifi-device-resume.service; do
    systemctl disable --now "$u" >/dev/null 2>&1 || true
  done
  rm -rf /usr/share/shadow-network/lib 2>/dev/null || true
  rm -f /etc/pacman.d/hooks/shadownet.hook 2>/dev/null || true
  if [ -d /etc/shadow-network ] && [ ! -e /etc/shadow-tui/.migrated ]; then
    mkdir -p /etc/shadow-tui
    [ -f /etc/shadow-network/opt.conf ] && cp -n /etc/shadow-network/opt.conf /etc/shadow-tui/powerd.conf
    [ -f /etc/shadow-network/known-bad.conf ] && cp -n /etc/shadow-network/known-bad.conf /etc/shadow-tui/
    touch /etc/shadow-tui/.migrated
  fi
}

selinux() {
  # /opt/shadow-tui gets standard labels (bin_t for bin/, lib/) from the base policy
  command -v restorecon >/dev/null 2>&1 && restorecon -R /opt/shadow-tui /etc/shadow-tui /var/lib/shadow-tui /var/lib/shadow-powerd >/dev/null 2>&1 || true
}

case "$1" in
  install)
    retire_old
    seed_modules
    selinux
    systemctl daemon-reload >/dev/null 2>&1
    for u in $units; do systemctl enable "$u" >/dev/null 2>&1 || true; done
    if systemctl is-active -q NetworkManager.service 2>/dev/null; then
      systemctl start shadow-powerd.service shadow-netd.service shadow-healer.service >/dev/null 2>&1 || true
    fi
    cat <<'MSG'
==> Shadow TUI Restricted installed (/opt/shadow-tui). Next, as your normal user:
      shadow setup                         # config.toml and the niri config
    Login screen (static console login, sized to the panel):
      sudo shadow greeter enable           # needs greetd (./install.sh --greeter installs it)
    Or pick "Shadow TUI Restricted" in your current login manager, or run shadow-session on a TTY.
    State on GitHub (private):  shadow-sync init --github shadow-state && shadow-sync push
    Optional security subsystem: shadow-tui-sec   (nothing is active until enabled)
MSG
    ;;
  upgrade)
    retire_old
    seed_modules
    selinux
    systemctl daemon-reload >/dev/null 2>&1
    for u in $units; do systemctl enable "$u" >/dev/null 2>&1 || true; done
    systemctl try-restart shadow-powerd.service shadow-netd.service shadow-healer.service >/dev/null 2>&1 || true
    echo "==> Shadow TUI Restricted upgraded. The side bar and session services pick up the new version at the next login."
    if grep -qs '^theme=' /etc/shadow-tui/greeter.conf 2>/dev/null; then
      echo "==> 7.0 replaced the login screen with shadow-greet: run  sudo shadow greeter enable  once to refresh its settings"
    fi
    ;;
  pre-remove)
    for u in $units; do systemctl disable --now "$u" >/dev/null 2>&1 || true; done
    if grep -qs shadow-tui /etc/greetd/config.toml; then
      echo "==> restoring the previous greetd / display-manager setup"
      /opt/shadow-tui/bin/shadow greeter disable || true
    fi
    ;;
  post-remove)
    systemctl daemon-reload >/dev/null 2>&1 || true
    echo "==> Your state (~/.config/shadow-state) and /var/lib/shadow-tui/modules are kept."
    ;;
esac
exit 0
