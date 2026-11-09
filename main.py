#!/usr/bin/env python3
"""ROG Control: a background tray app for ASUS ROG laptop control (asusctl/supergfxctl),
styled to match this machine's caelestia-shell rice with a Material 3 skin, modeled
directly on the real G-Helper (https://github.com/seerge/g-helper) UI -- read from its
actual C# source, not just its README -- as closely as the Linux asus-wmi/asusctl
backend allows.

Structurally this follows G-Helper's real architecture: the main window is a single
vertically-stacked dashboard (no tabs, no sidebar -- G-Helper has neither; grep its
source and there is no TabControl anywhere), auto-sized to its content, with a live
inline status label in every section's header row. "Fans + Power" is a separate
companion window, matching G-Helper's own Fans.cs being a standalone Form rather than
a tab page.

No window is shown on launch -- it registers a StatusNotifierItem tray icon (shown in
caelestia-shell's own bar tray, next to Steam etc.) and left-click toggles the panel.
libayatana-appindicator was deliberately not used: it never exposes SNI's Activate
(left-click) callback to the app, only a right-click menu -- so the SNI item is
implemented directly here via dbus-python against org.kde.StatusNotifierWatcher
(confirmed present, owned by quickshell itself, on this system).

CLI flags (used by Hyprland hotkey binds, see ~/.config/hypr/custom/keybinds.lua) are
routed to the already-running instance via a small D-Bus method call (RogControlService
below) rather than GApplication's own remote-command-line machinery, which was tried
first and hung indefinitely on this system's session bus:
  --open                 toggle the popup panel (bound to the physical ROG key)
  --cycle-profile        switch to the next performance profile
  --cycle-profile-back   switch to the previous performance profile
No flags at all is what systemd's ExecStart uses at boot -- it starts the real service
and stays hidden in the tray until something explicitly activates it.

Attributes that G-Helper exposes but this hardware's asus-armoury reports "unavailable"
(ppt_pl1_spl, ppt_pl2_sppt, nv_dynamic_boost, nv_temp_target -- confirmed via
`asusctl armoury list` on this G733ZW) simply hide their rows rather than being faked.
G-Helper's charge_mode-less battery model, CPU P/E-core control, Windows-only power
plans, undervolting, AniMe Matrix/Slash/Ally/peripheral support, and driver-update
checker are out of scope here (no Windows registry/WMI surface, no matching hardware,
or no safe Linux equivalent within this backend).
"""
import json
import math
import os
import re
import shutil
import subprocess
import sys
import threading

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
from gi.repository import Adw, Gdk, Gio, GLib, Gtk

import dbus
import dbus.service
from dbus.mainloop.glib import DBusGMainLoop

import config

APP_ID = "com.orneyfish.RogControl"
APP_DIR = os.path.dirname(os.path.abspath(__file__))
ICONS_DIR = os.path.join(APP_DIR, "icons")
TRAY_ICON_NAME = "rog-control-symbolic"

SCHEME_JSON_PATH = os.path.expanduser("~/.local/state/caelestia/scheme.json")

PROFILES = ["Quiet", "Balanced", "Performance"]
PROFILE_LABELS = {"Quiet": "Silent", "Balanced": "Balanced", "Performance": "Turbo"}
PROFILE_ICONS = {"Quiet": "weather-clear-symbolic", "Balanced": "weather-few-clouds-symbolic", "Performance": "weather-storm-symbolic"}
PROFILE_HINTS = {
    "Quiet": "Lower power draw, quieter fans",
    "Balanced": "Default balance of power and noise",
    "Performance": "Maximum performance",
}
PLATFORM_PROFILE_PATH = "/sys/firmware/acpi/platform_profile"
BATTERY_LIMIT_PATH = "/sys/class/power_supply/BAT0/charge_control_end_threshold"
BATTERY_CAPACITY_PATH = "/sys/class/power_supply/BAT0/capacity"
BATTERY_STATUS_PATH = "/sys/class/power_supply/BAT0/status"
BATTERY_POWER_NOW_PATH = "/sys/class/power_supply/BAT0/power_now"
BATTERY_ENERGY_FULL_PATH = "/sys/class/power_supply/BAT0/energy_full"
BATTERY_ENERGY_FULL_DESIGN_PATH = "/sys/class/power_supply/BAT0/energy_full_design"
KBD_BRIGHTNESS_PATH = "/sys/class/leds/asus::kbd_backlight/brightness"
GPU_MUX_PATH = "/sys/class/firmware-attributes/asus-armoury/attributes/gpu_mux_mode/current_value"
DGPU_DISABLE_PATH = "/sys/class/firmware-attributes/asus-armoury/attributes/dgpu_disable/current_value"
PENDING_REBOOT_PATH = "/sys/class/firmware-attributes/asus-armoury/attributes/pending_reboot"
PANEL_OVERDRIVE_PATH = "/sys/class/firmware-attributes/asus-armoury/attributes/panel_overdrive/current_value"
BOOT_SOUND_PATH = "/sys/class/firmware-attributes/asus-armoury/attributes/boot_sound/current_value"
CHARGE_MODE_PATH = "/sys/class/firmware-attributes/asus-armoury/attributes/charge_mode/current_value"
PPT_PL1_PATH = "/sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl1_spl/current_value"
PPT_PL2_PATH = "/sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl2_sppt/current_value"
NV_BOOST_PATH = "/sys/class/firmware-attributes/asus-armoury/attributes/nv_dynamic_boost/current_value"
NV_TEMP_TARGET_PATH = "/sys/class/firmware-attributes/asus-armoury/attributes/nv_temp_target/current_value"
# Resolved by driver name, not hardcoded hwmonN indices -- those aren't
# guaranteed stable across boots/kernel versions, and weren't stable here:
# CPU_TEMP_PATH previously hardcoded hwmon4, which is actually "spd5118" (a
# RAM module's SPD temperature sensor reading ~44C) rather than the CPU
# (confirmed via `cat /sys/class/hwmon/hwmon*/name` -- the real CPU package
# sensor is the "coretemp" driver's temp1_input, ~56C). fan1/fan2 on the
# "asus" hwmon device happened to already be correct (self-labelled
# "cpu_fan"/"gpu_fan" by the driver) but are resolved the same safe way now.


def _resolve_hwmon_paths():
    coretemp_dir = asus_dir = None
    base = "/sys/class/hwmon"
    try:
        for entry in os.listdir(base):
            name_path = os.path.join(base, entry, "name")
            try:
                with open(name_path) as f:
                    name = f.read().strip()
            except OSError:
                continue
            if name == "coretemp" and coretemp_dir is None:
                coretemp_dir = os.path.join(base, entry)
            elif name == "asus" and asus_dir is None:
                asus_dir = os.path.join(base, entry)
    except OSError:
        pass
    cpu_temp = os.path.join(coretemp_dir, "temp1_input") if coretemp_dir else None
    cpu_fan = os.path.join(asus_dir, "fan1_input") if asus_dir else None
    gpu_fan = os.path.join(asus_dir, "fan2_input") if asus_dir else None
    return cpu_temp, cpu_fan, gpu_fan


CPU_TEMP_PATH, CPU_FAN_PATH, GPU_FAN_PATH = _resolve_hwmon_paths()

BRIGHTNESS_LEVELS = ["off", "low", "med", "high"]
BRIGHTNESS_LABELS = ["Off", "Low", "Medium", "High"]

# GPU mode semantics match G-Helper's Gpu/GPUModeControl.cs exactly -- Eco/Standard/
# Optimized all run with gpu_mux_mode=1 (iGPU drives the panel) and differ only in
# dgpu_disable; only Ultimate needs the MUX flipped to 0, which is the one that
# needs a reboot (G-Helper force-restarts the machine on confirm; this asks first
# via a dialog and calls `systemctl reboot`, never automatically).
GPU_MODES = ["Eco", "Standard", "Ultimate", "Optimized"]
# Exact wording from G-Helper's Strings.resx hover-hint labels.
GPU_HINTS = {
    "Eco": "Disables dGPU to save battery",
    "Standard": "Enables dGPU for standard use",
    "Ultimate": "Routes laptop screen to dGPU, maximizing FPS",
    "Optimized": "Switch to Eco on battery and to Standard when plugged in",
}
SCREEN_HINTS = {
    "auto": "Sets 60Hz to save battery, and back when plugged",
    "60": "Fixed at 60Hz",
    "max": "Fixed at the display's maximum refresh rate, with overdrive",
}
CHARGE_MODES = ["Full Capacity", "Balanced", "Max Lifespan"]
# effect name -> which extra fields it needs, per `asusctl aura effect <name> --help`.
# This is the full set asusctl supports; this hardware reports SupportedBasicZones
# as empty (confirmed via `busctl introspect` on the Aura object), meaning true
# per-key/multi-zone colour addressing isn't available on this exact keyboard --
# these effects apply across the whole keyboard as one zone.
AURA_EFFECTS = {
    "static": {"colour1"},
    "breathe": {"colour1", "colour2", "speed"},
    "pulse": {"colour1"},
    "rainbow-cycle": {"speed"},
    "rainbow-wave": {"direction", "speed"},
    "stars": {"colour1", "colour2", "speed"},
    "rain": {"speed"},
    "highlight": {"colour1", "speed"},
    "laser": {"colour1", "speed"},
    "ripple": {"colour1", "speed"},
    "comet": {"colour1"},
    "flash": {"colour1"},
}
DIRECTIONS = ["up", "down", "left", "right"]

# asusctl's CLI has no "get current effect" command, only set -- the only way to
# read back which mode/colours are actually active is asusd's own D-Bus LedMode/
# LedModeData properties (xyz.ljones.Aura, system bus). LedMode is a plain
# integer with no CLI-documented mapping, so this was derived empirically by
# setting each named effect in turn and reading back the resulting integer
# (confirmed: 0=static, 1=breathe, 2=rainbow-cycle, 3=rainbow-wave, 4=stars,
# 5=rain, 6=highlight, 7=laser, 8=ripple, 9=<unused/unsupported>, 10=pulse,
# 11=comet, 12=flash -- note pulse is NOT sequential with the rest).
AURA_MODE_INDEX = {
    "static": 0, "breathe": 1, "rainbow-cycle": 2, "rainbow-wave": 3, "stars": 4,
    "rain": 5, "highlight": 6, "laser": 7, "ripple": 8, "pulse": 10, "comet": 11, "flash": 12,
}
AURA_INDEX_MODE = {v: k for k, v in AURA_MODE_INDEX.items()}
# Hardware-specific object suffix (USB vendor/product/bus/port), discoverable
# via `busctl tree xyz.ljones.Asusd` if this ever runs on different hardware --
# consistent with the hwmon/sysfs paths above already being hardcoded to this
# exact machine rather than auto-discovered.
AURA_OBJECT_PATH = "/xyz/ljones/aura/19b6_3_3"

NOTIFY_SEND = shutil.which("notify-send")

# G-Helper's fixed, semantic per-concept accent colours (RButton.cs / colorEco etc)
# -- a deliberate exception to the Material You palette below: this is one of the
# app's clearest visual identities (teal=battery-friendly, blue=standard, red=max
# performance, orange=secondary/custom action) and stays constant across themes.
ACCENT_ECO = "#06B48A"
ACCENT_STANDARD = "#3AAEEF"
ACCENT_TURBO = "#FF2020"
ACCENT_CUSTOM = "#FF8000"
PROFILE_ACCENT = {"Quiet": ACCENT_ECO, "Balanced": ACCENT_STANDARD, "Performance": ACCENT_TURBO}
GPU_ACCENT = {"Eco": ACCENT_ECO, "Standard": ACCENT_STANDARD, "Ultimate": ACCENT_TURBO, "Optimized": ACCENT_ECO}

# Full Material 3 role set, matching matugen/caelestia's scheme.json keys 1:1 --
# used only if that file is missing (e.g. running this app off this machine).
FALLBACK_MATERIAL_COLORS = {
    "surface": "#1a1112",
    "surfaceDim": "#1a1112",
    "surfaceBright": "#413735",
    "surfaceContainerLowest": "#140c0d",
    "surfaceContainerLow": "#231819",
    "surfaceContainer": "#271c1d",
    "surfaceContainerHigh": "#322627",
    "surfaceContainerHighest": "#3d3132",
    "onSurface": "#f8e0e0",
    "onSurfaceVariant": "#bba6a6",
    "primary": "#f7b6b9",
    "onPrimary": "#603337",
    "primaryContainer": "#7a4a4d",
    "onPrimaryContainer": "#ffdadb",
    "secondary": "#e6bdbe",
    "onSecondary": "#55393a",
    "secondaryContainer": "#6d4f50",
    "onSecondaryContainer": "#ffdadb",
    "tertiary": "#e6bdbe",
    "onTertiary": "#55393a",
    "tertiaryContainer": "#6d4f50",
    "onTertiaryContainer": "#ffdadb",
    "outline": "#847172",
    "outlineVariant": "#524344",
    "error": "#f97758",
    "onError": "#450900",
    "errorContainer": "#852c0a",
    "onErrorContainer": "#ffdad2",
}


def read_sysfs(path, default=None):
    # A resolver like _resolve_hwmon_paths() can legitimately return None
    # (sensor not found on this hardware) -- open(None) raises TypeError,
    # not OSError, so that has to be guarded explicitly rather than falling
    # into the except below.
    if not path:
        return default
    try:
        with open(path) as f:
            return f.read().strip()
    except OSError:
        return default


def read_sysfs_int(path, default=None):
    v = read_sysfs(path)
    try:
        return int(v)
    except (TypeError, ValueError):
        return default


def run_cmd(cmd, on_done=None):
    """Fire-and-forget a command off the GTK main thread; optionally report failure."""

    def worker():
        try:
            result = subprocess.run(cmd, capture_output=True, text=True, timeout=10)
            ok = result.returncode == 0
            err = result.stderr.strip() if not ok else ""
        except (OSError, subprocess.TimeoutExpired) as e:
            ok, err = False, str(e)
        if on_done:
            GLib.idle_add(on_done, ok, err)

    threading.Thread(target=worker, daemon=True).start()


def notify_osd(title, body=""):
    """Desktop-notification feedback for hotkey/automation-triggered actions, since
    those fire without the panel necessarily being open."""
    if not NOTIFY_SEND:
        return
    cmd = [NOTIFY_SEND, "-a", "ROG Control", "-t", "2000", title]
    if body:
        cmd.append(body)
    run_cmd(cmd)


def device_name():
    name = read_sysfs("/sys/class/dmi/id/product_name", "")
    # DMI often duplicates the SKU after an underscore, e.g. "ROG Strix G733ZW_G733ZW".
    return name.split("_")[0].strip() if name else "This laptop"


def load_material_colors():
    try:
        with open(SCHEME_JSON_PATH) as f:
            data = json.load(f)
        colours = data.get("colours") or {}
        if colours.get("surface") and colours.get("primary"):
            return {k: (v if str(v).startswith("#") else f"#{v}") for k, v in colours.items()}
    except (OSError, ValueError):
        pass
    return FALLBACK_MATERIAL_COLORS


def theme_primary_hex():
    return load_material_colors().get("primary", "#f7b6b9").lstrip("#")


def theme_secondary_hex():
    return load_material_colors().get("secondary", "#e6bdbe").lstrip("#")


