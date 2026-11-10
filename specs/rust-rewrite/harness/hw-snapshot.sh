#!/bin/sh
# Prints the hardware state the app can change (read-only). Run before the first eval and after the last; the outputs must be identical.
for f in /sys/firmware/acpi/platform_profile \
  /sys/class/firmware-attributes/asus-armoury/attributes/*/current_value \
  /sys/class/power_supply/BAT0/charge_control_end_threshold \
  /sys/class/leds/asus::kbd_backlight/brightness; do
  printf '%s=%s\n' "$f" "$(cat "$f" 2>/dev/null)"
done
asusctl fan-curve --get-enabled 2>/dev/null
powerprofilesctl get 2>/dev/null
hyprctl monitors -j 2>/dev/null | jq -c '.[] | {name, width, height, refreshRate}'
