#!/usr/bin/env python3
"""ROG Control: a background tray app for ASUS ROG laptop control (asusctl/supergfxctl),
styled to match this machine's caelestia-shell rice. Modeled on G-Helper's feature set.

No window is shown on launch -- it registers a StatusNotifierItem tray icon (shown in
caelestia-shell's own bar tray, next to Steam etc.) and left-click toggles a small popup
panel. libayatana-appindicator was deliberately not used: it never exposes SNI's Activate
(left-click) callback to the app, only a right-click menu -- so the SNI item is implemented
directly here via dbus-python against org.kde.StatusNotifierWatcher (confirmed present,
owned by quickshell itself, on this system).
"""
import json
import math
import os
import subprocess
import threading

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
from gi.repository import Adw, Gdk, Gio, GLib, Gtk

import dbus
import dbus.service
from dbus.mainloop.glib import DBusGMainLoop

APP_ID = "com.orneyfish.RogControl"
APP_DIR = os.path.dirname(os.path.abspath(__file__))
ICONS_DIR = os.path.join(APP_DIR, "icons")
TRAY_ICON_NAME = "rog-control-symbolic"

SCHEME_JSON_PATH = os.path.expanduser("~/.local/state/caelestia/scheme.json")

PROFILES = ["Quiet", "Balanced", "Performance"]
PLATFORM_PROFILE_PATH = "/sys/firmware/acpi/platform_profile"
BATTERY_LIMIT_PATH = "/sys/class/power_supply/BAT0/charge_control_end_threshold"
KBD_BRIGHTNESS_PATH = "/sys/class/leds/asus::kbd_backlight/brightness"
GPU_MUX_PATH = "/sys/class/firmware-attributes/asus-armoury/attributes/gpu_mux_mode/current_value"
DGPU_DISABLE_PATH = "/sys/class/firmware-attributes/asus-armoury/attributes/dgpu_disable/current_value"
PENDING_REBOOT_PATH = "/sys/class/firmware-attributes/asus-armoury/attributes/pending_reboot"
PANEL_OVERDRIVE_PATH = "/sys/class/firmware-attributes/asus-armoury/attributes/panel_overdrive/current_value"
BOOT_SOUND_PATH = "/sys/class/firmware-attributes/asus-armoury/attributes/boot_sound/current_value"
PPT_PL1_PATH = "/sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl1_spl/current_value"
PPT_PL2_PATH = "/sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl2_sppt/current_value"
NV_BOOST_PATH = "/sys/class/firmware-attributes/asus-armoury/attributes/nv_dynamic_boost/current_value"
NV_TEMP_TARGET_PATH = "/sys/class/firmware-attributes/asus-armoury/attributes/nv_temp_target/current_value"
CPU_TEMP_PATH = "/sys/class/hwmon/hwmon4/temp1_input"
CPU_FAN_PATH = "/sys/class/hwmon/hwmon8/fan1_input"
GPU_FAN_PATH = "/sys/class/hwmon/hwmon8/fan2_input"

BRIGHTNESS_LEVELS = ["off", "low", "med", "high"]
BRIGHTNESS_LABELS = ["Off", "Low", "Medium", "High"]
# gpu_mux_mode sysfs semantics per kernel ABI docs: 0 = discrete/Ultimate, 1 = Optimus/hybrid
GPU_MODES = ["Ultimate (dGPU only)", "Optimus (hybrid)"]
KBD_COLOURS = [
    ("Theme", None),
    ("White", "ffffff"),
    ("Red", "ff0000"),
    ("Green", "00ff00"),
    ("Blue", "0088ff"),
    ("Purple", "8000ff"),
]
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

FALLBACK_MATERIAL_COLORS = {
    "surface": "#1a1112",
    "surfaceContainer": "#211717",
    "surfaceContainerHigh": "#281d1d",
    "onSurface": "#f8e0e0",
    "onSurfaceVariant": "#bba6a6",
    "primary": "#f7b6b9",
    "onPrimary": "#603337",
    "secondary": "#e6bdbe",
    "onSecondary": "#55393a",
    "outline": "#847172",
    "error": "#f97758",
    "onError": "#450900",
}


def read_sysfs(path, default=None):
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


def build_material_css(c):
    """Deliberately restrained: this reuses caelestia's own token palette rather
    than inventing new colours, keeps a single consistent radius and an 8px
    spacing rhythm, and skips decorative nested cards / gradients / motion --
    the things that make generated UI read as generated."""

    def col(name):
        return c.get(name, FALLBACK_MATERIAL_COLORS.get(name, "#888888"))

    return f"""
    window {{
        background-color: {col('surface')};
        color: {col('onSurface')};
    }}
    headerbar {{
        background-color: {col('surface')};
        color: {col('onSurface')};
        box-shadow: none;
        border-bottom: 1px solid {col('surfaceContainerHigh')};
    }}
    label.rog-section {{
        color: {col('onSurfaceVariant')};
        font-weight: 600;
        font-size: 90%;
        margin-top: 8px;
    }}
    label.rog-subtitle {{
        color: {col('onSurfaceVariant')};
        font-size: 90%;
    }}
    label.rog-stat {{
        color: {col('primary')};
        font-weight: 600;
    }}
    scale trough highlight {{
        background-color: {col('primary')};
    }}
    switch:checked {{
        background-color: {col('primary')};
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
    togglebutton {{
        border-radius: 8px;
    }}
    togglebutton:checked {{
        background-color: {col('primary')};
        color: {col('onPrimary')};
    }}
    """