def build_material_css(c):
    """Material 3 colours/shapes rendered in G-Helper's actual layout language rather
    than M3's usual card-heavy patterns: G-Helper's real dashboard has no boxed
    section backgrounds at all (confirmed from source -- every panel sits directly
    on the window background, separated only by spacing), so this doesn't add any
    either. What carries the Material 3 identity here is the surface/on-surface
    tokens, tonal button fills, and M3 shape scale on every control."""

    def col(name):
        return c.get(name, FALLBACK_MATERIAL_COLORS.get(name, "#888888"))

    def rgba_css(hexval, a):
        r, g, b, _ = hex_to_rgba(hexval)
        return f"rgba({int(r * 255)}, {int(g * 255)}, {int(b * 255)}, {a})"

    return f"""
    window {{
        background-color: {col('surface')};
        color: {col('onSurface')};
    }}
    headerbar {{
        background-color: {col('surface')};
        color: {col('onSurface')};
        box-shadow: none;
        border-bottom: 1px solid {col('outlineVariant')};
        min-height: 40px;
    }}
    headerbar windowtitle title {{
        font-size: 95%;
        font-weight: 600;
    }}

    label.rog-title {{
        font-weight: 700;
        font-size: 100%;
    }}
    label.rog-subtitle {{
        color: {col('onSurfaceVariant')};
        font-size: 85%;
    }}
    label.rog-hint {{
        color: {col('onSurfaceVariant')};
        font-size: 80%;
        min-height: 1.4em;
    }}
    label.rog-empty {{
        color: {col('onSurfaceVariant')};
        font-size: 90%;
    }}
    label.rog-section {{
        color: {col('onSurfaceVariant')};
        font-weight: 700;
        font-size: 78%;
        letter-spacing: 0.5px;
    }}

    scale trough {{
        background-color: {col('surfaceContainerHighest')};
        border: 1px solid {col('outlineVariant')};
        border-radius: 999px;
        min-height: 8px;
    }}
    scale trough highlight {{
        background-color: {col('primary')};
        border-radius: 999px;
    }}
    scale slider {{
        background-color: {col('primary')};
        border-radius: 999px;
        min-width: 18px;
        min-height: 18px;
        margin: -6px;
    }}

    switch {{
        background-color: {col('surfaceContainerHighest')};
        border: 1px solid {col('outline')};
        border-radius: 999px;
    }}
    switch:checked {{
        background-color: {col('primary')};
        border-color: {col('primary')};
    }}
    switch slider {{
        background-color: {col('surfaceContainer')};
        border-radius: 999px;
    }}
    switch:checked slider {{
        background-color: {col('onPrimary')};
    }}

    button {{
        border-radius: 20px;
    }}
    button.suggested-action {{
        background-color: {col('primary')};
        color: {col('onPrimary')};
    }}
    button.destructive-action {{
        background-color: {col('error')};
        color: {col('onError')};
    }}
    button.rog-swatch {{
        border-radius: 999px;
        min-width: 24px;
        min-height: 24px;
        padding: 0;
        border: 1px solid {col('outline')};
    }}
    button.rog-pill {{
        border-radius: 999px;
        padding: 2px 12px;
        font-size: 85%;
        background-color: {col('surfaceContainerHighest')};
        color: {col('onSurfaceVariant')};
    }}

    dropdown button {{
        background-color: {col('surfaceContainerHigh')};
        color: {col('onSurface')};
        border: 1px solid {col('outlineVariant')};
        border-radius: 12px;
    }}

    /* Mode buttons: M3 tonal surface when unselected, fixed G-Helper accent glow
       (via one of the rog-accent-* classes) when selected -- see mode_button(). */
    button.rog-mode-btn {{
        background-color: {col('surfaceContainerHigh')};
        color: {col('onSurfaceVariant')};
        border: 1.5px solid transparent;
        border-radius: 14px;
        box-shadow: none;
        padding: 10px 4px;
    }}
    button.rog-mode-btn:hover {{
        background-color: {col('surfaceContainerHighest')};
    }}
    button.rog-mode-btn.rog-secondary {{
        color: {col('onSurfaceVariant')};
    }}
    button.rog-mode-btn:checked.rog-accent-eco {{
        background-color: {rgba_css(ACCENT_ECO, 0.18)};
        color: {col('onSurface')};
        border-color: {ACCENT_ECO};
        box-shadow: 0 0 10px {rgba_css(ACCENT_ECO, 0.55)};
    }}
    button.rog-mode-btn:checked.rog-accent-standard {{
        background-color: {rgba_css(ACCENT_STANDARD, 0.18)};
        color: {col('onSurface')};
        border-color: {ACCENT_STANDARD};
        box-shadow: 0 0 10px {rgba_css(ACCENT_STANDARD, 0.55)};
    }}
    button.rog-mode-btn:checked.rog-accent-turbo {{
        background-color: {rgba_css(ACCENT_TURBO, 0.18)};
        color: {col('onSurface')};
        border-color: {ACCENT_TURBO};
        box-shadow: 0 0 10px {rgba_css(ACCENT_TURBO, 0.55)};
    }}
    button.rog-mode-btn:checked {{
        background-color: {col('primaryContainer')};
        color: {col('onPrimaryContainer')};
    }}
    button.rog-mode-btn.rog-accent-custom {{
        color: {ACCENT_CUSTOM};
    }}

    togglebutton {{
        background-color: {col('surfaceContainerHigh')};
        color: {col('onSurfaceVariant')};
        border: 1px solid {col('outlineVariant')};
        border-radius: 12px;
        box-shadow: none;
    }}
    togglebutton:checked {{
        background-color: {col('primaryContainer')};
        color: {col('onPrimaryContainer')};
        border-color: transparent;
    }}

    spinbutton {{
        background-color: {col('surfaceContainerHigh')};
        border: 1px solid {col('outlineVariant')};
        border-radius: 12px;
    }}

    separator {{
        background-color: {col('outlineVariant')};
        min-height: 1px;
        margin: 6px 0;
    }}
    """


def register_icons():
    display = Gdk.Display.get_default()
    Gtk.IconTheme.get_for_display(display).add_search_path(ICONS_DIR)


def current_monitor():
    """The laptop's own internal panel specifically -- refresh rate and
    panel_overdrive are both laptop-panel-only features (an external
    monitor has neither), but this used to just grab monitors[0], whichever
    happened to enumerate first. Confirmed as a real bug with an external
    display connected: `hyprctl monitors -j` listed the external HDMI
    monitor before the internal eDP-1 panel, so every refresh-rate/
    overdrive change (manual or from screen_auto automation, which fires on
    every power-source transition) was silently targeting the *external*
    monitor's `hyprctl keyword monitor` rule instead -- reported as
    intermittent blackouts and a "monitor setup is incorrect" error with
    two monitors attached. Falls back to monitors[0] only if no eDP output
    is found at all (e.g. a docked/lid-closed setup with no internal panel
    reported), so this stays safe on that edge case too."""
    try:
        out = subprocess.run(["hyprctl", "monitors", "-j"], capture_output=True, text=True, timeout=5).stdout
        monitors = json.loads(out)
        if not monitors:
            return None
        internal = next((m for m in monitors if m.get("name", "").startswith("eDP")), None)
        return internal if internal is not None else monitors[0]
    except (OSError, subprocess.TimeoutExpired, ValueError, IndexError):
        return None


def max_refresh_rate():
    monitor = current_monitor()
    modes = monitor.get("availableModes", []) if monitor else []
    try:
        rates = [int(round(float(m.split("@")[1].rstrip("Hz")))) for m in modes]
    except (ValueError, IndexError):
        rates = []
    return max(rates) if rates else 60


# ---------------------------------------------------------------------------
# Shared apply functions -- used by both direct UI actions and AutomationManager,
# so "what happens when you pick Balanced" is defined exactly once.
# ---------------------------------------------------------------------------

# G-Helper pairs each BIOS performance mode with a matching Windows Power Mode
# (Silent+Best power efficiency, Balanced+Balanced, Turbo+Best performance --
# confirmed both from the README and directly from the user). power-profiles-
# daemon is the Linux equivalent surface (confirmed present and active on this
# system via `systemctl status power-profiles-daemon`).
PROFILE_POWER_PLAN = {"Quiet": "power-saver", "Balanced": "balanced", "Performance": "performance"}
POWERPROFILESCTL = shutil.which("powerprofilesctl")


def apply_profile(name, on_done=None):
    run_cmd(["asusctl", "profile", "set", name], on_done)
    if POWERPROFILESCTL:
        run_cmd([POWERPROFILESCTL, "set", PROFILE_POWER_PLAN[name]])


def cycle_profile(step, on_done=None):
    current = read_sysfs(PLATFORM_PROFILE_PATH, "balanced").capitalize()
    try:
        idx = PROFILES.index(current)
    except ValueError:
        idx = 0
    target = PROFILES[(idx + step) % len(PROFILES)]

    def done(ok, err):
        if ok:
            notify_osd("Performance mode", PROFILE_LABELS[target])
        if on_done:
            on_done(ok, err)

    apply_profile(target, done)


def current_gpu_mode(cfg):
    mux = read_sysfs_int(GPU_MUX_PATH, 0)
    if mux == 0:
        return "Ultimate"
    if cfg["automation"]["gpu_optimized"]:
        return "Optimized"
    return "Eco" if read_sysfs_int(DGPU_DISABLE_PATH, 0) == 1 else "Standard"


def gpu_mode_needs_reboot(mode):
    target_mux = 0 if mode == "Ultimate" else 1
    current_mux = read_sysfs_int(GPU_MUX_PATH)
    return current_mux is not None and current_mux != target_mux


def apply_gpu_mode(mode, on_battery, on_done=None):
    # Safety-critical: a MUX change only takes effect after an actual reboot
    # (that's the whole reason gpu_mode_needs_reboot()/the confirmation
    # dialog exist), but that dialog lives only in GpuPanel's manual-click
    # handler -- AutomationManager calls this function directly, with no
    # dialog possible from a background component. If a mux write were
    # followed by immediately also touching dgpu_disable in the same call
    # (as an earlier version of this function did), that would disable the
    # dGPU while it might *still* be the only thing driving the display
    # (the real running mux state doesn't change until reboot) -- a real
    # risk of blanking the screen. So: when a mux change is actually needed,
    # do ONLY that and stop; dgpu_disable is only ever touched once the real
    # mux already matches the target (i.e. after the user has rebooted).
    target_mux = 0 if mode == "Ultimate" else 1
    current_mux = read_sysfs_int(GPU_MUX_PATH)
    if current_mux != target_mux:
        run_cmd(["asusctl", "armoury", "set", "gpu_mux_mode", str(target_mux)], on_done)
        return
    if mode == "Ultimate":
        if on_done:
            on_done(True, "")
        return
    disable = "1" if (mode == "Eco" or (mode == "Optimized" and on_battery)) else "0"
    if disable == "1" and external_display_on_dgpu():
        # Confirmed real risk, not hypothetical: an external monitor wired
        # directly to the dGPU (e.g. this laptop's HDMI port) has no iGPU
        # fallback -- disabling the dGPU would blank it outright, regardless
        # of gpu_mux_mode. Refuse rather than silently disconnecting
        # whatever the user is actively looking at.
        if on_done:
            on_done(False, "An external display is connected directly to the dGPU -- disabling it would disconnect that screen")
        return
    run_cmd(["asusctl", "armoury", "set", "dgpu_disable", disable], on_done)


def gpu_in_heavy_use():
    """Best-effort check mirroring G-Helper's HardwareControl.IsUsedGPU() guard --
    Optimized mode shouldn't silently kill the dGPU mid-render when switching to
    battery. Utilization is the simplest reliable signal without needing to
    enumerate process handles."""
    try:
        out = subprocess.run(
            ["nvidia-smi", "--query-gpu=utilization.gpu", "--format=csv,noheader,nounits"],
            capture_output=True, text=True, timeout=5,
        ).stdout.strip()
        return int(out) > 20
    except (OSError, subprocess.TimeoutExpired, ValueError):
        return False


def external_display_on_dgpu():
    """True if a non-internal-panel display is currently connected directly
    to the NVIDIA dGPU, bypassing the MUX entirely. Confirmed on this exact
    hardware: an external monitor plugged into the HDMI port shows up as
    connector `HDMI-A-1` under the dGPU's own DRM card (vendor 0x10de), a
    completely separate path from the MUX-switched internal panel -- so
    disabling the dGPU would blank that external display outright,
    regardless of gpu_mux_mode/dgpu_disable's usual "no reboot needed"
    safety, with no iGPU fallback for that specific port. This is the gap
    that let a user's external-monitor setup get silently disconnected by
    Optimized-mode-on-battery automation before this check existed."""
    base = "/sys/class/drm"
    try:
        entries = os.listdir(base)
    except OSError:
        return False
    for entry in entries:
        if not entry.startswith("card") or "-" not in entry:
            continue
        card_name = entry.split("-", 1)[0]
        vendor = read_sysfs(os.path.join(base, card_name, "device", "vendor"))
        if vendor != "0x10de":  # NVIDIA's PCI vendor ID -- fixed across all their GPUs
            continue
        connector = entry[len(card_name) + 1:]
        if connector.startswith("eDP"):
            continue  # internal panel routed through the dGPU is expected, not "external"
        if read_sysfs(os.path.join(base, entry, "status")) == "connected":
            return True
    return False


# NVIDIA GPU overclocking (Core/Memory Clock Offset, GPU Power/TGP) -- matches
# G-Helper's Fans GPU tab. Targets NVAPI on Windows; the Linux equivalent is
# nvidia-settings' NV-CONTROL protocol (clock offsets, per performance level --
# perf=3 is this GPU's top P-state, confirmed via `nvidia-settings -q
# GPUPerfModes`) and nvidia-smi (power limit). Both are probed and hidden if
# unsupported rather than presented broken -- confirmed on this exact laptop
# GPU (mobile RTX 3070 Ti): GPUGraphicsClockOffset/GPUMemoryTransferRateOffset
# don't exist at all under a pure-Wayland session with no Xorg+Coolbits, and
# `nvidia-smi -pl` is rejected outright ("not supported in current scope") --
# a vBIOS-level lock common on laptop GPUs, not something software can unlock.
NV_PERF_LEVEL = 3


def nvidia_settings_query_ok(attr):
    try:
        result = subprocess.run(["nvidia-settings", "-q", attr], capture_output=True, text=True, timeout=5)
        return "ERROR" not in result.stdout and "ERROR" not in result.stderr
    except (OSError, subprocess.TimeoutExpired):
        return False


def nvidia_clock_offset_available():
    return bool(shutil.which("nvidia-settings")) and nvidia_settings_query_ok(f"[gpu:0]/GPUGraphicsClockOffset[{NV_PERF_LEVEL}]")


def apply_core_clock_offset(mhz, on_done=None):
    run_cmd(["nvidia-settings", "-a", f"[gpu:0]/GPUGraphicsClockOffset[{NV_PERF_LEVEL}]={mhz}"], on_done)


def apply_mem_clock_offset(mhz, on_done=None):
    run_cmd(["nvidia-settings", "-a", f"[gpu:0]/GPUMemoryTransferRateOffset[{NV_PERF_LEVEL}]={mhz}"], on_done)


