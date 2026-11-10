#!/bin/sh
# usage: shoot.sh "<window title>" <out.png>   -> screenshots that ROG Control window on the live
# Hyprland session and prints "<w> <h> <x> <y> <address>". Exit 1 if no such window is mapped.
c=$(hyprctl clients -j | jq -c --arg t "$1" \
  '[.[] | select((.class|startswith("com.orneyfish.RogControl")) and .title==$t)][0]')
[ "$c" = null ] && { echo "window not found: $1" >&2; exit 1; }
set -- "$1" "$2" $(echo "$c" | jq -r '"\(.size[0]) \(.size[1]) \(.at[0]) \(.at[1]) \(.address)"')
grim -g "$5,$6 ${3}x${4}" "$2" && echo "$3 $4 $5 $6 $7"