def register_icons():
    display = Gdk.Display.get_default()
    Gtk.IconTheme.get_for_display(display).add_search_path(ICONS_DIR)


class StatSection(Gtk.Box):
    def __init__(self):
        super().__init__(orientation=Gtk.Orientation.HORIZONTAL, spacing=12)
        self.set_halign(Gtk.Align.CENTER)
        self.cpu_temp = self._stat("CPU")
        self.gpu_temp = self._stat("GPU")
        self.cpu_fan = self._stat("CPU fan")
        self.gpu_fan = self._stat("GPU fan")

    def _stat(self, title):
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=2)
        value = Gtk.Label(label="--")
        value.add_css_class("rog-stat")
        caption = Gtk.Label(label=title)
        caption.add_css_class("rog-subtitle")
        box.append(value)
        box.append(caption)
        self.append(box)
        return value

    def refresh(self):
        cpu_temp = read_sysfs_int(CPU_TEMP_PATH)
        self.cpu_temp.set_label(f"{cpu_temp / 1000:.0f}°C" if cpu_temp else "--")
        cpu_fan = read_sysfs_int(CPU_FAN_PATH)
        self.cpu_fan.set_label(f"{cpu_fan} rpm" if cpu_fan is not None else "--")
        gpu_fan = read_sysfs_int(GPU_FAN_PATH)
        self.gpu_fan.set_label(f"{gpu_fan} rpm" if gpu_fan is not None else "--")

        def worker():
            try:
                out = subprocess.run(
                    ["nvidia-smi", "--query-gpu=temperature.gpu", "--format=csv,noheader"],
                    capture_output=True, text=True, timeout=5,
                ).stdout.strip()
                temp = f"{out}°C" if out else "--"
            except (OSError, subprocess.TimeoutExpired):
                temp = "--"
            GLib.idle_add(self.gpu_temp.set_label, temp)

        threading.Thread(target=worker, daemon=True).start()