def nvidia_power_limit_bounds():
    """(current, min, max) watts, or None if the GPU doesn't report them."""
    try:
        out = subprocess.run(["nvidia-smi", "-q", "-d", "POWER"], capture_output=True, text=True, timeout=5).stdout
    except (OSError, subprocess.TimeoutExpired):
        return None
    cur = re.search(r"Current Power Limit\s*:\s*([\d.]+)\s*W", out)
    lo = re.search(r"Min Power Limit\s*:\s*([\d.]+)\s*W", out)
    hi = re.search(r"Max Power Limit\s*:\s*([\d.]+)\s*W", out)
    return (float(cur.group(1)), float(lo.group(1)), float(hi.group(1))) if (cur and lo and hi) else None


def _nvidia_smi_pl(watts, timeout=5):
    """`nvidia-smi -pl` returns exit code 0 even when it explicitly refuses
    the change (confirmed by direct testing: prints "Changing power
    management limit is not supported in current scope" but still exits 0),
    so success has to be read from the output text, not the return code."""
    try:
        result = subprocess.run(
            ["nvidia-smi", "-pl", str(int(round(watts)))], capture_output=True, text=True, timeout=timeout,
        )
        ok = result.returncode == 0 and "not supported" not in result.stdout.lower()
        err = "" if ok else (result.stdout.strip() or result.stderr.strip())
        return ok, err
    except (OSError, subprocess.TimeoutExpired) as e:
        return False, str(e)


def nvidia_power_limit_settable(watts):
    """One-off no-op probe (sets the limit back to its current value): confirms
    whether `nvidia-smi -pl` is actually honoured rather than just accepted."""
    ok, _err = _nvidia_smi_pl(watts)
    return ok


def apply_gpu_power_limit(watts, on_done=None):
    def worker():
        ok, err = _nvidia_smi_pl(watts, timeout=10)
        if on_done:
            GLib.idle_add(on_done, ok, err)

    threading.Thread(target=worker, daemon=True).start()


def read_aura_state():
    """Current keyboard RGB mode/colours/speed/direction, read straight from
    asusd's own D-Bus property rather than assumed from whatever this app last
    set -- so the panel reflects reality even after e.g. a physical Fn-combo
    change or an asusctl CLI call made outside this app."""
    try:
        bus = dbus.SystemBus()
        obj = bus.get_object("xyz.ljones.Asusd", AURA_OBJECT_PATH)
        props = dbus.Interface(obj, "org.freedesktop.DBus.Properties")
        mode, _zone, colour1, colour2, speed, direction = props.Get("xyz.ljones.Aura", "LedModeData")
        return {
            "effect": AURA_INDEX_MODE.get(int(mode), "static"),
            "colour1": "".join(f"{int(c):02x}" for c in colour1),
            "colour2": "".join(f"{int(c):02x}" for c in colour2),
            "speed": str(speed).lower(),
            "direction": str(direction).lower(),
        }
    except dbus.DBusException:
        return None


def hex_to_gdk_rgba(hexval):
    rgba = Gdk.RGBA()
    rgba.parse(f"#{hexval.lstrip('#')}")
    return rgba


def gdk_rgba_to_hex(rgba):
    return f"{round(rgba.red * 255):02x}{round(rgba.green * 255):02x}{round(rgba.blue * 255):02x}"


def color_picker_button(initial_hex):
    """A native GTK colour picker (full RGB, not a fixed swatch palette) -- G-
    Helper's own keyboard colour control is a plain button that opens a colour
    dialog, and this hardware's keyboard is a single addressable zone (no
    per-key colour) so one picker per colour slot is the right amount of UI."""
    btn = Gtk.ColorDialogButton()
    dialog = Gtk.ColorDialog()
    dialog.set_with_alpha(False)
    btn.set_dialog(dialog)
    btn.set_rgba(hex_to_gdk_rgba(initial_hex))
    return btn


def apply_screen_preset(preset, on_done=None):
    """preset: 'auto-battery' (60Hz, overdrive off), 'auto-ac' (max Hz, overdrive
    on), '60', or 'max' (max Hz + overdrive on, matching G-Helper's single combined
    '<Hz> + OD' button).

    Applies the mode change via `hyprctl eval "hl.monitor({...})"`, not `hyprctl
    keyword monitor ...` -- this Hyprland setup uses a Lua config (hyprland.lua),
    and `keyword` is a runtime hook into the classic keyword-based config parser
    only; confirmed directly that it no-ops with exit code 0 and stderr "keyword
    can't work with non-legacy parsers. Use eval." on this machine, meaning every
    refresh-rate/overdrive change (manual or from screen_auto automation) was
    silently doing nothing -- run_cmd only checks the return code, so this
    never even surfaced as an error toast. `hl.monitor(...)` is the Lua-config
    equivalent (same function nwg-displays' generated monitors.lua calls) and
    is confirmed working via direct testing. Position/scale are passed through
    unchanged from the live readback -- this function only ever touches mode,
    never rearranges the user's monitor layout."""
    hz = 60 if preset in ("60", "auto-battery") else max_refresh_rate()
    overdrive = "0" if preset in ("60", "auto-battery") else "1"

    def after_refresh(ok, err):
        if not ok:
            if on_done:
                on_done(ok, err)
            return
        run_cmd(["asusctl", "armoury", "set", "panel_overdrive", overdrive], on_done)

    monitor = current_monitor()
    if not monitor:
        if on_done:
            on_done(False, "could not read monitor info")
        return
    res = f"{monitor['width']}x{monitor['height']}"
    pos = f"{monitor['x']}x{monitor['y']}"
    name = monitor["name"].replace('"', '')
    lua = (
        f'hl.monitor({{output = "{name}", mode = "{res}@{hz}", '
        f'position = "{pos}", scale = {monitor["scale"]}}})'
    )
    if read_sysfs_int(PANEL_OVERDRIVE_PATH) is None:
        run_cmd(["hyprctl", "eval", lua], on_done)
    else:
        run_cmd(["hyprctl", "eval", lua], after_refresh)


def apply_kbd_level(level, on_done=None):
    run_cmd(["asusctl", "leds", "set", level], on_done)


def battery_charging_text():
    status = read_sysfs(BATTERY_STATUS_PATH, "")
    power_now = read_sysfs_int(BATTERY_POWER_NOW_PATH)
    if status == "Full":
        return "Full"
    if power_now:
        watts = power_now / 1_000_000
        verb = "Charging" if status == "Charging" else "Discharging"
        return f"{verb}: {watts:.1f}W"
    return status or "--"


def battery_health_text():
    full = read_sysfs_int(BATTERY_ENERGY_FULL_PATH)
    design = read_sysfs_int(BATTERY_ENERGY_FULL_DESIGN_PATH)
    if not full or not design:
        return None
    return f"Battery Health: {full / design * 100:.1f}%"


# ---------------------------------------------------------------------------
# Small reusable widgets
# ---------------------------------------------------------------------------

def section_label(text):
    lbl = Gtk.Label(label=text)
    lbl.set_halign(Gtk.Align.START)
    lbl.add_css_class("rog-section")
    return lbl


def section_header(icon_name, title):
    """Icon + bold title on the left, with a live-status label on the right --
    G-Helper puts the sensor/state readout inline in the title row itself, not a
    separate stats panel."""
    row = Gtk.Box(spacing=8)
    icon = Gtk.Image.new_from_icon_name(icon_name)
    title_lbl = Gtk.Label(label=title)
    title_lbl.add_css_class("rog-title")
    stat_lbl = Gtk.Label(label="")
    stat_lbl.add_css_class("rog-subtitle")
    stat_lbl.set_halign(Gtk.Align.END)
    stat_lbl.set_hexpand(True)
    row.append(icon)
    row.append(title_lbl)
    row.append(stat_lbl)
    row.title_label = title_lbl
    row.stat_label = stat_lbl
    return row


def hint_label():
    lbl = Gtk.Label(label="")
    lbl.set_halign(Gtk.Align.START)
    lbl.add_css_class("rog-hint")
    return lbl


def add_hover_hint(widget, hint, text):
    motion = Gtk.EventControllerMotion()
    motion.connect("enter", lambda *_: hint.set_label(text))
    motion.connect("leave", lambda *_: hint.set_label(""))
    widget.add_controller(motion)


def mode_button(icon_name, label, accent_class=None, secondary=False, group_with=None):
    btn = Gtk.ToggleButton() if not secondary else Gtk.Button()
    btn.add_css_class("rog-mode-btn")
    if accent_class:
        btn.add_css_class(accent_class)
    if secondary:
        btn.add_css_class("rog-secondary")
    if group_with is not None and hasattr(btn, "set_group"):
        btn.set_group(group_with)
    content = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=4, halign=Gtk.Align.CENTER)
    image = Gtk.Image.new_from_icon_name(icon_name)
    image.set_pixel_size(20)
    text = Gtk.Label(label=label)
    content.append(image)
    content.append(text)
    btn.set_child(content)
    btn.set_hexpand(True)
    return btn


def switch_row(text, handler):
    row = Gtk.Box(spacing=8)
    label = Gtk.Label(label=text)
    label.set_hexpand(True)
    label.set_halign(Gtk.Align.START)
    switch = Gtk.Switch()
    switch.set_valign(Gtk.Align.CENTER)
    switch.connect("state-set", handler)
    row.append(label)
    row.append(switch)
    row.switch = switch
    return row


def confirm_dialog(parent, heading, body, ok_label, on_confirm, destructive=True):
    dialog = Adw.AlertDialog(heading=heading, body=body)
    dialog.add_response("cancel", "Cancel")
    dialog.add_response("ok", ok_label)
    if destructive:
        dialog.set_response_appearance("ok", Adw.ResponseAppearance.DESTRUCTIVE)
    dialog.set_default_response("cancel")
    dialog.set_close_response("cancel")

    def on_response(_d, response):
        if response == "ok":
            on_confirm()

    dialog.connect("response", on_response)
    dialog.present(parent)


class Debouncer:
    """Coalesces rapid repeated triggers into one call, fired after `delay_ms`
    of quiet. Fixes a real race condition (confirmed by direct testing):
    changing colour1, colour2, and speed in quick succession -- exactly what
    dragging a colour-dialog control does -- each fired its own independent
    `asusctl aura effect ...` subprocess call sending the *full* effect state
    read at that instant. With no ordering guarantee between background
    threads, the hardware's final state depended on whichever call happened
    to *finish* last, not the last one the user actually picked, so a quick
    multi-field edit landed in an unpredictable combination -- reported as
    "RGB doesn't work". Debouncing means only the last change in a burst ever
    turns into a real command, reading fully-settled widget state."""

    def __init__(self, delay_ms, callback):
        self.delay_ms = delay_ms
        self.callback = callback
        self._source = None

    def trigger(self):
        if self._source is not None:
            GLib.source_remove(self._source)
        self._source = GLib.timeout_add(self.delay_ms, self._fire)

    def _fire(self):
        self._source = None
        self.callback()
        return False


# ---------------------------------------------------------------------------
# Main window panels
# ---------------------------------------------------------------------------

class PerformancePanel(Gtk.Box):
    def __init__(self, toast_cb, app_cfg, save_cfg, get_on_battery, open_fans_cb):
        super().__init__(orientation=Gtk.Orientation.VERTICAL, spacing=6)
        self.toast_cb = toast_cb
        self.app_cfg = app_cfg
        self.save_cfg = save_cfg
        self.get_on_battery = get_on_battery

        self.header = section_header("speedometer-symbolic", "Performance Mode")
        self.append(self.header)

        row = Gtk.Box(spacing=6, homogeneous=True, hexpand=True)
        self.hint = hint_label()
        self._btns = {}
        group = None
        for name in PROFILES:
            btn = mode_button(PROFILE_ICONS[name], PROFILE_LABELS[name], accent_class={
                ACCENT_ECO: "rog-accent-eco", ACCENT_STANDARD: "rog-accent-standard", ACCENT_TURBO: "rog-accent-turbo",
            }[PROFILE_ACCENT[name]], group_with=group)
            group = group or btn
            btn.connect("toggled", self._on_profile_toggled, name)
            add_hover_hint(btn, self.hint, PROFILE_HINTS[name])
            row.append(btn)
            self._btns[name] = btn
        fans_btn = mode_button("weather-windy-symbolic", "Fans + Power", accent_class="rog-accent-custom", secondary=True)
        fans_btn.connect("clicked", lambda *_: open_fans_cb())
        add_hover_hint(fans_btn, self.hint, "Custom power limits and fan curves per mode")
        row.append(fans_btn)

        self.append(row)
        self.append(self.hint)

        self._suppress = False
        self.refresh()

    def refresh(self):
        self._suppress = True
        current = read_sysfs(PLATFORM_PROFILE_PATH, "balanced").capitalize()
        btn = self._btns.get(current)
        if btn:
            btn.set_active(True)

        cpu_temp = read_sysfs_int(CPU_TEMP_PATH)
        cpu_fan = read_sysfs_int(CPU_FAN_PATH)
        temp_s = f"{cpu_temp / 1000:.0f}°C" if cpu_temp else "--"
        fan_s = f"{cpu_fan} RPM" if cpu_fan is not None else "--"
        self.header.stat_label.set_label(f"CPU: {temp_s}  Fan: {fan_s}")
        self._suppress = False

    def _on_profile_toggled(self, btn, name):
        if self._suppress or not btn.get_active():
            return
        on_battery = self.get_on_battery()
        key = "profile_battery" if on_battery else "profile_ac"
        self.app_cfg["automation"][key] = name
        self.save_cfg()
        apply_profile(name, self._report)

    def _report(self, ok, err):
        if not ok:
            self.toast_cb(f"Command failed: {err[:80]}")


