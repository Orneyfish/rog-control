# Source me after fake-tools.sh:  . specs/rust-rewrite/harness/lib.sh
CTL_NAME=com.orneyfish.RogControl.Hotkeys CTL_PATH=/com/orneyfish/RogControl/Hotkeys

# wait_name <bus-name> [timeout_s=5]  -> 0 once the name has an owner on the current session bus
wait_name() {
  local end=$(( $(date +%s%N) / 1000000 + ${2:-5} * 1000 ))
  while [ $(( $(date +%s%N) / 1000000 )) -lt $end ]; do
    busctl --user list --no-legend 2>/dev/null | awk '{print $1}' | grep -qx "$1" && return 0
    sleep 0.02
  done; return 1
}
# ping_svc [timeout_s=5]  -> 0 if the control service answers on the main loop
ping_svc() { busctl --user --timeout="${1:-5}" call $CTL_NAME $CTL_PATH org.freedesktop.DBus.Peer Ping; }
# ctl <Method> [sig args...]  e.g.  ctl OpenWindow s fans
ctl() { busctl --user call $CTL_NAME $CTL_PATH $CTL_NAME "$@"; }
# win "<title>"  -> number of mapped ROG Control windows with that title on the live Hyprland session
win() { hyprctl clients -j | jq --arg t "$1" '[.[] | select((.class|startswith("com.orneyfish.RogControl")) and .title==$t)] | length'; }
# wait_win "<title>" <count> [timeout_s=2]  -> 0 once win() equals count
wait_win() {
  local end=$(( $(date +%s%N) / 1000000 + ${3:-2} * 1000 ))
  while [ $(( $(date +%s%N) / 1000000 )) -lt $end ]; do [ "$(win "$1")" = "$2" ] && return 0; sleep 0.05; done; return 1
}
# imgdiff <a.png> <b.png> -> "<AE3> <RMSE_small>" over the common top-left area:
#   AE3        = fraction of pixels differing by more than 3% colour
#   RMSE_small = RMSE of both images shrunk to 1/8 size (blind to 1px text noise, sees layout/icons/colour)
# Measured on the Python reference: re-shooting the same window gives AE3 <= 0.0013, RMSE_small <= 0.0101;
# a blank window gives >= 0.019 / 0.075, a 3px shift 0.0335 / 0.0527, a blanked icon row 0.0037 / 0.0292.
imgdiff() {
  local w1 h1 w2 h2 w h a3 r
  read w1 h1 <<<"$(magick identify -format '%w %h' "$1")"; read w2 h2 <<<"$(magick identify -format '%w %h' "$2")"
  w=$(( w1 < w2 ? w1 : w2 )); h=$(( h1 < h2 ? h1 : h2 ))
  a3=$(magick compare -metric AE -fuzz 3% "$1[${w}x${h}+0+0]" "$2[${w}x${h}+0+0]" null: 2>&1 | awk '{print $1}')
  r=$(magick \( "$1[${w}x${h}+0+0]" -resize 12.5% \) \( "$2[${w}x${h}+0+0]" -resize 12.5% \) -metric RMSE -compare -format '%[distortion]' info: 2>&1)
  awk -v a="$a3" -v t=$(( w * h )) -v r="$r" 'BEGIN { printf "%.4f %.4f\n", a / t, r }'
}
# imgsame <a.png> <b.png> -> exit 0 iff AE3 <= 0.015 and RMSE_small <= 0.02 (prints the two numbers)
imgsame() { local d; d=$(imgdiff "$1" "$2"); echo "$d"; echo "$d" | awk '{ exit !($1 <= 0.015 && $2 <= 0.02) }'; }
# sizeok <a.png> <b.png> -> exit 0 iff width and height are each within 4px
sizeok() { local a b; a=$(magick identify -format '%w %h' "$1"); b=$(magick identify -format '%w %h' "$2")
  echo "$a vs $b"; echo "$a $b" | awk '{ dw=$1-$3; dh=$2-$4; exit !((dw<5&&dw>-5)&&(dh<5&&dh>-5)) }'; }
# app [args]  -> runs the binary under test with the fake hardware tools on PATH.
#   HOME is $H (a throwaway copy of the config) unless APPHOME is set. Needs BIN, FAKE, H.
# In a subshell (app ... & pid=$!, $(app ...), a pipeline) it execs, so $! is the binary and `kill $pid` stops it.
app() { if [ "$BASHPID" != "$$" ]; then exec env PATH="$FAKE:$PATH" HOME="${APPHOME:-$H}" "$BIN" "$@"; else env PATH="$FAKE:$PATH" HOME="${APPHOME:-$H}" "$BIN" "$@"; fi; }
# closewin <hyprland-address>  -> closes that window like its title-bar x (Hyprland 0.57 Lua dispatcher; verified)
closewin() { hyprctl dispatch "hl.dsp.window.close({ window = \"address:$1\" })" >/dev/null; }