class MainSection(Gtk.Box):
    def __init__(self, toast_cb):
        super().__init__(orientation=Gtk.Orientation.VERTICAL, spacing=10)
        self.toast_cb = toast_cb
        self.set_margin_top(12)
        self.set_margin_bottom(12)
        self.set_margin_start(14)
        self.set_margin_end(14)

        self.stats = StatSection()
        self.append(self.stats)

        self.append(self._section_label("Power profile"))
        self.profile_buttons = Gtk.Box(spacing=6, homogeneous=True, hexpand=True)
        group = None
        self._profile_btns = {}
        for name in PROFILES:
            btn = Gtk.ToggleButton(label=name, hexpand=True)
            if group is None:
                group = btn
            else:
                btn.set_group(group)
            btn.connect("toggled", self._on_profile_toggled, name)
            self.profile_buttons.append(btn)
            self._profile_btns[name] = btn
        self.append(self.profile_buttons)

        self.append(self._section_label("Battery charge limit"))
        self.battery_scale = Gtk.Scale.new_with_range(Gtk.Orientation.HORIZONTAL, 20, 100, 5)
        self.battery_scale.set_draw_value(True)
        self.battery_scale.set_value_pos(Gtk.PositionType.RIGHT)
        self.battery_scale.set_hexpand(True)
        self.battery_scale.connect("value-changed", self._on_battery_changed)
        self.append(self.battery_scale)

        self.append(self._section_label("GPU mode (requires reboot)"))
        self.gpu_buttons = Gtk.Box(spacing=6, homogeneous=True, hexpand=True)
        group = None
        self._gpu_btns = {}
        for name in GPU_MODES:
            btn = Gtk.ToggleButton(label=name, hexpand=True)
            if group is None:
                group = btn
            else:
                btn.set_group(group)
            btn.connect("toggled", self._on_gpu_toggled, name)
            self.gpu_buttons.append(btn)
            self._gpu_btns[name] = btn
        self.append(self.gpu_buttons)

        self.dgpu_row = Gtk.Box(spacing=8)
        dgpu_label = Gtk.Label(label="Disable dGPU entirely")
        dgpu_label.set_hexpand(True)
        dgpu_label.set_halign(Gtk.Align.START)
        self.dgpu_switch = Gtk.Switch()
        self.dgpu_switch.set_valign(Gtk.Align.CENTER)
        self.dgpu_switch.connect("state-set", self._on_dgpu_toggled)
        self.dgpu_row.append(dgpu_label)
        self.dgpu_row.append(self.dgpu_switch)
        self.append(self.dgpu_row)

        self.reboot_notice = Gtk.Label(label="")
        self.reboot_notice.add_css_class("rog-subtitle")
        self.append(self.reboot_notice)

        self.append(self._section_label("Screen"))
        self.refresh_buttons = Gtk.Box(spacing=6, homogeneous=True, hexpand=True)
        self._refresh_btns = {}
        rgroup = None
        for hz in self._available_refresh_rates():
            btn = Gtk.ToggleButton(label=f"{hz} Hz", hexpand=True)
            if rgroup is None:
                rgroup = btn
            else:
                btn.set_group(rgroup)
            btn.connect("toggled", self._on_refresh_toggled, hz)
            self.refresh_buttons.append(btn)
            self._refresh_btns[hz] = btn
        self.append(self.refresh_buttons)

        self.overdrive_row = Gtk.Box(spacing=8)
        od_label = Gtk.Label(label="Panel overdrive (reduce ghosting)")
        od_label.set_hexpand(True)
        od_label.set_halign(Gtk.Align.START)
        self.overdrive_switch = Gtk.Switch()
        self.overdrive_switch.set_valign(Gtk.Align.CENTER)
        self.overdrive_switch.connect("state-set", self._on_overdrive_toggled)
        self.overdrive_row.append(od_label)
        self.overdrive_row.append(self.overdrive_switch)
        self.append(self.overdrive_row)

        self._suppress = False
        self.refresh()

    def _available_refresh_rates(self):
        try:
            out = subprocess.run(["hyprctl", "monitors", "-j"], capture_output=True, text=True, timeout=5).stdout
            monitors = json.loads(out)
            modes = monitors[0].get("availableModes", []) if monitors else []
            rates = sorted({int(round(float(m.split("@")[1].rstrip("Hz")))) for m in modes}, reverse=True)
            return rates or [60]
        except (OSError, subprocess.TimeoutExpired, ValueError, IndexError, KeyError):
            return [60]

    def _current_monitor(self):
        try:
            out = subprocess.run(["hyprctl", "monitors", "-j"], capture_output=True, text=True, timeout=5).stdout
            monitors = json.loads(out)
            return monitors[0] if monitors else None
        except (OSError, subprocess.TimeoutExpired, ValueError, IndexError):
            return None

    def _section_label(self, text):
        lbl = Gtk.Label(label=text)
        lbl.set_halign(Gtk.Align.START)
        lbl.add_css_class("rog-section")
        return lbl

    def refresh(self):
        self.stats.refresh()

        self._suppress = True
        current = read_sysfs(PLATFORM_PROFILE_PATH, "balanced").capitalize()
        btn = self._profile_btns.get(current)
        if btn:
            btn.set_active(True)

        limit = read_sysfs_int(BATTERY_LIMIT_PATH, 100)
        self.battery_scale.set_value(limit)

        mux = read_sysfs_int(GPU_MUX_PATH, 0)
        gpu_btn = self._gpu_btns.get(GPU_MODES[mux] if mux in (0, 1) else GPU_MODES[0])
        if gpu_btn:
            gpu_btn.set_active(True)

        self.dgpu_switch.set_active(read_sysfs_int(DGPU_DISABLE_PATH, 0) == 1)

        pending = read_sysfs(PENDING_REBOOT_PATH, "0") == "1"
        self.reboot_notice.set_label("Reboot required to apply a pending change" if pending else "")

        monitor = self._current_monitor()
        if monitor:
            current_hz = int(round(monitor.get("refreshRate", 0)))
            closest = min(self._refresh_btns, key=lambda hz: abs(hz - current_hz), default=None)
            btn = self._refresh_btns.get(closest)
            if btn:
                btn.set_active(True)

        od = read_sysfs_int(PANEL_OVERDRIVE_PATH)
        if od is not None:
            self.overdrive_switch.set_active(od == 1)
        self.overdrive_row.set_visible(od is not None)

        self._suppress = False

    def _on_profile_toggled(self, btn, name):
        if self._suppress or not btn.get_active():
            return
        run_cmd(["asusctl", "profile", "set", name], self._report)

    def _on_battery_changed(self, scale):
        if self._suppress:
            return
        value = int(scale.get_value())
        run_cmd(["asusctl", "battery", "limit", str(value)], self._report)

    def _on_gpu_toggled(self, btn, name):
        if self._suppress or not btn.get_active():
            return
        idx = GPU_MODES.index(name)
        run_cmd(["asusctl", "armoury", "set", "gpu_mux_mode", str(idx)], self._report)

    def _on_dgpu_toggled(self, switch, state):
        if self._suppress:
            return False
        run_cmd(["asusctl", "armoury", "set", "dgpu_disable", "1" if state else "0"], self._report)
        return False

    def _on_refresh_toggled(self, btn, hz):
        if self._suppress or not btn.get_active():
            return
        monitor = self._current_monitor()
        if not monitor:
            self.toast_cb("Could not read monitor info")
            return
        res = f"{monitor['width']}x{monitor['height']}"
        pos = f"{monitor['x']}x{monitor['y']}"
        rule = f"{monitor['name']},{res}@{hz},{pos},{monitor['scale']}"
        run_cmd(["hyprctl", "keyword", "monitor", rule], self._report)

    def _on_overdrive_toggled(self, switch, state):
        if self._suppress:
            return False
        run_cmd(["asusctl", "armoury", "set", "panel_overdrive", "1" if state else "0"], self._report)
        return False

    def _report(self, ok, err):
        if not ok:
            self.toast_cb(f"Command failed: {err[:80]}")