class GpuPanel(Gtk.Box):
    def __init__(self, toast_cb, app_cfg, save_cfg, get_on_battery, get_root):
        super().__init__(orientation=Gtk.Orientation.VERTICAL, spacing=6)
        self.toast_cb = toast_cb
        self.app_cfg = app_cfg
        self.save_cfg = save_cfg
        self.get_on_battery = get_on_battery
        self.get_root = get_root
        self._last_gpu_temp_label = "--"

        self.header = section_header("video-display-symbolic", "GPU Mode")
        self.append(self.header)

        # "leaf-symbolic"/"flower-symbolic" don't exist in this icon theme
        # (confirmed via Gtk.IconTheme.has_icon) and rendered blank -- these are
        # the real GNOME power-profile icon names, which exist because
        # power-profiles-daemon is active on this system.
        icons = {
            "Eco": "power-profile-power-saver-symbolic",
            "Standard": "power-profile-balanced-symbolic",
            "Ultimate": "input-gaming-symbolic",
            "Optimized": "power-profile-performance-symbolic",
        }
        accent_class = {ACCENT_ECO: "rog-accent-eco", ACCENT_STANDARD: "rog-accent-standard", ACCENT_TURBO: "rog-accent-turbo"}
        row = Gtk.Box(spacing=6, homogeneous=True, hexpand=True)
        self.hint = hint_label()
        self._btns = {}
        group = None
        for name in GPU_MODES:
            icon = icons.get(name, "video-display-symbolic")
            btn = mode_button(icon, name, accent_class=accent_class[GPU_ACCENT[name]], group_with=group)
            group = group or btn
            btn.connect("toggled", self._on_gpu_toggled, name)
            add_hover_hint(btn, self.hint, GPU_HINTS[name])
            row.append(btn)
            self._btns[name] = btn
        self.append(row)
        self.append(self.hint)

        self.reboot_notice = Gtk.Label(label="")
        self.reboot_notice.add_css_class("rog-subtitle")
        self.reboot_notice.set_halign(Gtk.Align.START)
        self.append(self.reboot_notice)

        self._suppress = False
        self.refresh()

    def refresh(self):
        self._suppress = True
        mode = current_gpu_mode(self.app_cfg)
        btn = self._btns.get(mode)
        if btn:
            btn.set_active(True)
        pending = read_sysfs(PENDING_REBOOT_PATH, "0") == "1"
        self.reboot_notice.set_label("Reboot required to apply a pending GPU mode change" if pending else "")

        # Fan RPM is a cheap synchronous sysfs read, so it updates immediately;
        # GPU temp needs an nvidia-smi subprocess call, so it updates
        # asynchronously via _set_gpu_temp below. Composing the label from the
        # last-known temp here (instead of blanking it to a "--" placeholder
        # first) avoids a real bug: every single 2-second poll was flickering
        # the label to "GPU: --" and back as the async fetch raced the
        # placeholder, confirmed by the user as "GPU:Temp flickers and
        # doesn't update".
        self._update_stat_label()
        self._refresh_gpu_temp()
        self._suppress = False

    def _update_stat_label(self):
        gpu_fan = read_sysfs_int(GPU_FAN_PATH)
        fan_s = f"{gpu_fan} RPM" if gpu_fan is not None else "--"
        self.header.stat_label.set_label(f"GPU: {self._last_gpu_temp_label}  Fan: {fan_s}")

    def _refresh_gpu_temp(self):
        def worker():
            try:
                out = subprocess.run(
                    ["nvidia-smi", "--query-gpu=temperature.gpu", "--format=csv,noheader"],
                    capture_output=True, text=True, timeout=5,
                ).stdout.strip()
            except (OSError, subprocess.TimeoutExpired):
                out = ""
            GLib.idle_add(self._set_gpu_temp, out)

        threading.Thread(target=worker, daemon=True).start()

    def _set_gpu_temp(self, temp):
        self._last_gpu_temp_label = f"{temp}°C" if temp else "--"
        self._update_stat_label()

    def _on_gpu_toggled(self, btn, name):
        if self._suppress or not btn.get_active():
            return
        if gpu_mode_needs_reboot(name):
            turning_on = name == "Ultimate"
            confirm_dialog(
                self.get_root(), "Reboot now?",
                "Ultimate Mode requires restart" if turning_on else "Switching off Ultimate Mode requires restart",
                "Reboot", lambda: self._apply(name, reboot=True),
            )
            # Nothing applied yet -- resync the buttons to the real (unchanged) state.
            GLib.idle_add(self.refresh)
        else:
            self._apply(name, reboot=False)

    def _apply(self, name, reboot):
        self.app_cfg["automation"]["gpu_optimized"] = (name == "Optimized")
        self.save_cfg()

        def done(ok, err):
            if not ok:
                self.toast_cb(f"GPU mode failed: {err[:80]}")
            elif reboot:
                run_cmd(["systemctl", "reboot"])
            self.refresh()

        apply_gpu_mode(name, bool(self.get_on_battery()), done)


class ScreenPanel(Gtk.Box):
    def __init__(self, toast_cb, app_cfg, save_cfg):
        super().__init__(orientation=Gtk.Orientation.VERTICAL, spacing=6)
        self.toast_cb = toast_cb
        self.app_cfg = app_cfg
        self.save_cfg = save_cfg

        self.header = section_header("preferences-desktop-display-symbolic", "Screen")
        self.append(self.header)

        row = Gtk.Box(spacing=6, homogeneous=True, hexpand=True)
        self.hint = hint_label()
        self._btns = {}
        group = None
        for key, label in (("auto", "Auto"), ("60", "60Hz"), ("max", f"{max_refresh_rate()}Hz + OD")):
            btn = mode_button("emblem-synchronizing-symbolic" if key == "auto" else "video-display-symbolic", label, group_with=group)
            group = group or btn
            btn.connect("toggled", self._on_toggled, key)
            add_hover_hint(btn, self.hint, SCREEN_HINTS[key])
            row.append(btn)
            self._btns[key] = btn
        self.append(row)
        self.append(self.hint)

        self._suppress = False
        self.refresh()

    def _current_key(self):
        if self.app_cfg["automation"]["screen_auto"]:
            return "auto"
        monitor = current_monitor()
        hz = int(round(monitor.get("refreshRate", 0))) if monitor else 0
        return "60" if hz <= 60 else "max"

    def refresh(self):
        self._suppress = True
        btn = self._btns.get(self._current_key())
        if btn:
            btn.set_active(True)
        self._suppress = False

    def _on_toggled(self, btn, key):
        if self._suppress or not btn.get_active():
            return
        self.app_cfg["automation"]["screen_auto"] = (key == "auto")
        self.save_cfg()
        if key == "auto":
            self.toast_cb("Screen: auto (60Hz on battery, max Hz when plugged)")
            return
        apply_screen_preset(key, self._report)

    def _report(self, ok, err):
        if not ok:
            self.toast_cb(f"Command failed: {err[:80]}")


class KeyboardPanel(Gtk.Box):
    def __init__(self, toast_cb, app_cfg, save_cfg, get_on_battery, open_lighting_cb):
        super().__init__(orientation=Gtk.Orientation.VERTICAL, spacing=6)
        # Set before any widget/signal exists -- see the matching note in
        # LightingWindow.__init__.
        self._suppress = True
        self.toast_cb = toast_cb
        self.app_cfg = app_cfg
        self.save_cfg = save_cfg
        self.get_on_battery = get_on_battery
        self._apply_debouncer = Debouncer(150, self._apply_effect)

        self.append(section_header("input-keyboard-symbolic", "Laptop Keyboard"))

        row = Gtk.Box(spacing=6, homogeneous=True, hexpand=True)
        self._btns = {}
        group = None
        for level, label in zip(BRIGHTNESS_LEVELS, BRIGHTNESS_LABELS):
            btn = Gtk.ToggleButton(label=label, hexpand=True)
            btn.add_css_class("rog-mode-btn")
            if group is None:
                group = btn
            else:
                btn.set_group(group)
            btn.connect("toggled", self._on_brightness_toggled, level)
            row.append(btn)
            self._btns[level] = btn
        self.append(row)

        # Compact RGB quick controls, matching G-Helper's real main-window row
        # (effect combo | colour swatch | Extra button for direction/speed/2nd
        # colour) rather than burying effect switching a window deep.
        rgb_row = Gtk.Box(spacing=6, hexpand=True)
        self.effect_dropdown = Gtk.DropDown.new_from_strings(list(AURA_EFFECTS.keys()))
        self.effect_dropdown.set_hexpand(True)
        self.effect_dropdown.connect("notify::selected", self._on_effect_changed)
        self.colour_btn = color_picker_button(theme_primary_hex())
        self.colour_btn.connect("notify::rgba", self._on_colour_changed)
        extra_btn = Gtk.Button(label="Extra")
        extra_btn.add_css_class("rog-pill")
        extra_btn.connect("clicked", lambda *_: open_lighting_cb())
        rgb_row.append(self.effect_dropdown)
        rgb_row.append(self.colour_btn)
        rgb_row.append(extra_btn)
        self.append(rgb_row)

        self.refresh()

    def refresh(self):
        self._suppress = True
        idx = read_sysfs_int(KBD_BRIGHTNESS_PATH, 0)
        level = BRIGHTNESS_LEVELS[idx] if idx is not None and 0 <= idx < len(BRIGHTNESS_LEVELS) else "off"
        btn = self._btns.get(level)
        if btn:
            btn.set_active(True)

        # Reflect whatever is actually active on the keyboard right now --
        # colours default to the desktop's Material You palette only when
        # asusd has no state to report at all (e.g. daemon not running yet).
        state = read_aura_state()
        effects = list(AURA_EFFECTS.keys())
        effect = state["effect"] if state else "static"
        if effect in effects:
            self.effect_dropdown.set_selected(effects.index(effect))
        self.colour_btn.set_rgba(hex_to_gdk_rgba(state["colour1"] if state else theme_primary_hex()))
        self._suppress = False

    def _on_brightness_toggled(self, btn, level):
        if self._suppress or not btn.get_active():
            return
        # Implicit automation, no opt-in checkbox, matching G-Helper's own keyboard-
        # backlight behaviour: just remember this level for whichever power source
        # is currently active, and AutomationManager restores it on transitions.
        key = "kbd_battery" if self.get_on_battery() else "kbd_ac"
        self.app_cfg["automation"][key] = level
        self.save_cfg()
        apply_kbd_level(level, self._report)

    def _on_effect_changed(self, *_a):
        if self._suppress:
            return
        self._apply_debouncer.trigger()

    def _on_colour_changed(self, *_a):
        if self._suppress:
            return
        self._apply_debouncer.trigger()

    def _apply_effect(self):
        effect = list(AURA_EFFECTS.keys())[self.effect_dropdown.get_selected()]
        needs = AURA_EFFECTS[effect]
        colour1 = gdk_rgba_to_hex(self.colour_btn.get_rgba())
        # This compact row only has controls for effect + colour1 -- colour2/
        # direction/speed must come from whatever's actually active (as set via
        # the "Extra" Lighting window), not hardcoded, or picking a new effect
        # here would silently overwrite a custom colour2/speed the user set
        # there with defaults every time.
        state = read_aura_state()
        colour2 = state["colour2"] if state else theme_secondary_hex()
        direction = state["direction"] if state and state["direction"] in DIRECTIONS else "right"
        speed = state["speed"] if state and state["speed"] in ("low", "med", "high") else "med"
        cmd = ["asusctl", "aura", "effect", effect]
        if "colour1" in needs:
            cmd += ["-c" if effect in ("static", "pulse", "highlight", "laser", "ripple", "comet", "flash") else "--colour", colour1]
        if "colour2" in needs:
            cmd += ["--colour2", colour2]
        if "direction" in needs:
            cmd += ["--direction", direction]
        if "speed" in needs:
            cmd += ["--speed", speed]
        run_cmd(cmd, lambda ok, err: (None if ok else self.toast_cb(f"Effect failed: {err[:80]}")))

    def _report(self, ok, err):
        if not ok:
            self.toast_cb(f"Command failed: {err[:80]}")


class BatteryPanel(Gtk.Box):
    def __init__(self, toast_cb):
        super().__init__(orientation=Gtk.Orientation.VERTICAL, spacing=6)
        self.toast_cb = toast_cb

        self.header = section_header("battery-good-symbolic", "Battery Charge Limit")
        self.append(self.header)

        row = Gtk.Box(spacing=8)
        # G-Helper's own slider is 40-100, but asusctl's real supported range on
        # this hardware is 20-100 (confirmed via `asusctl battery --help`) --
        # that backend fact wins over copying G-Helper's Windows-specific UI
        # range, which was silently clamping a real value below 40 (e.g. a
        # deliberately-set 20%) to 40 on refresh while the title label kept
        # showing the true unclamped number, and would then push that wrong
        # clamped value to hardware on the next drag.
        self.scale = Gtk.Scale.new_with_range(Gtk.Orientation.HORIZONTAL, 20, 100, 5)
        self.scale.set_draw_value(True)
        self.scale.set_value_pos(Gtk.PositionType.RIGHT)
        self.scale.set_hexpand(True)
        self.scale.connect("value-changed", self._on_changed)
        self.oneshot_btn = Gtk.Button(label="100%")
        self.oneshot_btn.add_css_class("rog-pill")
        self.oneshot_btn.connect("clicked", self._on_oneshot)
        motion = Gtk.EventControllerMotion()
        motion.connect("enter", self._show_health)
        motion.connect("leave", self._show_limit_label)
        self.oneshot_btn.add_controller(motion)
        row.append(self.scale)
        row.append(self.oneshot_btn)
        self.append(row)

        self._suppress = False
        self.refresh()

    def _show_limit_label(self, *_a):
        self.oneshot_btn.set_label("100%")
        self.oneshot_btn.set_tooltip_text("One time charge to 100%")

    def _show_health(self, *_a):
        health = battery_health_text()
        if health:
            self.oneshot_btn.set_tooltip_text(health)

    def refresh(self):
        self._suppress = True
        limit = read_sysfs_int(BATTERY_LIMIT_PATH, 100)
        self.scale.set_value(limit)
        capacity = read_sysfs_int(BATTERY_CAPACITY_PATH)
        self.header.title_label.set_label(f"Battery Charge Limit: {limit}%")
        self.header.stat_label.set_label(battery_charging_text())
        self._show_limit_label()
        self._suppress = False

    def _on_changed(self, scale):
        if self._suppress:
            return
        value = int(scale.get_value())
        self.header.title_label.set_label(f"Battery Charge Limit: {value}%")
        run_cmd(["asusctl", "battery", "limit", str(value)], self._report)

    def _on_oneshot(self, _btn):
        run_cmd(["asusctl", "battery", "oneshot"], self._report)

    def _report(self, ok, err):
        if not ok:
            self.toast_cb(f"Command failed: {err[:80]}")
        else:
            self.refresh()


class StartupPanel(Gtk.Box):
    def __init__(self, toast_cb):
        super().__init__(orientation=Gtk.Orientation.VERTICAL, spacing=4)
        self.toast_cb = toast_cb
        self.row = switch_row("Run on Startup", self._on_toggled)
        # This is a checkbox in G-Helper's own designer; a switch reads better in
        # this app's control language, same on/off semantics.
        self.append(self.row)
        self._suppress = False
        self.refresh()

    def refresh(self):
        self._suppress = True
        try:
            out = subprocess.run(
                ["systemctl", "--user", "is-enabled", "rog-control.service"],
                capture_output=True, text=True, timeout=5,
            ).stdout.strip()
            self.row.switch.set_active(out == "enabled")
        except (OSError, subprocess.TimeoutExpired):
            pass
        self._suppress = False

    def _on_toggled(self, switch, state):
        if self._suppress:
            return False
        run_cmd(["systemctl", "--user", "enable" if state else "disable", "rog-control.service"], self._report)
        return False

    def _report(self, ok, err):
        if not ok:
            self.toast_cb(f"Command failed: {err[:80]}")


class FooterPanel(Gtk.Box):
    def __init__(self, on_extra, on_quit):
        super().__init__(orientation=Gtk.Orientation.VERTICAL, spacing=4)
        version_lbl = Gtk.Label(label="ROG Control · asusctl backend")
        version_lbl.add_css_class("rog-subtitle")
        version_lbl.set_halign(Gtk.Align.START)
        self.append(version_lbl)

        row = Gtk.Box(spacing=6, homogeneous=True, hexpand=True)
        extra_btn = Gtk.Button(label="Firmware")
        extra_btn.connect("clicked", lambda *_: on_extra())
        quit_btn = Gtk.Button(label="Quit")
        quit_btn.add_css_class("destructive-action")
        quit_btn.connect("clicked", lambda *_: on_quit())
        row.append(extra_btn)
        row.append(quit_btn)
        self.append(row)


def hex_to_rgba(hexval, alpha=1.0):
    hexval = hexval.lstrip("#")
    r = int(hexval[0:2], 16) / 255
    g = int(hexval[2:4], 16) / 255
    b = int(hexval[4:6], 16) / 255
    return r, g, b, alpha


