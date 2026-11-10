#!/usr/bin/env bash
# Build ROG Control from this checkout and install it for the current user (no sudo).
#
#   ./install.sh                 build, install, enable and (re)start rog-control.service
#   ./install.sh --no-enable     build and install only; never touch systemctl
#   ./install.sh --uninstall     stop and disable the service, remove what this script installed
#
# Everything is relocatable for testing: PREFIX (default ~/.local) holds bin/rog-control,
# XDG_DATA_HOME the launcher entry and icons, XDG_CONFIG_HOME the systemd user unit.
# Nothing here edits Hyprland config; the keybinds are a one-time manual step (see CLAUDE.md).
# PREFIX must not contain whitespace or any of  " ' \ % $ `  (it is written unquoted into the unit and
# desktop entry, where they split or rewrite the command); the script refuses such a path up front.
# PREFIX, XDG_DATA_HOME, XDG_CONFIG_HOME and HOME must be absolute when set: the script changes into the
# checkout before it writes anything, and systemd refuses a relative ExecStart=.
set -eu

for var in HOME PREFIX XDG_DATA_HOME XDG_CONFIG_HOME; do
  case ${!var:-} in
    ''|/*) ;;
    *)
      echo "install.sh: $var must be an absolute path, got '${!var}'. Nothing was installed." >&2
      exit 2
      ;;
  esac
done

PREFIX=${PREFIX:-$HOME/.local}
DATA_HOME=${XDG_DATA_HOME:-$HOME/.local/share}
CONFIG_HOME=${XDG_CONFIG_HOME:-$HOME/.config}

BIN=$PREFIX/bin/rog-control
UNIT=$CONFIG_HOME/systemd/user/rog-control.service
DESKTOP=$DATA_HOME/applications/com.orneyfish.RogControl.desktop
ICON_ROOT=$DATA_HOME/icons/hicolor
ICONS="scalable/apps/com.orneyfish.RogControl.svg scalable/status/rog-control-symbolic.svg"

mode=install
enable=yes
for arg in "$@"; do
  case $arg in
    --uninstall) mode=uninstall ;;
    --no-enable) enable=no ;;
    -h|--help) sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "install.sh: unknown option '$arg' (try --help)" >&2; exit 2 ;;
  esac
done

say() { printf '==> %s\n' "$*"; }

if [ "$mode" = uninstall ]; then
  say "stopping and disabling rog-control.service"
  systemctl --user disable --now rog-control.service || true
  say "removing installed files (config and *.pre-rust backups are kept)"
  rm -f "$BIN" "$UNIT" "$DESKTOP"
  for icon in $ICONS; do rm -f "$ICON_ROOT/$icon"; done
  systemctl --user daemon-reload || true
  exit 0
fi

# The binary path goes into ExecStart= and Exec= unquoted: whitespace would split the command, and
# quotes, backslashes, % and $ are expanded by systemd / the desktop entry parser. Refuse it now,
# before anything is built or changed, instead of installing a service that cannot start.
case $BIN in
  *[[:space:]]*|*[\"\'\\%\$\`]*)
    {
      echo "install.sh: cannot install to '$BIN': PREFIX (or HOME) contains whitespace or one of the characters \" ' \\ % \$ \`"
      echo "that systemd and desktop entries would read as syntax. Use a plain path, e.g.  PREFIX=\$HOME/.local ./install.sh"
      echo "Nothing was installed."
    } >&2
    exit 2
    ;;
esac

# Everything below runs from the checkout this script lives in.
cd "$(dirname "$0")"

# 1. Prerequisites: check them all before touching anything.
missing=
command -v cargo >/dev/null 2>&1 || missing="$missing cargo"
if command -v pkg-config >/dev/null 2>&1; then
  pkg-config --exists gtk4 2>/dev/null || missing="$missing gtk4"
  pkg-config --exists libadwaita-1 2>/dev/null || missing="$missing libadwaita"
else
  missing="$missing pkg-config"
fi
if [ -n "$missing" ]; then
  {
    echo "install.sh: missing prerequisites:$missing"
    echo "On Arch / CachyOS install them with:  sudo pacman -S rust gtk4 libadwaita"
    case $missing in *pkg-config*) echo "(pkg-config itself comes from the pkgconf package)" ;; esac
    echo "Nothing was installed."
  } >&2
  exit 1
fi

# 2. Build first: a failed build must leave the machine exactly as it was.
# Build into ./target whatever CARGO_TARGET_DIR or a cargo `target-dir` setting says (--target-dir wins
# over both), and remove the old binary first: a copy left there by an earlier build must never be
# mistaken for the one this run builds. cargo re-links a removed binary from its cache in no time.
rm -f target/release/rog-control
say "building (cargo build --release --locked)"
if ! cargo build --release --locked --target-dir target; then
  echo "install.sh: the build failed; nothing was installed and the service was not touched." >&2
  exit 1
fi
[ -x target/release/rog-control ] || { echo "install.sh: cargo did not produce target/release/rog-control; nothing was installed" >&2; exit 1; }

tmpfiles=()
cleanup() { for f in ${tmpfiles[@]+"${tmpfiles[@]}"}; do rm -f "$f"; done; }
trap cleanup EXIT

# put <source> <dest> <mode>: copy beside the destination, then rename over it, so a running
# binary is replaced atomically (a plain cp onto a running executable fails with ETXTBSY).
put() {
  mkdir -p "$(dirname "$2")"
  tmp=$(mktemp "$2.XXXXXX")
  tmpfiles+=("$tmp")
  install -m "$3" "$1" "$tmp"
  mv -f "$tmp" "$2"
}

# backup_if_different <new file> <dest>: a file about to be replaced with different content is first
# copied, once, to <dest>.pre-rust (an existing backup is never overwritten), so rollback is possible.
backup_if_different() {
  if [ -f "$2" ] && [ ! -e "$2.pre-rust" ] && ! cmp -s "$1" "$2"; then
    cp -p "$2" "$2.pre-rust"
    say "kept the previous $(basename "$2") as $(basename "$2").pre-rust"
  fi
}

# put_generated <dest> <mode>: put for a file read from stdin.
put_generated() {
  new=$(mktemp)
  tmpfiles+=("$new")
  cat > "$new"
  backup_if_different "$new" "$1"
  put "$new" "$1" "$2"
}

say "installing the binary to $BIN"
put target/release/rog-control "$BIN" 755

say "installing icons under $ICON_ROOT"
for icon in $ICONS; do
  backup_if_different "icons/hicolor/$icon" "$ICON_ROOT/$icon"
  put "icons/hicolor/$icon" "$ICON_ROOT/$icon" 644
done

say "writing $UNIT"
put_generated "$UNIT" 644 <<EOF
[Unit]
Description=ROG Control tray (ASUS performance profile, battery, fans, RGB)
After=graphical-session.target
PartOf=graphical-session.target

[Service]
Type=simple
ExecStart=$BIN
Restart=on-failure
RestartSec=3

[Install]
WantedBy=graphical-session.target
EOF

say "writing $DESKTOP"
put_generated "$DESKTOP" 644 <<EOF
[Desktop Entry]
Type=Application
Name=ROG Control
Comment=Tray control for ASUS ROG performance profile, battery, fans, and RGB
Exec=$BIN
Icon=com.orneyfish.RogControl
Terminal=false
Categories=System;
StartupWMClass=com.orneyfish.RogControl
EOF

if [ "$enable" = yes ]; then
  say "reloading systemd and (re)starting rog-control.service"
  systemctl --user daemon-reload
  systemctl --user enable rog-control.service
  systemctl --user restart rog-control.service
else
  say "--no-enable: service left alone"
fi
say "done"