def hex_to_rgba(hexval, alpha=1.0):
    hexval = hexval.lstrip("#")
    r = int(hexval[0:2], 16) / 255
    g = int(hexval[2:4], 16) / 255
    b = int(hexval[4:6], 16) / 255
    return r, g, b, alpha


class FanCurveGraph(Gtk.DrawingArea):
    """A draggable temp->speed% curve, like G-Helper/omarchy-asus's fan graph --
    editing 8 numbers in a list is accurate but doesn't communicate the *shape*
    of the curve, which is the thing that actually matters when tuning fan
    behaviour (how aggressively it ramps, where the knee is)."""

    TEMP_MIN, TEMP_MAX = 20, 100
    SPEED_MIN, SPEED_MAX = 0, 100
    PAD = 28
    HIT_RADIUS = 16

    def __init__(self, on_point_selected=None):
        super().__init__()
        self.on_point_selected = on_point_selected
        self.points = [(40, 20), (50, 30), (55, 40), (60, 50), (65, 60), (70, 75), (80, 90), (95, 100)]
        self._dragging = None
        self._drag_origin = None
        colours = load_material_colors()
        self.line_rgba = hex_to_rgba(colours.get("primary", "#f7b6b9"))
        self.point_rgba = hex_to_rgba(colours.get("onSurface", "#f8e0e0"))
        self.grid_rgba = hex_to_rgba(colours.get("onSurfaceVariant", "#bba6a6"), 0.18)

        self.set_content_width(400)
        self.set_content_height(220)
        self.set_hexpand(True)
        self.set_vexpand(False)
        self.set_draw_func(self._draw)

        drag = Gtk.GestureDrag()
        drag.connect("drag-begin", self._on_drag_begin)
        drag.connect("drag-update", self._on_drag_update)
        drag.connect("drag-end", self._on_drag_end)
        self.add_controller(drag)

    def set_points(self, points):
        self.points = list(points)
        self.queue_draw()

    def _geometry(self):
        w = self.get_width() or self.get_content_width()
        h = self.get_height() or self.get_content_height()
        return w, h, self.PAD

    def _to_px(self, temp, speed, w, h, pad):
        x = pad + (temp - self.TEMP_MIN) / (self.TEMP_MAX - self.TEMP_MIN) * (w - 2 * pad)
        y = h - pad - (speed - self.SPEED_MIN) / (self.SPEED_MAX - self.SPEED_MIN) * (h - 2 * pad)
        return x, y

    def _from_px(self, x, y, w, h, pad):
        temp = self.TEMP_MIN + (x - pad) / (w - 2 * pad) * (self.TEMP_MAX - self.TEMP_MIN)
        speed = self.SPEED_MIN + (h - pad - y) / (h - 2 * pad) * (self.SPEED_MAX - self.SPEED_MIN)
        return temp, speed

    def _draw(self, _area, cr, w, h):
        pad = self.PAD
        cr.set_source_rgba(*self.grid_rgba)
        cr.set_line_width(1)
        for i in range(5):
            y = pad + i * (h - 2 * pad) / 4
            cr.move_to(pad, y)
            cr.line_to(w - pad, y)
        for i in range(5):
            x = pad + i * (w - 2 * pad) / 4
            cr.move_to(x, pad)
            cr.line_to(x, h - pad)
        cr.stroke()

        cr.set_source_rgba(*self.line_rgba)
        cr.set_line_width(2.5)
        for i, (t, s) in enumerate(self.points):
            x, y = self._to_px(t, s, w, h, pad)
            cr.line_to(x, y) if i else cr.move_to(x, y)
        cr.stroke()

        for i, (t, s) in enumerate(self.points):
            x, y = self._to_px(t, s, w, h, pad)
            radius = 7 if i == self._dragging else 5
            cr.arc(x, y, radius, 0, 2 * math.pi)
            cr.set_source_rgba(*self.point_rgba)
            cr.fill_preserve()
            cr.set_source_rgba(*self.line_rgba)
            cr.set_line_width(2)
            cr.stroke()

    def _nearest_point(self, x, y, w, h, pad):
        best_i, best_d = None, self.HIT_RADIUS
        for i, (t, s) in enumerate(self.points):
            px, py = self._to_px(t, s, w, h, pad)
            d = math.hypot(px - x, py - y)
            if d < best_d:
                best_i, best_d = i, d
        return best_i

    def _on_drag_begin(self, gesture, start_x, start_y):
        w, h, pad = self._geometry()
        self._dragging = self._nearest_point(start_x, start_y, w, h, pad)
        self._drag_origin = (start_x, start_y)
        self.queue_draw()

    def _on_drag_update(self, gesture, offset_x, offset_y):
        if self._dragging is None:
            return
        w, h, pad = self._geometry()
        start_x, start_y = self._drag_origin
        temp, speed = self._from_px(start_x + offset_x, start_y + offset_y, w, h, pad)

        i = self._dragging
        lo = self.points[i - 1][0] + 1 if i > 0 else self.TEMP_MIN
        hi = self.points[i + 1][0] - 1 if i < len(self.points) - 1 else self.TEMP_MAX
        temp = max(lo, min(hi, round(temp)))
        speed = max(self.SPEED_MIN, min(self.SPEED_MAX, round(speed)))
        self.points[i] = (int(temp), int(speed))
        self.queue_draw()
        if self.on_point_selected:
            self.on_point_selected(i, self.points[i])

    def _on_drag_end(self, gesture, offset_x, offset_y):
        self._dragging = None
        self.queue_draw()