class FanCurveGraph(Gtk.DrawingArea):
    """A draggable temp->speed% curve, styled after G-Helper's real fan chart
    (RChart : System.Windows.Forms.DataVisualization.Charting.Chart -- confirmed
    from source: SeriesChartType.Line, MarkerStyle.Circle, 8 points, X=20-110C,
    Y=0-100%). asusctl's fan-curve model is percent-based, not RPM, so this stays
    on a percent axis rather than G-Helper's RPM one -- a genuine backend
    difference, not a simplification. Mouse-up auto-applies, matching G-Helper's
    'commits both curves on release' behaviour, via on_drag_end."""

    TEMP_MIN, TEMP_MAX = 20, 100
    SPEED_MIN, SPEED_MAX = 0, 100
    PAD_L, PAD_R, PAD_T, PAD_B = 34, 12, 12, 22

    HIT_RADIUS = 16

    def __init__(self, on_point_selected=None, on_drag_end=None):
        super().__init__()
        self.on_point_selected = on_point_selected
        self.on_drag_end = on_drag_end
        self.points = [(40, 20), (50, 30), (55, 40), (60, 50), (65, 60), (70, 75), (80, 90), (95, 100)]
        self.enabled = True
        self._dragging = None
        self._drag_origin = None
        colours = load_material_colors()
        self.line_rgba = hex_to_rgba(colours.get("primary", "#f7b6b9"))
        self.point_rgba = hex_to_rgba(colours.get("onSurface", "#f8e0e0"))
        self.grid_rgba = hex_to_rgba(colours.get("onSurfaceVariant", "#bba6a6"), 0.18)
        self.axis_rgba = hex_to_rgba(colours.get("onSurfaceVariant", "#bba6a6"), 0.6)
        self.inert_rgba = hex_to_rgba(colours.get("onSurfaceVariant", "#bba6a6"), 0.4)

        self.set_content_width(380)
        self.set_content_height(180)
        self.set_hexpand(True)
        self.set_vexpand(False)
        self.set_draw_func(self._draw)

        drag = Gtk.GestureDrag()
        drag.connect("drag-begin", self._on_drag_begin)
        drag.connect("drag-update", self._on_drag_update)
        drag.connect("drag-end", self._on_drag_end_gesture)
        self.add_controller(drag)

    def set_points(self, points):
        self.points = list(points)
        self.queue_draw()

    def set_enabled(self, enabled):
        self.enabled = enabled
        self.queue_draw()

    def _geometry(self):
        w = self.get_width() or self.get_content_width()
        h = self.get_height() or self.get_content_height()
        return w, h

    def _to_px(self, temp, speed, w, h):
        x = self.PAD_L + (temp - self.TEMP_MIN) / (self.TEMP_MAX - self.TEMP_MIN) * (w - self.PAD_L - self.PAD_R)
        y = h - self.PAD_B - (speed - self.SPEED_MIN) / (self.SPEED_MAX - self.SPEED_MIN) * (h - self.PAD_T - self.PAD_B)
        return x, y

    def _from_px(self, x, y, w, h):
        temp = self.TEMP_MIN + (x - self.PAD_L) / (w - self.PAD_L - self.PAD_R) * (self.TEMP_MAX - self.TEMP_MIN)
        speed = self.SPEED_MIN + (h - self.PAD_B - y) / (h - self.PAD_T - self.PAD_B) * (self.SPEED_MAX - self.SPEED_MIN)
        return temp, speed

    def _draw(self, _area, cr, w, h):
        cr.select_font_face("sans-serif")
        cr.set_font_size(9)
        cr.set_source_rgba(*self.grid_rgba)
        cr.set_line_width(1)
        for i in range(5):
            y = self.PAD_T + i * (h - self.PAD_T - self.PAD_B) / 4
            cr.move_to(self.PAD_L, y)
            cr.line_to(w - self.PAD_R, y)
            speed = self.SPEED_MAX - i * (self.SPEED_MAX - self.SPEED_MIN) / 4
            cr.move_to(2, y + 3)
            cr.show_text(f"{int(speed)}")
        for i in range(5):
            x = self.PAD_L + i * (w - self.PAD_L - self.PAD_R) / 4
            cr.move_to(x, self.PAD_T)
            cr.line_to(x, h - self.PAD_B)
            temp = self.TEMP_MIN + i * (self.TEMP_MAX - self.TEMP_MIN) / 4
            cr.move_to(x - 6, h - 6)
            cr.show_text(f"{int(temp)}")
        cr.stroke()

        line_rgba = self.line_rgba if self.enabled else self.inert_rgba
        point_rgba = self.point_rgba if self.enabled else self.inert_rgba
        cr.set_source_rgba(*line_rgba)
        cr.set_line_width(2.5)
        for i, (t, s) in enumerate(self.points):
            x, y = self._to_px(t, s, w, h)
            cr.line_to(x, y) if i else cr.move_to(x, y)
        cr.stroke()

        for i, (t, s) in enumerate(self.points):
            x, y = self._to_px(t, s, w, h)
            radius = 6 if i == self._dragging else 4
            cr.arc(x, y, radius, 0, 2 * math.pi)
            cr.set_source_rgba(*point_rgba)
            cr.fill_preserve()
            cr.set_source_rgba(*line_rgba)
            cr.set_line_width(2)
            cr.stroke()

    def _nearest_point(self, x, y, w, h):
        best_i, best_d = None, self.HIT_RADIUS
        for i, (t, s) in enumerate(self.points):
            px, py = self._to_px(t, s, w, h)
            d = math.hypot(px - x, py - y)
            if d < best_d:
                best_i, best_d = i, d
        return best_i

    def _on_drag_begin(self, gesture, start_x, start_y):
        w, h = self._geometry()
        self._dragging = self._nearest_point(start_x, start_y, w, h)
        self._drag_origin = (start_x, start_y)
        self.queue_draw()

    def _on_drag_update(self, gesture, offset_x, offset_y):
        if self._dragging is None:
            return
        w, h = self._geometry()
        start_x, start_y = self._drag_origin
        temp, speed = self._from_px(start_x + offset_x, start_y + offset_y, w, h)

        i = self._dragging
        lo = self.points[i - 1][0] + 1 if i > 0 else self.TEMP_MIN
        hi = self.points[i + 1][0] - 1 if i < len(self.points) - 1 else self.TEMP_MAX
        temp = max(lo, min(hi, round(temp)))
        # asusctl rejects a curve where speed isn't monotonically
        # non-decreasing with temperature ("Invalid percentage, previous
        # value X is higher than next value Y") -- confirmed directly, and
        # only the temp axis was being clamped between neighbours before,
        # so dragging a point's speed above a later point's (or below an
        # earlier one's) silently produced a curve the backend would refuse
        # to apply, surfacing as an unexplained "Fan curve failed" toast.
        speed_lo = self.points[i - 1][1] if i > 0 else self.SPEED_MIN
        speed_hi = self.points[i + 1][1] if i < len(self.points) - 1 else self.SPEED_MAX
        speed = max(speed_lo, min(speed_hi, round(speed)))
        self.points[i] = (int(temp), int(speed))
        self.queue_draw()
        if self.on_point_selected:
            self.on_point_selected(i, self.points[i])

    def _on_drag_end_gesture(self, gesture, offset_x, offset_y):
        was_dragging = self._dragging is not None
        self._dragging = None
        self.queue_draw()
        if was_dragging and self.on_drag_end:
            self.on_drag_end()


def labeled_slider(text, lo, hi, unit="W"):
    box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=2)
    label_row = Gtk.Box(spacing=8)
    label = Gtk.Label(label=text, halign=Gtk.Align.START, hexpand=True)
    value_lbl = Gtk.Label(label=f"{lo}{unit}")
    value_lbl.add_css_class("rog-subtitle")
    label_row.append(label)
    label_row.append(value_lbl)
    scale = Gtk.Scale.new_with_range(Gtk.Orientation.HORIZONTAL, lo, hi, 1)
    scale.set_draw_value(False)
    scale.connect("value-changed", lambda s: value_lbl.set_label(f"{int(s.get_value())}{unit}"))
    box.append(label_row)
    box.append(scale)
    box.scale = scale
    box.value_label = value_lbl
    return box


class FanCurveEditor(Gtk.Box):
    """One fan's curve + its enable/reset controls, shared shape for CPU and GPU."""

    def __init__(self, fan, toast_cb, get_profile):
        super().__init__(orientation=Gtk.Orientation.VERTICAL, spacing=8)
        self.fan = fan
        self.toast_cb = toast_cb
        self.get_profile = get_profile

        self.append(section_label(f"{fan.upper()} Fan Profile, %/°C"))
        self.graph = FanCurveGraph(on_point_selected=self._on_point_dragged, on_drag_end=self._auto_apply)
        self.append(self.graph)
        self.point_label = Gtk.Label(label="Drag a point to edit it", halign=Gtk.Align.START)
        self.point_label.add_css_class("rog-subtitle")
        self.append(self.point_label)

        self.enable_row = switch_row("Apply Custom Fan Curve", self._on_enable_toggled)
        self.append(self.enable_row)

        btn_row = Gtk.Box(spacing=6, homogeneous=True, hexpand=True)
        reset_btn = Gtk.Button(label="Factory Defaults", hexpand=True)
        reset_btn.connect("clicked", self._reset)
        btn_row.append(reset_btn)
        self.append(btn_row)

        self._suppress = False
        self.load_curve()

    def load_curve(self):
        fan = self.fan
        profile = self.get_profile()

        def worker():
            try:
                # `--get-enabled` only ever returns the *currently active*
                # profile's curve, regardless of which profile you ask about --
                # confirmed empirically, it silently ignores any profile
                # selection. `--mod-profile <Name>` with no other flags is the
                # actual per-profile query (per `asusctl fan-curve --help`:
                # "profile to modify fan-curve for. shows data if no options
                # provided"), so that's what's needed to make the mode dropdown
                # actually change what's shown.
                out = subprocess.run(
                    ["asusctl", "fan-curve", "--mod-profile", profile],
                    capture_output=True, text=True, timeout=10,
                ).stdout
            except (OSError, subprocess.TimeoutExpired):
                out = ""
            points, enabled = self._parse_curve(out, fan)
            GLib.idle_add(self._populate, points, enabled)

        threading.Thread(target=worker, daemon=True).start()

    @staticmethod
    def _parse_curve(text, fan):
        # e.g.:
        # (
        #     fan: CPU,
        #     pwm: (64, 77, 94, 117, 125, 148, 168, 255),
        #     temp: (47, 51, 55, 59, 63, 68, 75, 97),
        #     enabled: false,
        # ),
        marker = f"fan: {fan.upper()}"
        idx = text.find(marker)
        if idx != -1:
            chunk = text[idx:idx + 400]
            pwm_m = re.search(r"pwm:\s*\(([^)]*)\)", chunk)
            temp_m = re.search(r"temp:\s*\(([^)]*)\)", chunk)
            enabled_m = re.search(r"enabled:\s*(true|false)", chunk)
            if pwm_m and temp_m and enabled_m:
                pwms = [int(x) for x in pwm_m.group(1).split(",") if x.strip()]
                temps = [int(x) for x in temp_m.group(1).split(",") if x.strip()]
                if pwms and temps:
                    points = [(t, round(p / 255 * 100)) for t, p in zip(temps, pwms)]
                    return points, enabled_m.group(1) == "true"
        return [(40, 20), (50, 30), (55, 40), (60, 50), (65, 60), (70, 75), (80, 90), (95, 100)], False

    def _populate(self, points, enabled):
        self._suppress = True
        self.graph.set_points(points)
        self.graph.set_enabled(enabled)
        self.point_label.set_label("Drag a point to edit it")
        self.enable_row.switch.set_active(enabled)
        self._suppress = False

    def _on_point_dragged(self, index, point):
        temp, speed = point
        self.point_label.set_label(f"Point {index + 1}: {temp}°C → {speed}%")

    def _auto_apply(self):
        # Mirrors G-Helper: mouse-up on a dragged point commits and pushes the
        # curve live immediately, no separate "Apply" button needed.
        data = ",".join(f"{t}c:{s}%" for t, s in self.graph.points)
        run_cmd(
            ["asusctl", "fan-curve", "--mod-profile", self.get_profile(), "--fan", self.fan, "--data", data],
            lambda ok, err: (None if ok else self.toast_cb(f"Fan curve failed: {err[:80]}")),
        )

    def _on_enable_toggled(self, switch, state):
        if self._suppress:
            return False
        self.graph.set_enabled(state)
        run_cmd(
            ["asusctl", "fan-curve", "--mod-profile", self.get_profile(), "--enable-fan-curve",
             "true" if state else "false", "--fan", self.fan],
            lambda ok, err: (None if ok else self.toast_cb(f"Enable failed: {err[:80]}")),
        )
        return False

    def _reset(self, _btn):
        # asusctl's own --help is explicit that --default resets "the active
        # profile's fan curve" -- --mod-profile is accepted alongside it but
        # silently ignored for this specific operation (confirmed directly:
        # running --mod-profile Quiet --default while Performance was active
        # left Quiet's curve completely untouched). Refuse rather than
        # silently resetting the wrong (active) profile when the one being
        # edited here isn't it.
        profile = self.get_profile()
        active = read_sysfs(PLATFORM_PROFILE_PATH, "").capitalize()
        if profile != active:
            self.toast_cb(f"Can't reset {profile}'s curve: only the active profile ({active}) can be reset to default")
            return
        run_cmd(
            ["asusctl", "fan-curve", "--mod-profile", profile, "--fan", self.fan, "--default"],
            lambda ok, err: (self.load_curve() if ok else self.toast_cb(f"Reset failed: {err[:80]}")),
        )


