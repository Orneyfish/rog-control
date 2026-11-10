# Source me:  . specs/rust-rewrite/harness/fake-tools.sh
# Creates $FAKE, a directory of stand-ins for the hardware CLIs, and $FAKE_LOG, a one-line-per-call log.
# Put it first on PATH (PATH=$FAKE:$PATH) to run an app without it ever changing real hardware.
# Fails closed: only the read-only invocations listed below reach the real tool; EVERYTHING else
# (every write, `systemctl reboot`, notify-send, ...) is logged and answered with empty stdout / exit 0.
#   FAKE_STDOUT  text printed for a blocked call        FAKE_EXIT   exit code for a blocked call
#   FAKE_SLEEP   seconds every call (even reads) hangs first
T=${T:-$(mktemp -d)}; FAKE=$T/fakebin; FAKE_LOG=$T/calls.log
mkdir -p "$FAKE"; : > "$FAKE_LOG"; export T FAKE FAKE_LOG
for tool in asusctl nvidia-smi nvidia-settings powerprofilesctl hyprctl systemctl notify-send; do
  real=$(command -v "$tool" 2>/dev/null || true)
  sed -e "s|@TOOL@|$tool|g" -e "s|@REAL@|$real|g" > "$FAKE/$tool" <<'TPL'
#!/bin/sh
echo "@TOOL@ $*" >> "$FAKE_LOG"
[ -n "$FAKE_SLEEP" ] && sleep "$FAKE_SLEEP"
case "@TOOL@ $*" in
  "hyprctl monitors -j"|"nvidia-smi --query-gpu="*|"nvidia-smi -q -d POWER"|"nvidia-settings -q "*|\
  "systemctl --user is-enabled rog-control.service"|"powerprofilesctl get")
    [ -n "@REAL@" ] && exec "@REAL@" "$@";;
  "asusctl fan-curve --mod-profile "*)
    [ $# -eq 3 ] && [ -n "@REAL@" ] && exec "@REAL@" "$@";;
esac
[ -n "$FAKE_STDOUT" ] && printf '%s\n' "$FAKE_STDOUT"
exit "${FAKE_EXIT:-0}"
TPL
  chmod +x "$FAKE/$tool"
done