class FanSection(Gtk.Box):
    def __init__(self, toast_cb):
        super().__init__(orientation=Gtk.Orientation.VERTICAL, spacing=10)
        self.toast_cb = toast_cb
        self.set_margin_top(12)
        self.set_margin_bottom(12)
        self.set_margin_start(14)
        self.set_margin_end(14)

        self.stats = StatSection()
        self.append(self.stats)

        self.append(Gtk.Label(label="Edit the fan curve for:", halign=Gtk.Align.START))

        selector = Gtk.Box(spacing=6, homogeneous=True, hexpand=True)
        self.profile_dropdown = Gtk.DropDown.new_from_strings(PROFILES)
        self.profile_dropdown.set_hexpand(True)
        self.fan_dropdown = Gtk.DropDown.new_from_strings(["cpu", "gpu"])
        self.fan_dropdown.set_hexpand(True)
        selector.append(self.profile_dropdown)
        selector.append(self.fan_dropdown)
        self.append(selector)

        self.profile_dropdown.connect("notify::selected", lambda *_: self.load_curve())
        self.fan_dropdown.connect("notify::selected", lambda *_: self.load_curve())

        self.graph = FanCurveGraph(on_point_selected=self._on_point_dragged)
        self.append(self.graph)

        self.point_label = Gtk.Label(label="Drag a point to edit it")
        self.point_label.add_css_class("rog-subtitle")
        self.append(self.point_label)

        self.enable_switch_row = Gtk.Box(spacing=8)
        en_label = Gtk.Label(label="Curve enabled")
        en_label.set_hexpand(True)
        en_label.set_halign(Gtk.Align.START)
        self.enable_switch = Gtk.Switch()
        self.enable_switch.set_valign(Gtk.Align.CENTER)
        self.enable_switch_row.append(en_label)
        self.enable_switch_row.append(self.enable_switch)
        self.append(self.enable_switch_row)

        button_row = Gtk.Box(spacing=6, homogeneous=True, hexpand=True)
        apply_btn = Gtk.Button(label="Apply curve", hexpand=True)
        apply_btn.add_css_class("suggested-action")
        apply_btn.connect("clicked", self._apply)
        reset_btn = Gtk.Button(label="Reset to default", hexpand=True)
        reset_btn.connect("clicked", self._reset)
        button_row.append(apply_btn)
        button_row.append(reset_btn)
        self.append(button_row)

        self._suppress = False
        self.load_curve()

    def _current_profile(self):
        return PROFILES[self.profile_dropdown.get_selected()]

    def _current_fan(self):
        return ["cpu", "gpu"][self.fan_dropdown.get_selected()]

    def load_curve(self):
        fan = self._current_fan()

        def worker():
            try:
                out = subprocess.run(
                    ["asusctl", "fan-curve", "--get-enabled"],
                    capture_output=True, text=True, timeout=10,
                ).stdout
            except (OSError, subprocess.TimeoutExpired):
                out = ""
            points, enabled = self._parse_curve(out, fan)
            GLib.idle_add(self._populate, points, enabled)

        threading.Thread(target=worker, daemon=True).start()

    @staticmethod
    def _parse_curve(text, fan):
        # e.g. "CPU: enabled: false, 47c:25%,51c:30%,..."
        prefix = fan.upper() + ":"
        for line in text.splitlines():
            line = line.strip()
            if not line.startswith(prefix):
                continue
            enabled = "enabled: true" in line
            data_part = line.split(",", 1)[1] if "," in line else ""
            points = []
            for pair in data_part.split(","):
                pair = pair.strip()
                if "c:" not in pair:
                    continue
                t, s = pair.split("c:")
                points.append((int(t), int(s.rstrip("%"))))
            if points:
                return points, enabled
        return [(40, 20), (50, 30), (55, 40), (60, 50), (65, 60), (70, 75), (80, 90), (95, 100)], False

    def _populate(self, points, enabled):
        self._suppress = True
        self.graph.set_points(points)
        self.point_label.set_label("Drag a point to edit it")
        self.enable_switch.set_active(enabled)
        self._suppress = False

    def _on_point_dragged(self, index, point):
        temp, speed = point
        self.point_label.set_label(f"Point {index + 1}: {temp}°C → {speed}%")

    def _apply(self, _btn):
        profile = self._current_profile()
        fan = self._current_fan()
        data = ",".join(f"{t}c:{s}%" for t, s in self.graph.points)
        enable = "true" if self.enable_switch.get_active() else "false"

        def after_data(ok, err):
            if not ok:
                self.toast_cb(f"Fan curve failed: {err[:80]}")
                return
            run_cmd(
                ["asusctl", "fan-curve", "--mod-profile", profile, "--enable-fan-curve", enable, "--fan", fan],
                lambda ok2, err2: self.toast_cb("Fan curve applied" if ok2 else f"Enable failed: {err2[:80]}"),
            )

        run_cmd(["asusctl", "fan-curve", "--mod-profile", profile, "--fan", fan, "--data", data], after_data)

    def _reset(self, _btn):
        profile = self._current_profile()
        run_cmd(
            ["asusctl", "fan-curve", "--mod-profile", profile, "--default"],
            lambda ok, err: (self.load_curve() if ok else self.toast_cb(f"Reset failed: {err[:80]}")),
        )

    def refresh(self):
        self.stats.refresh()