class FansPowerWindow(Adw.Window):
    """Companion window, matching G-Helper's Fans.cs being a standalone Form (not a
    tab in the main window). PL1/PL2/GPU-boost/temp-target are shown once, not per
    performance mode: unlike the fan curve (genuinely stored per-profile by
    asusctl), this hardware's asus-armoury power-limit attributes are a single
    live value with no per-profile storage to switch between -- and they read as
    "unavailable" on this exact G733ZW regardless, so the whole block just hides."""

    def __init__(self, app, toast_cb, cfg, save_cfg):
        super().__init__(application=app)
        self.toast_cb = toast_cb
        self.limits_cfg = cfg["power_limits"]
        self.save_cfg = save_cfg
        # Bigger than before -- the previous 440x620 was cramped enough that
        # slider value labels (e.g. "PL1 ... 10W") were getting clipped at
        # the right edge, reported directly as "not scaled properly".
        self.set_default_size(560, 760)
        self.set_title("Fans and Power")

        toolbar_view = Adw.ToolbarView()
        header = Adw.HeaderBar(title_widget=Adw.WindowTitle(title="Fans and Power", subtitle=device_name()))
        toolbar_view.add_top_bar(header)
        self.set_content(toolbar_view)

        outer = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=10)
        outer.set_margin_top(12)
        outer.set_margin_bottom(14)
        outer.set_margin_start(14)
        outer.set_margin_end(14)

        # The fan curve is genuinely per-profile (asusctl stores a separate
        # curve per Silent/Balanced/Turbo); the power limits below are not (a
        # single live value on this hardware, no per-profile storage). This
        # selector only means anything for the Fan Curve sub-view -- reported
        # directly by the user as confusing when it sat unconditionally above
        # *both* Power and Fan Curve ("doesn't say what [curve] is the
        # profile" / "shouldn't even be there" while looking at Power). Kept
        # as one shared widget (both CPU and GPU fan curves read the same
        # selection) and reparented into whichever fan page is actually
        # showing, so it only exists right above the chart it actually
        # affects -- see _update_mode_selector_visibility().
        mode_row = Gtk.Box(spacing=8)
        mode_row.append(Gtk.Label(label="Fan curve for profile:", halign=Gtk.Align.START))
        self.mode_dropdown = Gtk.DropDown.new_from_strings([PROFILE_LABELS[p] for p in PROFILES])
        self.mode_dropdown.set_hexpand(True)
        self.mode_dropdown.connect("notify::selected", self._on_mode_changed)
        mode_row.append(self.mode_dropdown)
        self.mode_row = mode_row
        self.mode_row.set_visible(False)

        nav_row = Gtk.Box(spacing=6, homogeneous=True, hexpand=True)
        self.cpu_nav = Gtk.ToggleButton(label="CPU", active=True, hexpand=True)
        self.gpu_nav = Gtk.ToggleButton(label="GPU", hexpand=True)
        self.gpu_nav.set_group(self.cpu_nav)
        nav_row.append(self.cpu_nav)
        nav_row.append(self.gpu_nav)
        outer.append(nav_row)

        self.stack = Gtk.Stack()
        self.cpu_nav.connect("toggled", lambda b: b.get_active() and self.stack.set_visible_child_name("cpu"))
        self.gpu_nav.connect("toggled", lambda b: b.get_active() and self.stack.set_visible_child_name("gpu"))
        self.cpu_nav.connect("toggled", lambda *_a: self._update_mode_selector_visibility())
        self.gpu_nav.connect("toggled", lambda *_a: self._update_mode_selector_visibility())

        # -- CPU page: decoupled Power / Fan Curve sub-views, not stacked
        # together in one scroll -- they're unrelated settings that happened to
        # share a page before, which read as "a single [combined] one".
        cpu_page, self.cpu_power_page, self.cpu_fan_page, self.cpu_fan_btn = self._sub_view_page("cpu")
        self.cpu_fan_btn.connect("toggled", lambda *_a: self._update_mode_selector_visibility())

        # G-Helper's "Windows Power Mode" combo, here backed by power-profiles-
        # daemon (the confirmed-active Linux equivalent) instead of a Windows
        # power plan -- this is the same pairing apply_profile() sets
        # automatically when you pick Silent/Balanced/Turbo, exposed here too as
        # its own control since the two aren't always meant to move in lockstep
        # (e.g. Balanced BIOS mode with a manually-forced power-saver plan).
        power_mode_row = Gtk.Box(spacing=8)
        power_mode_row.append(Gtk.Label(label="Power Mode", hexpand=True, halign=Gtk.Align.START))
        self.power_mode_dropdown = Gtk.DropDown.new_from_strings(["Power Saver", "Balanced", "Performance"])
        self.power_mode_dropdown.set_hexpand(True)
        self.power_mode_dropdown.connect("notify::selected", self._on_power_mode_changed)
        power_mode_row.append(self.power_mode_dropdown)
        self.power_mode_row = power_mode_row
        self.cpu_power_page.append(power_mode_row)

        # G-Helper labels these SPL/sPPT on AMD Ryzen and PL1/PL2 on Intel; this
        # laptop is Intel (i9-12900H). Always shown and interactive even though
        # this exact model's firmware reports them unavailable (confirmed via
        # `asusctl armoury list`) -- hiding them outright was too conservative;
        # a write attempt against unsupported firmware just reports a clear
        # toast error instead, same as any other control here can fail.
        self.pl1 = labeled_slider("PL1 (CPU sustained)", 10, 90)
        self.pl1.scale.connect("value-changed", self._on_pl1_changed)
        self.pl2 = labeled_slider("PL2 (CPU long boost)", 10, 150)
        self.pl2.scale.connect("value-changed", self._on_pl2_changed)
        self.apply_limits_row = switch_row("Apply Power Limits", self._on_apply_limits_toggled)
        self.power_limits_box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=10)
        self.power_limits_box.append(section_label("Power Limits"))
        self.power_limits_box.append(self.pl1)
        self.power_limits_box.append(self.pl2)
        self.power_limits_box.append(self.apply_limits_row)
        self.cpu_power_page.append(self.power_limits_box)

        self.cpu_fan = FanCurveEditor("cpu", toast_cb, self._current_profile)
        self.cpu_fan_page.append(self.cpu_fan)
        self.stack.add_named(cpu_page, "cpu")

        # -- GPU page: same Power / Fan Curve split --
        gpu_page, self.gpu_power_page, self.gpu_fan_page, self.gpu_fan_btn = self._sub_view_page("gpu")
        self.gpu_fan_btn.connect("toggled", lambda *_a: self._update_mode_selector_visibility())

        # Same reasoning as CPU: always shown, never hidden behind an
        # availability probe. Core/Memory Clock Offset and GPU Power are
        # confirmed unsupported specifically on THIS GPU/session (mobile RTX
        # 3070 Ti under pure-Wayland Hyprland with no Xorg+Coolbits, and a
        # vBIOS-level power-limit lock) -- that's real, but it's still this
        # laptop's limitation to hit when the user actually tries, not this
        # app's decision to make for them by hiding the control.
        self.core_clock = labeled_slider("Core Clock Offset", -200, 300, unit=" MHz")
        self.core_clock.scale.connect("value-changed", self._on_core_clock_changed)
        self.mem_clock = labeled_slider("Memory Clock Offset (VRAM)", -500, 1500, unit=" MHz")
        self.mem_clock.scale.connect("value-changed", self._on_mem_clock_changed)
        self.gpu_power = labeled_slider("GPU Power (TGP)", 1, 150, unit="W")
        self.gpu_power.scale.connect("value-changed", self._on_gpu_power_changed)
        self.boost = labeled_slider("Dynamic Boost", 0, 25)
        self.boost.scale.connect("value-changed", self._on_boost_changed)
        self.temp_target = labeled_slider("Thermal Target", 60, 100, unit="°C")
        self.temp_target.scale.connect("value-changed", self._on_temp_changed)
        self.gpu_unsupported_notice = Gtk.Label(
            label="Core/Memory Clock Offset and GPU Power need Xorg + Coolbits and/or\n"
                  "firmware support this laptop's GPU doesn't have -- shown anyway in\n"
                  "case that changes; a failed change reports why below.",
        )
        self.gpu_unsupported_notice.add_css_class("rog-empty")
        self.gpu_unsupported_notice.set_halign(Gtk.Align.START)
        self.gpu_limits_box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=10)
        self.gpu_limits_box.append(section_label("GPU Settings"))
        self.gpu_limits_box.append(self.core_clock)
        self.gpu_limits_box.append(self.mem_clock)
        self.gpu_limits_box.append(self.gpu_power)
        self.gpu_limits_box.append(self.boost)
        self.gpu_limits_box.append(self.temp_target)
        self.gpu_limits_box.append(self.gpu_unsupported_notice)
        self.gpu_power_page.append(self.gpu_limits_box)

        self.gpu_fan = FanCurveEditor("gpu", toast_cb, self._current_profile)
        self.gpu_fan_page.append(self.gpu_fan)
        self.stack.add_named(gpu_page, "gpu")

        scroller = Gtk.ScrolledWindow()
        scroller.set_child(self.stack)
        scroller.set_vexpand(True)
        outer.append(scroller)

        toolbar_view.set_content(outer)

        self._suppress = False
        self._nvidia_probe_done = False
        self._nvidia_clock_ok = False
        self._nvidia_power_bounds = None
        self._nvidia_power_ok = False

        # Gtk.Scale fires "value-changed" continuously through a drag (every
        # pixel of movement), not once at release -- without debouncing, each
        # of these fired its own concurrent asusctl/nvidia-smi/nvidia-settings
        # subprocess call, and a burst of them hitting asusd at once was
        # confirmed to make some queue long enough to hit run_cmd's 10s
        # timeout and surface as a "Command failed... timed out" toast, even
        # though the same command run alone completes in well under a second.
        # Same fix already proven for the RGB controls' equivalent race.
        self._pl1_debouncer = Debouncer(150, self._apply_pl1)
        self._pl2_debouncer = Debouncer(150, self._apply_pl2)
        self._core_clock_debouncer = Debouncer(150, self._apply_core_clock)
        self._mem_clock_debouncer = Debouncer(150, self._apply_mem_clock)
        self._gpu_power_debouncer = Debouncer(150, self._apply_gpu_power)
        self._boost_debouncer = Debouncer(150, self._apply_boost)
        self._temp_debouncer = Debouncer(150, self._apply_temp)

        self.refresh_power_limits()
        self._update_mode_selector_visibility()

    def _update_mode_selector_visibility(self):
        """Keeps the shared profile selector parented inside whichever fan
        page (CPU or GPU) is actually visible, and hidden entirely while
        looking at a Power page. Both fan pages want the exact same widget
        instance (one profile selection drives both fan curves), so this
        reparents it via remove+append rather than duplicating it."""
        active_fan_page = None
        if self.cpu_nav.get_active() and self.cpu_fan_btn.get_active():
            active_fan_page = self.cpu_fan_page
        elif self.gpu_nav.get_active() and self.gpu_fan_btn.get_active():
            active_fan_page = self.gpu_fan_page

        if active_fan_page is None:
            self.mode_row.set_visible(False)
            return

        parent = self.mode_row.get_parent()
        if parent is not active_fan_page:
            if parent is not None:
                parent.remove(self.mode_row)
            active_fan_page.prepend(self.mode_row)
        self.mode_row.set_visible(True)

    @staticmethod
    def _sub_view_page(name_prefix):
        """A Power/Fan Curve segmented toggle over a two-page Gtk.Stack --
        decouples the two so they read as distinct sections instead of one
        continuously-scrolling page. Returns (outer_box, power_page, fan_page,
        fan_btn); caller appends its own content into the two page boxes and
        can watch fan_btn to know when this page's Fan Curve view is active."""
        outer = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=10)

        nav = Gtk.Box(spacing=6, homogeneous=True, hexpand=True)
        power_btn = Gtk.ToggleButton(label="Power", active=True, hexpand=True)
        fan_btn = Gtk.ToggleButton(label="Fan Curve", hexpand=True)
        fan_btn.set_group(power_btn)
        nav.append(power_btn)
        nav.append(fan_btn)
        outer.append(nav)

        sub_stack = Gtk.Stack()
        power_page = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=10)
        fan_page = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=10)
        sub_stack.add_named(power_page, f"{name_prefix}-power")
        sub_stack.add_named(fan_page, f"{name_prefix}-fan")
        power_btn.connect("toggled", lambda b: b.get_active() and sub_stack.set_visible_child_name(f"{name_prefix}-power"))
        fan_btn.connect("toggled", lambda b: b.get_active() and sub_stack.set_visible_child_name(f"{name_prefix}-fan"))
        outer.append(sub_stack)

        return outer, power_page, fan_page, fan_btn

    def refresh(self):
        self.refresh_power_limits()
        self.cpu_fan.load_curve()
        self.gpu_fan.load_curve()

    def _current_profile(self):
        return PROFILES[self.mode_dropdown.get_selected()]

    def _on_mode_changed(self, *_a):
        self.cpu_fan.load_curve()
        self.gpu_fan.load_curve()

    def _sync_slider(self, slider, sysfs_path, cfg_key):
        """Prefer the real hardware reading; when there isn't one (firmware
        reports the attribute unavailable), fall back to whatever the user
        last set here rather than the slider's construction-time default --
        this window is destroyed and rebuilt fresh every time it's closed
        and reopened, so without this the slider always reset and looked
        like "everything resets" even within the same run of the app."""
        value = read_sysfs_int(sysfs_path)
        if value is not None:
            slider.scale.set_value(value)
        elif self.limits_cfg[cfg_key] is not None:
            slider.scale.set_value(self.limits_cfg[cfg_key])

    def refresh_power_limits(self):
        # Only fast, synchronous sysfs reads happen directly here -- everything
        # that shells out (power-profiles-daemon, nvidia-settings, nvidia-smi)
        # runs on a background thread instead. This used to block window
        # construction for several seconds (confirmed: a test harness window
        # never appeared at all until the nvidia probes finished), the same
        # freeze this app's own `run_cmd`/thread pattern elsewhere exists to
        # avoid -- this function just wasn't following it.
        # Always visible now -- only the *value* is synced from real hardware
        # when a reading exists; when it doesn't (firmware reports
        # "unavailable"), the slider just keeps whatever it's already at and
        # stays fully interactive, rather than the row disappearing.
        self._suppress = True
        self._sync_slider(self.pl1, PPT_PL1_PATH, "pl1")
        self._sync_slider(self.pl2, PPT_PL2_PATH, "pl2")
        self._sync_slider(self.boost, NV_BOOST_PATH, "boost")
        self._sync_slider(self.temp_target, NV_TEMP_TARGET_PATH, "temp_target")
        self.apply_limits_row.switch.set_active(self.limits_cfg["apply_limits"])
        self._suppress = False

        self._refresh_power_mode_async()
        if self._nvidia_probe_done:
            self._apply_nvidia_state()
        else:
            self._probe_nvidia_async()

    def _refresh_power_mode_async(self):
        if not POWERPROFILESCTL:
            self.power_mode_row.set_visible(False)
            return

        def worker():
            try:
                current = subprocess.run(
                    [POWERPROFILESCTL, "get"], capture_output=True, text=True, timeout=5,
                ).stdout.strip()
            except (OSError, subprocess.TimeoutExpired):
                current = ""
            GLib.idle_add(self._set_power_mode_display, current)

        threading.Thread(target=worker, daemon=True).start()

    def _set_power_mode_display(self, current):
        self._suppress = True
        self.power_mode_row.set_visible(True)
        names = ["power-saver", "balanced", "performance"]
        if current in names:
            self.power_mode_dropdown.set_selected(names.index(current))
        self._suppress = False

    def _probe_nvidia_async(self):
        # These probes have a real (if brief) subprocess cost and, for the power
        # limit, a genuine no-op write -- run once in the background and cache,
        # rather than re-probing on every refresh/mode switch.
        def worker():
            clock_ok = nvidia_clock_offset_available()
            bounds = nvidia_power_limit_bounds()
            power_ok = nvidia_power_limit_settable(bounds[0]) if bounds else False
            GLib.idle_add(self._on_nvidia_probed, clock_ok, bounds, power_ok)

        threading.Thread(target=worker, daemon=True).start()

    def _on_nvidia_probed(self, clock_ok, bounds, power_ok):
        self._nvidia_clock_ok = clock_ok
        self._nvidia_power_bounds = bounds
        self._nvidia_power_ok = power_ok
        self._nvidia_probe_done = True
        self._apply_nvidia_state()

    def _apply_nvidia_state(self):
        # Always visible and interactive -- the probe only decides the starting
        # value (real bounds/current when readable, a sane default otherwise)
        # and whether the "may not work here" note shows, never whether the
        # slider itself exists.
        self._suppress = True
        # nvidia-settings can't even *read* these back on this session (only
        # fail to write), so 0 was always used as a hardcoded reset -- same
        # "everything resets" complaint as PL1/PL2, fall back to whatever the
        # user last set instead.
        self.core_clock.scale.set_value(self.limits_cfg["core_clock"] or 0)
        self.mem_clock.scale.set_value(self.limits_cfg["mem_clock"] or 0)

        if self._nvidia_power_bounds is not None:
            cur, lo, hi = self._nvidia_power_bounds
            self.gpu_power.scale.set_range(lo, hi)
            self.gpu_power.scale.set_value(cur)
        elif self.limits_cfg["gpu_power"] is not None:
            self.gpu_power.scale.set_value(self.limits_cfg["gpu_power"])

        self.gpu_unsupported_notice.set_visible(not (self._nvidia_clock_ok or self._nvidia_power_ok))
        self._suppress = False

    def _on_power_mode_changed(self, *_a):
        if self._suppress or not POWERPROFILESCTL:
            return
        names = ["power-saver", "balanced", "performance"]
        name = names[self.power_mode_dropdown.get_selected()]
        run_cmd([POWERPROFILESCTL, "set", name], self._report)

    def _on_pl1_changed(self, scale):
        if self._suppress:
            return
        self._pl1_debouncer.trigger()

    def _apply_pl1(self):
        value = int(self.pl1.scale.get_value())
        self.limits_cfg["pl1"] = value
        self.save_cfg()
        run_cmd(["asusctl", "armoury", "set", "ppt_pl1_spl", str(value)], self._report)

    def _on_pl2_changed(self, scale):
        if self._suppress:
            return
        self._pl2_debouncer.trigger()

    def _apply_pl2(self):
        value = int(self.pl2.scale.get_value())
        self.limits_cfg["pl2"] = value
        self.save_cfg()
        run_cmd(["asusctl", "armoury", "set", "ppt_pl2_sppt", str(value)], self._report)

    def _on_core_clock_changed(self, scale):
        if self._suppress:
            return
        self._core_clock_debouncer.trigger()

    def _apply_core_clock(self):
        value = int(self.core_clock.scale.get_value())
        self.limits_cfg["core_clock"] = value
        self.save_cfg()
        apply_core_clock_offset(value, self._report)

    def _on_mem_clock_changed(self, scale):
        if self._suppress:
            return
        self._mem_clock_debouncer.trigger()

    def _apply_mem_clock(self):
        value = int(self.mem_clock.scale.get_value())
        self.limits_cfg["mem_clock"] = value
        self.save_cfg()
        apply_mem_clock_offset(value, self._report)

    def _on_gpu_power_changed(self, scale):
        if self._suppress:
            return
        self._gpu_power_debouncer.trigger()

    def _apply_gpu_power(self):
        value = self.gpu_power.scale.get_value()
        self.limits_cfg["gpu_power"] = value
        self.save_cfg()
        apply_gpu_power_limit(value, self._report)

    def _on_boost_changed(self, scale):
        if self._suppress:
            return
        self._boost_debouncer.trigger()

    def _apply_boost(self):
        value = int(self.boost.scale.get_value())
        self.limits_cfg["boost"] = value
        self.save_cfg()
        run_cmd(["asusctl", "armoury", "set", "nv_dynamic_boost", str(value)], self._report)

    def _on_temp_changed(self, scale):
        if self._suppress:
            return
        self._temp_debouncer.trigger()

    def _apply_temp(self):
        value = int(self.temp_target.scale.get_value())
        self.limits_cfg["temp_target"] = value
        self.save_cfg()
        run_cmd(["asusctl", "armoury", "set", "nv_temp_target", str(value)], self._report)

    def _on_apply_limits_toggled(self, switch, state):
        if self._suppress:
            return False
        self.limits_cfg["apply_limits"] = state
        self.save_cfg()
        return False

    def _report(self, ok, err):
        if not ok:
            self.toast_cb(f"Command failed: {err[:80]}")