class ExtraSection(Gtk.Box):
    def __init__(self, toast_cb):
        super().__init__(orientation=Gtk.Orientation.VERTICAL, spacing=10)
        self.toast_cb = toast_cb
        self.set_margin_top(12)
        self.set_margin_bottom(12)
        self.set_margin_start(14)
        self.set_margin_end(14)

        self.append(self._section_label("Keyboard brightness"))
        self.brightness_buttons = Gtk.Box(spacing=6, homogeneous=True, hexpand=True)
        group = None
        self._brightness_btns = {}
        for level, label in zip(BRIGHTNESS_LEVELS, BRIGHTNESS_LABELS):
            btn = Gtk.ToggleButton(label=label, hexpand=True)
            if group is None:
                group = btn
            else:
                btn.set_group(group)
            btn.connect("toggled", self._on_brightness_toggled, level)
            self.brightness_buttons.append(btn)
            self._brightness_btns[level] = btn
        self.append(self.brightness_buttons)

        self.append(self._section_label("Effect"))
        self.effect_dropdown = Gtk.DropDown.new_from_strings(list(AURA_EFFECTS.keys()))
        self.effect_dropdown.set_hexpand(True)
        self.effect_dropdown.connect("notify::selected", self._on_effect_changed)
        self.append(self.effect_dropdown)

        self.theme_sync_row = Gtk.Box(spacing=8)
        theme_label = Gtk.Label(label="Match desktop theme colours")
        theme_label.set_hexpand(True)
        theme_label.set_halign(Gtk.Align.START)
        self.theme_sync_switch = Gtk.Switch()
        self.theme_sync_switch.set_valign(Gtk.Align.CENTER)
        self.theme_sync_switch.connect("state-set", self._on_theme_sync_toggled)
        self.theme_sync_row.append(theme_label)
        self.theme_sync_row.append(self.theme_sync_switch)
        self.append(self.theme_sync_row)

        self.append(self._section_label("Colour"))
        self.swatch_row = Gtk.Box(spacing=8)
        self._colour1 = KBD_COLOURS[0][1] or self._theme_primary()
        for label, hexval in KBD_COLOURS:
            btn = Gtk.Button()
            btn.add_css_class("rog-swatch")
            colour = hexval or self._theme_primary()
            css = Gtk.CssProvider()
            css.load_from_data(f"button {{ background-color: #{colour}; }}".encode())
            btn.get_style_context().add_provider(css, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)
            btn.set_tooltip_text(label)
            btn.connect("clicked", self._on_colour1_clicked, hexval)
            self.swatch_row.append(btn)
        self.append(self.swatch_row)

        self.append(self._section_label("Second colour (breathe / stars)"))
        self.swatch_row2 = Gtk.Box(spacing=8)
        self._colour2 = self._theme_secondary()
        for label, hexval in KBD_COLOURS:
            btn = Gtk.Button()
            btn.add_css_class("rog-swatch")
            colour = hexval or self._theme_primary()
            css = Gtk.CssProvider()
            css.load_from_data(f"button {{ background-color: #{colour}; }}".encode())
            btn.get_style_context().add_provider(css, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)
            btn.set_tooltip_text(label)
            btn.connect("clicked", self._on_colour2_clicked, hexval)
            self.swatch_row2.append(btn)
        self.append(self.swatch_row2)

        self.append(self._section_label("Direction (rainbow wave)"))
        self.direction_dropdown = Gtk.DropDown.new_from_strings(DIRECTIONS)
        self.direction_dropdown.set_hexpand(True)
        self.append(self.direction_dropdown)

        self.append(self._section_label("Speed"))
        self.speed_dropdown = Gtk.DropDown.new_from_strings(["low", "med", "high"])
        self.speed_dropdown.set_selected(1)
        self.speed_dropdown.set_hexpand(True)
        self.append(self.speed_dropdown)

        apply_effect_btn = Gtk.Button(label="Apply effect")
        apply_effect_btn.add_css_class("suggested-action")
        apply_effect_btn.connect("clicked", self._apply_effect)
        self.append(apply_effect_btn)

        self._suppress = False
        self.refresh()
        self._update_effect_fields()

    def _section_label(self, text):
        lbl = Gtk.Label(label=text)
        lbl.set_halign(Gtk.Align.START)
        lbl.add_css_class("rog-section")
        return lbl

    def _theme_primary(self):
        colours = load_material_colors()
        primary = colours.get("primary", "#f7b6b9")
        return primary.lstrip("#")

    def _theme_secondary(self):
        colours = load_material_colors()
        secondary = colours.get("secondary", "#e6bdbe")
        return secondary.lstrip("#")

    def refresh(self):
        self._suppress = True
        idx = read_sysfs_int(KBD_BRIGHTNESS_PATH, 0)
        level = BRIGHTNESS_LEVELS[idx] if idx is not None and 0 <= idx < len(BRIGHTNESS_LEVELS) else "off"
        btn = self._brightness_btns.get(level)
        if btn:
            btn.set_active(True)
        self._suppress = False

    def _on_brightness_toggled(self, btn, level):
        if self._suppress or not btn.get_active():
            return
        run_cmd(["asusctl", "leds", "set", level])

    def _on_theme_sync_toggled(self, switch, state):
        self.swatch_row.set_sensitive(not state)
        self.swatch_row2.set_sensitive(not state)
        if state:
            self._colour1 = self._theme_primary()
            self._colour2 = self._theme_secondary()
            self._apply_effect(None)
        return False

    def _on_colour1_clicked(self, _btn, hexval):
        self._colour1 = hexval or self._theme_primary()
        self._apply_effect(None)

    def _on_colour2_clicked(self, _btn, hexval):
        self._colour2 = hexval or self._theme_primary()
        self._apply_effect(None)

    def _on_effect_changed(self, *_a):
        self._update_effect_fields()

    def _current_effect(self):
        return list(AURA_EFFECTS.keys())[self.effect_dropdown.get_selected()]

    def _update_effect_fields(self):
        needs = AURA_EFFECTS[self._current_effect()]
        self.swatch_row.set_visible("colour1" in needs)
        self.theme_sync_row.set_visible("colour1" in needs)
        self.swatch_row2.set_visible("colour2" in needs)
        self.direction_dropdown.set_visible("direction" in needs)
        self.speed_dropdown.set_visible("speed" in needs)

    def _apply_effect(self, _btn):
        effect = self._current_effect()
        needs = AURA_EFFECTS[effect]
        if self.theme_sync_switch.get_active():
            self._colour1 = self._theme_primary()
            self._colour2 = self._theme_secondary()
        speed = ["low", "med", "high"][self.speed_dropdown.get_selected()]
        direction = DIRECTIONS[self.direction_dropdown.get_selected()]

        cmd = ["asusctl", "aura", "effect", effect]
        if "colour1" in needs:
            cmd += ["-c" if effect in ("static", "pulse", "highlight", "laser", "ripple", "comet", "flash") else "--colour", self._colour1]
        if "colour2" in needs:
            cmd += ["--colour2", self._colour2]
        if "direction" in needs:
            cmd += ["--direction", direction]
        if "speed" in needs:
            cmd += ["--speed", speed]
        run_cmd(cmd, lambda ok, err: (None if ok else self.toast_cb(f"Effect failed: {err[:80]}")))