class LightingWindow(Adw.Window):
    """Companion window opened from the main window's compact Keyboard panel,
    matching G-Helper's AsusKeyboardSettings popout (the main window only shows a
    Color swatch + an "Extra" button; full effect/colour/speed/direction editing
    lives here). This hardware reports SupportedBasicZones as empty (confirmed via
    `busctl introspect` on the Aura object), so these effects apply to the whole
    keyboard as one zone rather than true per-key addressing."""

    def __init__(self, app, toast_cb, on_changed):
        super().__init__(application=app)
        # Set before any widget/signal exists: GTK can fire notify::selected /
        # notify::rgba during construction (e.g. DropDown's initial selection),
        # and _on_changed must never apply a half-built effect state.
        self._suppress = True
        self.toast_cb = toast_cb
        self.on_changed = on_changed
        self._apply_debouncer = Debouncer(150, self._do_apply)
        self.set_default_size(380, 480)
        self.set_title("Keyboard Lighting")

        toolbar_view = Adw.ToolbarView()
        toolbar_view.add_top_bar(Adw.HeaderBar(title_widget=Adw.WindowTitle(title="Keyboard Lighting")))
        self.set_content(toolbar_view)

        scroller = Gtk.ScrolledWindow()
        scroller.set_vexpand(True)
        outer = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=10)
        outer.set_margin_top(12)
        outer.set_margin_bottom(14)
        outer.set_margin_start(14)
        outer.set_margin_end(14)
        scroller.set_child(outer)

        outer.append(section_label("Effect"))
        self.effect_dropdown = Gtk.DropDown.new_from_strings(list(AURA_EFFECTS.keys()))
        self.effect_dropdown.set_hexpand(True)
        self.effect_dropdown.connect("notify::selected", self._on_effect_changed)
        outer.append(self.effect_dropdown)

        self.colour1_row = Gtk.Box(spacing=8)
        self.colour1_row.append(Gtk.Label(label="Colour", hexpand=True, halign=Gtk.Align.START))
        self.colour1_btn = color_picker_button(theme_primary_hex())
        self.colour1_btn.connect("notify::rgba", self._on_changed)
        self.colour1_row.append(self.colour1_btn)
        outer.append(self.colour1_row)

        self.colour2_row = Gtk.Box(spacing=8)
        self.colour2_row.append(Gtk.Label(label="Second colour", hexpand=True, halign=Gtk.Align.START))
        self.colour2_btn = color_picker_button(theme_secondary_hex())
        self.colour2_btn.connect("notify::rgba", self._on_changed)
        self.colour2_row.append(self.colour2_btn)
        outer.append(self.colour2_row)

        reset_btn = Gtk.Button(label="Reset colours to desktop theme")
        reset_btn.connect("clicked", self._reset_to_theme)
        outer.append(reset_btn)

        self.direction_row = Gtk.Box(spacing=8)
        self.direction_row.append(Gtk.Label(label="Direction", hexpand=True, halign=Gtk.Align.START))
        self.direction_dropdown = Gtk.DropDown.new_from_strings(DIRECTIONS)
        self.direction_dropdown.connect("notify::selected", self._on_changed)
        self.direction_row.append(self.direction_dropdown)
        outer.append(self.direction_row)

        self.speed_row = Gtk.Box(spacing=8)
        self.speed_row.append(Gtk.Label(label="Speed", hexpand=True, halign=Gtk.Align.START))
        self.speed_dropdown = Gtk.DropDown.new_from_strings(["low", "med", "high"])
        self.speed_dropdown.set_selected(1)
        self.speed_dropdown.connect("notify::selected", self._on_changed)
        self.speed_row.append(self.speed_dropdown)
        outer.append(self.speed_row)

        toolbar_view.set_content(scroller)
        self.refresh()

    def refresh(self):
        self._suppress = True
        state = read_aura_state()
        effects = list(AURA_EFFECTS.keys())
        if state:
            if state["effect"] in effects:
                self.effect_dropdown.set_selected(effects.index(state["effect"]))
            self.colour1_btn.set_rgba(hex_to_gdk_rgba(state["colour1"]))
            self.colour2_btn.set_rgba(hex_to_gdk_rgba(state["colour2"]))
            if state["direction"] in DIRECTIONS:
                self.direction_dropdown.set_selected(DIRECTIONS.index(state["direction"]))
            if state["speed"] in ("low", "med", "high"):
                self.speed_dropdown.set_selected(["low", "med", "high"].index(state["speed"]))
        self._update_effect_fields()
        self._suppress = False

    def _current_effect(self):
        return list(AURA_EFFECTS.keys())[self.effect_dropdown.get_selected()]

    def _update_effect_fields(self):
        needs = AURA_EFFECTS[self._current_effect()]
        self.colour1_row.set_visible("colour1" in needs)
        self.colour2_row.set_visible("colour2" in needs)
        self.direction_row.set_visible("direction" in needs)
        self.speed_row.set_visible("speed" in needs)

    def _on_effect_changed(self, *_a):
        self._update_effect_fields()
        self._on_changed()

    def _reset_to_theme(self, _btn):
        # If the active effect doesn't use colour at all (rainbow-cycle, rain,
        # rainbow-wave -- speed/direction only), setting the colour pickers
        # below has nothing to actually send: confirmed by direct testing,
        # the hardware silently stayed on its old effect/colours entirely.
        # Switch to "static" first so "reset to theme" always has a real,
        # visible result instead of silently doing nothing.
        if "colour1" not in AURA_EFFECTS[self._current_effect()]:
            effects = list(AURA_EFFECTS.keys())
            self.effect_dropdown.set_selected(effects.index("static"))
        self.colour1_btn.set_rgba(hex_to_gdk_rgba(theme_primary_hex()))
        self.colour2_btn.set_rgba(hex_to_gdk_rgba(theme_secondary_hex()))

    def _on_changed(self, *_a):
        if self._suppress:
            return
        self._apply_debouncer.trigger()

    def _do_apply(self):
        effect = self._current_effect()
        needs = AURA_EFFECTS[effect]
        speed = ["low", "med", "high"][self.speed_dropdown.get_selected()]
        direction = DIRECTIONS[self.direction_dropdown.get_selected()]

        cmd = ["asusctl", "aura", "effect", effect]
        if "colour1" in needs:
            colour1 = gdk_rgba_to_hex(self.colour1_btn.get_rgba())
            cmd += ["-c" if effect in ("static", "pulse", "highlight", "laser", "ripple", "comet", "flash") else "--colour", colour1]
        if "colour2" in needs:
            cmd += ["--colour2", gdk_rgba_to_hex(self.colour2_btn.get_rgba())]
        if "direction" in needs:
            cmd += ["--direction", direction]
        if "speed" in needs:
            cmd += ["--speed", speed]

        def done(ok, err):
            if not ok:
                self.toast_cb(f"Effect failed: {err[:80]}")
            elif self.on_changed:
                self.on_changed()

        run_cmd(cmd, done)


class ExtraWindow(Adw.Window):
    """Companion window matching G-Helper's Extra.cs popout. Only the settings
    with a real Linux/asusctl backing are here (boot sound, charging mode) --
    G-Helper's Windows-only or hardware-absent items (key-binding remap, Windows
    Modern Standby/PCIe link tweaks, ACPI debug console, CPU P/E-core limiting,
    vendor-service start/stop) don't have a safe or meaningful equivalent on this
    stack and are left out rather than faked."""

    def __init__(self, app, toast_cb):
        super().__init__(application=app)
        self.toast_cb = toast_cb
        self.set_default_size(380, 320)
        self.set_title("Extra")

        toolbar_view = Adw.ToolbarView()
        toolbar_view.add_top_bar(Adw.HeaderBar(title_widget=Adw.WindowTitle(title="Extra")))
        self.set_content(toolbar_view)

        outer = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12)
        outer.set_margin_top(12)
        outer.set_margin_bottom(14)
        outer.set_margin_start(14)
        outer.set_margin_end(14)

        self.boot_sound_row = switch_row("Boot (POST) sound", self._on_boot_sound_toggled)
        outer.append(self.boot_sound_row)

        outer.append(section_label("Charging mode"))
        self.charge_mode_row = Gtk.Box(spacing=6, homogeneous=True, hexpand=True)
        self._charge_mode_btns = {}
        group = None
        for name in CHARGE_MODES:
            btn = Gtk.ToggleButton(label=name, hexpand=True)
            if group is None:
                group = btn
            else:
                btn.set_group(group)
            btn.connect("toggled", self._on_charge_mode_toggled, name)
            self.charge_mode_row.append(btn)
            self._charge_mode_btns[name] = btn
        outer.append(self.charge_mode_row)

        self.empty_notice = Gtk.Label(label="No additional firmware controls exposed on this laptop.")
        self.empty_notice.add_css_class("rog-empty")
        outer.append(self.empty_notice)

        toolbar_view.set_content(outer)
        self._suppress = False
        self.refresh()

    def refresh(self):
        self._suppress = True
        any_shown = False

        boot_sound = read_sysfs_int(BOOT_SOUND_PATH)
        self.boot_sound_row.set_visible(boot_sound is not None)
        if boot_sound is not None:
            self.boot_sound_row.switch.set_active(boot_sound == 1)
            any_shown = True

        charge_mode = read_sysfs_int(CHARGE_MODE_PATH)
        self.charge_mode_row.set_visible(charge_mode is not None)
        if charge_mode is not None and 0 <= charge_mode < len(CHARGE_MODES):
            self._charge_mode_btns[CHARGE_MODES[charge_mode]].set_active(True)
            any_shown = True

        self.empty_notice.set_visible(not any_shown)
        self._suppress = False

    def _on_boot_sound_toggled(self, switch, state):
        if self._suppress:
            return False
        run_cmd(["asusctl", "armoury", "set", "boot_sound", "1" if state else "0"], self._report)
        return False

    def _on_charge_mode_toggled(self, btn, name):
        if self._suppress or not btn.get_active():
            return
        idx = CHARGE_MODES.index(name)
        run_cmd(["asusctl", "armoury", "set", "charge_mode", str(idx)], self._report)

    def _report(self, ok, err):
        if not ok:
            self.toast_cb(f"Command failed: {err[:80]}")