class AdvancedSection(Gtk.Box):
    """Firmware controls beyond the common set -- rows only appear if this exact
    laptop's firmware actually exposes the attribute (many of these are ROG-model
    specific and read as unavailable on this G733ZW, e.g. the PPT power limits)."""

    def __init__(self, toast_cb):
        super().__init__(orientation=Gtk.Orientation.VERTICAL, spacing=10)
        self.toast_cb = toast_cb
        self.set_margin_top(12)
        self.set_margin_bottom(12)
        self.set_margin_start(14)
        self.set_margin_end(14)

        self.boot_sound_row = self._switch_row("Boot (POST) sound", self._on_boot_sound_toggled)
        self.append(self.boot_sound_row)

        self.pl1_row, self.pl1_spin = self._spin_row("CPU sustained power limit (PL1, W)", 10, 90)
        self.pl1_spin.connect("value-changed", self._on_pl1_changed)
        self.append(self.pl1_row)

        self.pl2_row, self.pl2_spin = self._spin_row("CPU boost power limit (PL2, W)", 10, 120)
        self.pl2_spin.connect("value-changed", self._on_pl2_changed)
        self.append(self.pl2_row)

        self.boost_row, self.boost_spin = self._spin_row("GPU dynamic boost (W)", 0, 25)
        self.boost_spin.connect("value-changed", self._on_boost_changed)
        self.append(self.boost_row)

        self.temp_row, self.temp_spin = self._spin_row("GPU temperature target (°C)", 60, 100)
        self.temp_spin.connect("value-changed", self._on_temp_changed)
        self.append(self.temp_row)

        self.empty_notice = Gtk.Label(label="No additional firmware controls exposed on this laptop.")
        self.empty_notice.add_css_class("rog-subtitle")
        self.append(self.empty_notice)

        self._suppress = False
        self.refresh()

    def _switch_row(self, text, handler):
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

    def _spin_row(self, text, lo, hi):
        row = Gtk.Box(spacing=8)
        label = Gtk.Label(label=text)
        label.set_hexpand(True)
        label.set_halign(Gtk.Align.START)
        spin = Gtk.SpinButton.new_with_range(lo, hi, 1)
        row.append(label)
        row.append(spin)
        return row, spin

    def refresh(self):
        self._suppress = True
        any_advanced = False

        boot_sound = read_sysfs_int(BOOT_SOUND_PATH)
        self.boot_sound_row.set_visible(boot_sound is not None)
        if boot_sound is not None:
            self.boot_sound_row.switch.set_active(boot_sound == 1)

        for row, spin, path in (
            (self.pl1_row, self.pl1_spin, PPT_PL1_PATH),
            (self.pl2_row, self.pl2_spin, PPT_PL2_PATH),
            (self.boost_row, self.boost_spin, NV_BOOST_PATH),
            (self.temp_row, self.temp_spin, NV_TEMP_TARGET_PATH),
        ):
            value = read_sysfs_int(path)
            row.set_visible(value is not None)
            if value is not None:
                spin.set_value(value)
                any_advanced = True

        self.empty_notice.set_visible(not any_advanced)
        self._suppress = False

    def _on_boot_sound_toggled(self, switch, state):
        if self._suppress:
            return False
        run_cmd(["asusctl", "armoury", "set", "boot_sound", "1" if state else "0"], self._report)
        return False

    def _on_pl1_changed(self, spin):
        if self._suppress:
            return
        run_cmd(["asusctl", "armoury", "set", "ppt_pl1_spl", str(int(spin.get_value()))], self._report)

    def _on_pl2_changed(self, spin):
        if self._suppress:
            return
        run_cmd(["asusctl", "armoury", "set", "ppt_pl2_sppt", str(int(spin.get_value()))], self._report)

    def _on_boost_changed(self, spin):
        if self._suppress:
            return
        run_cmd(["asusctl", "armoury", "set", "nv_dynamic_boost", str(int(spin.get_value()))], self._report)

    def _on_temp_changed(self, spin):
        if self._suppress:
            return
        run_cmd(["asusctl", "armoury", "set", "nv_temp_target", str(int(spin.get_value()))], self._report)

    def _report(self, ok, err):
        if not ok:
            self.toast_cb(f"Command failed: {err[:80]}")


class RogPopup(Gtk.Window):
    def __init__(self, app):
        super().__init__(application=app, title="ROG Control")
        self.set_default_size(480, 720)
        self.set_size_request(420, 560)
        self.set_resizable(True)
        self.set_decorated(True)

        self.toast_overlay = Adw.ToastOverlay()
        self.set_child(self.toast_overlay)

        toolbar_view = Adw.ToolbarView()
        self.toast_overlay.set_child(toolbar_view)

        self.view_stack = Adw.ViewStack()
        switcher_title = Adw.ViewSwitcherTitle(stack=self.view_stack, title="ROG Control")
        header = Adw.HeaderBar(title_widget=switcher_title)
        toolbar_view.add_top_bar(header)

        switcher_bar = Adw.ViewSwitcherBar(stack=self.view_stack)
        switcher_bar.set_reveal(True)
        toolbar_view.add_bottom_bar(switcher_bar)
        toolbar_view.set_content(self.view_stack)

        self.main_section = MainSection(self.show_toast)
        self.fan_section = FanSection(self.show_toast)
        self.extra_section = ExtraSection(self.show_toast)
        self.advanced_section = AdvancedSection(self.show_toast)

        self.view_stack.add_titled_with_icon(self._scroll(self.main_section), "main", "Main", "utilities-terminal-symbolic")
        self.view_stack.add_titled_with_icon(self._scroll(self.fan_section), "fan", "Fan", "weather-windy-symbolic")
        self.view_stack.add_titled_with_icon(self._scroll(self.extra_section), "extra", "RGB", "input-keyboard-symbolic")
        self.view_stack.add_titled_with_icon(self._scroll(self.advanced_section), "advanced", "Advanced", "applications-engineering-symbolic")

        self.connect("close-request", self._on_close_request)
        self._refresh_source = None

    def _scroll(self, widget):
        sw = Gtk.ScrolledWindow()
        sw.set_child(widget)
        sw.set_vexpand(True)
        return sw

    def show_toast(self, text):
        self.toast_overlay.add_toast(Adw.Toast.new(text))

    def _on_close_request(self, *_a):
        self.hide()
        self._stop_polling()
        return True

    def toggle(self):
        if self.get_visible():
            self.hide()
            self._stop_polling()
        else:
            self.main_section.refresh()
            self.fan_section.refresh()
            self.extra_section.refresh()
            self.advanced_section.refresh()
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
        self.main_section.stats.refresh()
        self.fan_section.stats.refresh()
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
            "ToolTip": ("", dbus.Array([], signature="(iiay)"), "ROG Control", "Left-click: open · scroll: cycle profile · right-click: quit"),
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


def register_tray(on_activate, on_secondary_activate, on_scroll):
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
    return item


class RogControlApp(Adw.Application):
    def __init__(self):
        super().__init__(application_id=APP_ID, flags=Gio.ApplicationFlags.IS_SERVICE)
        self.popup = None
        self.hold()  # keep running with no window (tray-resident)

    def do_startup(self):
        Adw.Application.do_startup(self)
        register_icons()
        css = Gtk.CssProvider()
        css.load_from_data(build_material_css(load_material_colors()).encode())
        Gtk.StyleContext.add_provider_for_display(
            Gdk.Display.get_default(), css, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION
        )
        register_tray(self._toggle_popup, self._quit_from_tray, self._on_scroll)

    def do_activate(self):
        self._toggle_popup()

    def _toggle_popup(self):
        try:
            if self.popup is None:
                self.popup = RogPopup(self)
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
        run_cmd(["asusctl", "profile", "next"],
                lambda ok, err: (self.popup.main_section.refresh() if ok and self.popup else None))


def main():
    app = RogControlApp()
    app.run(None)


if __name__ == "__main__":
    main()