class RogPopup(Adw.Window):
    """Main window: a single vertically-stacked dashboard (Performance -> GPU ->
    Screen -> Keyboard -> Battery -> Startup -> Footer), auto-sized to content,
    matching G-Helper's real SettingsForm -- no tabs, no sidebar, no scrollbar."""

    def __init__(self, app, cfg, save_cfg, get_on_battery, on_quit):
        super().__init__(application=app)
        self.set_title("ROG Control")
        self.set_resizable(False)
        self._on_quit = on_quit
        self.cfg = cfg
        self.save_cfg = save_cfg

        self.toast_overlay = Adw.ToastOverlay()
        self.set_content(self.toast_overlay)

        toolbar_view = Adw.ToolbarView()
        self.toast_overlay.set_child(toolbar_view)

        header = Adw.HeaderBar(title_widget=Adw.WindowTitle(title="ROG Control", subtitle=device_name()))
        toolbar_view.add_top_bar(header)

        outer = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12)
        outer.set_margin_top(14)
        outer.set_margin_bottom(14)
        outer.set_margin_start(16)
        outer.set_margin_end(16)

        self.fans_window = None
        self.lighting_window = None
        self.extra_window = None

        self.performance = PerformancePanel(self.show_toast, cfg, save_cfg, get_on_battery, self.open_fans)
        self.gpu = GpuPanel(self.show_toast, cfg, save_cfg, get_on_battery, lambda: self)
        self.screen = ScreenPanel(self.show_toast, cfg, save_cfg)
        self.keyboard = KeyboardPanel(self.show_toast, cfg, save_cfg, get_on_battery, self.open_lighting)
        self.battery = BatteryPanel(self.show_toast)
        self.startup = StartupPanel(self.show_toast)
        self.footer = FooterPanel(self.open_extra, self._quit_clicked)

        for panel in (self.performance, self.gpu, self.screen, self.keyboard, self.battery, self.startup):
            outer.append(panel)
            outer.append(Gtk.Separator())
        outer.append(self.footer)

        toolbar_view.set_content(outer)

        self.connect("close-request", self._on_close_request)
        self._refresh_source = None

    def show_toast(self, text):
        self.toast_overlay.add_toast(Adw.Toast.new(text))

    def open_fans(self):
        if self.fans_window is None:
            self.fans_window = FansPowerWindow(self.get_application(), self.show_toast, self.cfg, self.save_cfg)
            self.fans_window.connect("close-request", lambda *_a: self._forget("fans_window"))
        self.fans_window.refresh()
        self.fans_window.present()

    def open_lighting(self):
        if self.lighting_window is None:
            self.lighting_window = LightingWindow(self.get_application(), self.show_toast, self.keyboard.refresh)
            self.lighting_window.connect("close-request", lambda *_a: self._forget("lighting_window"))
        self.lighting_window.refresh()
        self.lighting_window.present()

    def open_extra(self):
        if self.extra_window is None:
            self.extra_window = ExtraWindow(self.get_application(), self.show_toast)
            self.extra_window.connect("close-request", lambda *_a: self._forget("extra_window"))
        self.extra_window.refresh()
        self.extra_window.present()

    def _forget(self, attr):
        setattr(self, attr, None)
        return False

    def _quit_clicked(self):
        self._on_quit()

    def _on_close_request(self, *_a):
        self.hide()
        self._stop_polling()
        return True

    def refresh_all(self):
        self.performance.refresh()
        self.gpu.refresh()
        self.screen.refresh()
        self.keyboard.refresh()
        self.battery.refresh()
        self.startup.refresh()
        if self.fans_window is not None:
            self.fans_window.refresh()
        if self.lighting_window is not None:
            self.lighting_window.refresh()
        if self.extra_window is not None:
            self.extra_window.refresh()

    def toggle(self):
        if self.get_visible():
            self.hide()
            self._stop_polling()
        else:
            self.refresh_all()
            self.present()
            self._start_polling()

    def _start_polling(self):
        if self._refresh_source is None:
            self._refresh_source = GLib.timeout_add_seconds(2, self._poll)

    def _stop_polling(self):
        if self._refresh_source is not None:
            GLib.source_remove(self._refresh_source)
            self._refresh_source = None

    def _poll(self):
        if not self.get_visible():
            self._refresh_source = None
            return False
        self.performance.refresh()
        self.gpu.refresh()
        self.battery.refresh()
        return True


SNI_IFACE = "org.kde.StatusNotifierItem"
SNI_PATH = "/StatusNotifierItem"


class StatusNotifierItem(dbus.service.Object):
    def __init__(self, bus, on_activate, on_secondary_activate, on_scroll):
        super().__init__(bus, SNI_PATH)
        self._on_activate = on_activate
        self._on_secondary_activate = on_secondary_activate
        self._on_scroll = on_scroll

    @dbus.service.method("org.freedesktop.DBus.Properties", in_signature="ss", out_signature="v")
    def Get(self, interface_name, property_name):
        return self.GetAll(interface_name)[property_name]

    @dbus.service.method("org.freedesktop.DBus.Properties", in_signature="s", out_signature="a{sv}")
    def GetAll(self, interface_name):
        if interface_name != SNI_IFACE:
            return {}
        return {
            "Category": "Hardware",
            "Id": "rog-control",
            "Title": "ROG Control",
            "Status": "Active",
            "IconName": TRAY_ICON_NAME,
            "IconThemePath": ICONS_DIR,
            "ItemIsMenu": False,
            "ToolTip": ("", dbus.Array([], signature="(iiay)"), "ROG Control",
                        "Left-click: open · scroll: cycle profile · right-click: quit · ROG key / Ctrl+Shift+F12: open"),
        }

    @dbus.service.method("org.freedesktop.DBus.Properties", in_signature="ssv")
    def Set(self, interface_name, property_name, new_value):
        return

    @dbus.service.method(SNI_IFACE, in_signature="ii")
    def Activate(self, x, y):
        GLib.idle_add(self._on_activate)

    @dbus.service.method(SNI_IFACE, in_signature="ii")
    def SecondaryActivate(self, x, y):
        GLib.idle_add(self._on_secondary_activate)

    @dbus.service.method(SNI_IFACE, in_signature="ii")
    def ContextMenu(self, x, y):
        return

    @dbus.service.method(SNI_IFACE, in_signature="is")
    def Scroll(self, delta, orientation):
        GLib.idle_add(self._on_scroll, delta)

    @dbus.service.signal(SNI_IFACE)
    def NewIcon(self):
        pass

    @dbus.service.signal(SNI_IFACE)
    def NewStatus(self, status):
        pass


# A distinct bus name from APP_ID: Adw.Application already owns "com.orneyfish.
# RogControl" itself via GApplication's own D-Bus registration, so requesting
# that same name here would just queue behind it instead of being acquired
# (confirmed: calls silently routed to GApplication's built-in object instead
# of this one, erroring "No such interface").
CONTROL_IFACE = "com.orneyfish.RogControl.Hotkeys"
CONTROL_PATH = "/com/orneyfish/RogControl/Hotkeys"


class RogControlService(dbus.service.Object):
    """Plain DBus method calls for hotkey/CLI dispatch (ROG key, Hyprland binds).

    GApplication's own HANDLES_COMMAND_LINE remote-activation machinery (which
    forwards a second process's argv to the primary instance over D-Bus,
    including FD-passing for stdin/stdout) was tried first and hung indefinitely
    on this system's session bus -- the calling process never got a reply. This
    reuses the same plain dbus-python pattern already proven to work for the
    tray's StatusNotifierItem instead: own a well-known bus name and expose a
    couple of trivial methods that a short CLI invocation can call directly."""

    def __init__(self, bus, on_open, on_cycle, on_cycle_back):
        bus_name = dbus.service.BusName(CONTROL_IFACE, bus=bus)
        super().__init__(bus_name, CONTROL_PATH)
        self._on_open = on_open
        self._on_cycle = on_cycle
        self._on_cycle_back = on_cycle_back

    @dbus.service.method(CONTROL_IFACE)
    def Open(self):
        GLib.idle_add(self._on_open)

    @dbus.service.method(CONTROL_IFACE)
    def CycleProfile(self):
        GLib.idle_add(self._on_cycle)

    @dbus.service.method(CONTROL_IFACE)
    def CycleProfileBack(self):
        GLib.idle_add(self._on_cycle_back)


def register_dbus_services(on_activate, on_secondary_activate, on_scroll, on_open, on_cycle, on_cycle_back):
    DBusGMainLoop(set_as_default=True)
    bus = dbus.SessionBus()
    item = StatusNotifierItem(bus, on_activate, on_secondary_activate, on_scroll)
    service_id = bus.get_unique_name() + SNI_PATH

    def try_register(attempts_left):
        try:
            watcher = bus.get_object("org.kde.StatusNotifierWatcher", "/StatusNotifierWatcher")
            watcher.RegisterStatusNotifierItem(service_id, dbus_interface="org.kde.StatusNotifierWatcher")
        except dbus.DBusException:
            if attempts_left > 0:
                GLib.timeout_add_seconds(3, try_register, attempts_left - 1)
        return False

    # The shell (which owns the watcher) may still be starting when we do -- retry a few times.
    try_register(10)

    control = RogControlService(bus, on_open, on_cycle, on_cycle_back)
    return item, control


class AutomationManager:
    """Watches UPower's OnBattery property (system bus) and re-applies whichever
    automation is due on every AC<->battery transition. Matches G-Helper's real
    model exactly (see config.py / the module docstring): performance-mode and
    keyboard-backlight level are implicit, always-on, per-power-source memory --
    no opt-in needed, just remembers whatever was last picked in each state.
    GPU-Optimized and Screen-Auto are the two settings with a genuine opt-in,
    chosen among each setting's own normal options rather than a separate switch."""

    def __init__(self, cfg, save_cfg, refresh_ui_cb):
        self.cfg = cfg
        self.save_cfg = save_cfg
        self.refresh_ui_cb = refresh_ui_cb
        self.on_battery = None
        self._watch()
        # _watch() only determines the current power state; without this, saved
        # automation (remembered profile/keyboard level, GPU-Optimized, screen-
        # auto) never actually applies until the *next* AC<->battery transition
        # -- a fresh boot/login sits on whatever raw state the hardware already
        # had. Silent (no OSD) since nothing actually "transitioned" here.
        self._apply_all(notify=False)

    def _watch(self):
        try:
            bus = dbus.SystemBus()
            bus.add_signal_receiver(
                self._on_props_changed,
                signal_name="PropertiesChanged",
                dbus_interface="org.freedesktop.DBus.Properties",
                path="/org/freedesktop/UPower",
            )
            proxy = bus.get_object("org.freedesktop.UPower", "/org/freedesktop/UPower")
            props = dbus.Interface(proxy, "org.freedesktop.DBus.Properties")
            self.on_battery = bool(props.Get("org.freedesktop.UPower", "OnBattery"))
        except dbus.DBusException:
            self.on_battery = None

    def _on_props_changed(self, iface, changed, invalidated):
        if "OnBattery" not in changed:
            return
        new_state = bool(changed["OnBattery"])
        if new_state == self.on_battery:
            return
        self.on_battery = new_state
        GLib.idle_add(self._apply_all)

    def _apply_all(self, notify=True):
        if self.on_battery is None:
            return
        auto = self.cfg["automation"]
        key = "battery" if self.on_battery else "ac"

        profile = auto[f"profile_{key}"]
        if profile:
            done = (lambda ok, err: notify_osd("Performance mode", PROFILE_LABELS[profile]) if ok else None) if notify else None
            apply_profile(profile, done)

        if auto["gpu_optimized"]:
            if self.on_battery and gpu_in_heavy_use():
                # Mirrors G-Helper's HardwareControl.IsUsedGPU() guard: don't yank
                # the dGPU out from under an active render just because the power
                # source changed.
                if notify:
                    notify_osd("Eco Mode", "GPU is in heavy use, leaving dGPU on")
            else:
                # apply_gpu_mode can refuse silently (e.g. an external display
                # wired straight to the dGPU) -- without this callback, that
                # refusal previously had no way to reach the user in this
                # automation path (no toast_cb exists here, only OSD).
                gpu_done = (lambda ok, err: notify_osd("Optimized GPU mode", err) if not ok and notify else None)
                apply_gpu_mode("Optimized", self.on_battery, gpu_done)

        if auto["screen_auto"]:
            apply_screen_preset("auto-battery" if self.on_battery else "auto-ac")

        kbd = auto[f"kbd_{key}"]
        if kbd:
            apply_kbd_level(kbd)

        if notify:
            notify_osd("On battery" if self.on_battery else "On AC power", "Automation applied")
        self.refresh_ui_cb()


class RogControlApp(Adw.Application):
    def __init__(self):
        # Plain default flags -- no IS_SERVICE, no HANDLES_COMMAND_LINE. Hotkey/CLI
        # dispatch goes through RogControlService (a plain D-Bus method call from
        # main(), see below) instead of GApplication's own remote-command-line
        # machinery, which hung indefinitely on this system. Staying hidden at
        # boot just means never calling _toggle_popup() during startup; .hold()
        # is what keeps the process alive with no window.
        super().__init__(application_id=APP_ID)
        self.popup = None
        self.cfg = config.load()
        self.automation = None
        self.hold()  # keep running with no window (tray-resident)

    def do_startup(self):
        Adw.Application.do_startup(self)
        register_icons()
        css = Gtk.CssProvider()
        css.load_from_data(build_material_css(load_material_colors()).encode())
        Gtk.StyleContext.add_provider_for_display(
            Gdk.Display.get_default(), css, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION
        )
        register_dbus_services(
            self._toggle_popup, self._quit_from_tray, self._on_scroll,
            self._toggle_popup, self._cycle_forward, self._cycle_back,
        )
        self.automation = AutomationManager(self.cfg, self._save_cfg, self._refresh_popup_if_visible)

    def do_activate(self):
        # GApplication fires this once at startup by default when nothing else
        # handles it (and warns if it's unhandled) -- deliberately a no-op since
        # opening the panel always goes through RogControlService now, not this.
        pass

    def _cycle_forward(self):
        cycle_profile(1, lambda ok, err: self._refresh_popup_if_visible())

    def _cycle_back(self):
        cycle_profile(-1, lambda ok, err: self._refresh_popup_if_visible())

    def _save_cfg(self):
        config.save(self.cfg)

    def _get_on_battery(self):
        return self.automation.on_battery if self.automation else False

    def _refresh_popup_if_visible(self):
        if self.popup is not None and self.popup.get_visible():
            self.popup.refresh_all()

    def _toggle_popup(self):
        try:
            if self.popup is None:
                self.popup = RogPopup(self, self.cfg, self._save_cfg, self._get_on_battery, self._quit_from_tray)
            self.popup.toggle()
        except Exception:
            import traceback
            traceback.print_exc()

    def _quit_from_tray(self):
        # Right-click on the tray icon quits -- the standard convention for a
        # single-action tray context menu, and the only way to close this app
        # short of `systemctl --user stop rog-control.service` since it has no
        # taskbar window by design.
        if self.popup is not None:
            self.popup.hide()
        self.quit()

    def _on_scroll(self, delta):
        # Any scroll cycles to the next performance profile, matching G-Helper /
        # omarchy-asus's tray-icon scroll behaviour.
        cycle_profile(1, lambda ok, err: self._refresh_popup_if_visible())


def main():
    args = sys.argv[1:]
    # Must precede the first dbus.SessionBus(): that connection is shared, and
    # register_dbus_services() later reuses it, so it needs the main loop from birth.
    DBusGMainLoop(set_as_default=True)
    if not args and dbus.SessionBus().name_has_owner(APP_ID):
        # The launcher entry runs a bare main.py, the same command as systemd's
        # ExecStart. With an instance already up, a second GApplication registers
        # as a "remote" instance and hangs forever with no window (found as eight
        # stray processes, one per launcher click) -- so treat it as --open.
        args = ["--open"]
    if args:
        # A hotkey/CLI invocation while the real instance (started once, by
        # systemd) is already running -- call straight into it over D-Bus and
        # exit, rather than spinning up a second GTK process.
        try:
            obj = dbus.SessionBus().get_object(CONTROL_IFACE, CONTROL_PATH, introspect=False)
            if "--cycle-profile" in args:
                obj.CycleProfile(dbus_interface=CONTROL_IFACE)
            elif "--cycle-profile-back" in args:
                obj.CycleProfileBack(dbus_interface=CONTROL_IFACE)
            elif "--open" in args:
                obj.Open(dbus_interface=CONTROL_IFACE)
        except dbus.DBusException as e:
            print(f"ROG Control service not reachable: {e}", file=sys.stderr)
        return

    # No flags: this is systemd's own boot-time launch -- start the real service.
    app = RogControlApp()
    app.run(None)


if __name__ == "__main__":
    main()
