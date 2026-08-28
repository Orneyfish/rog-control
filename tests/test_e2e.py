#!/usr/bin/env python3
"""End-to-end / regression test suite for ROG Control.

Uses the same technique that actually found every real bug fixed this
session (RGB race condition, GPU-temp flicker, the nvidia-smi exit-code lie,
the fan-curve --get-enabled parsing bug, the battery-scale range mismatch,
the Fans+Power window overlap, the slider-drag flood): construct the real
widget classes directly in-process and drive them via their real methods,
rather than simulating clicks. Synthetic mouse/keyboard input (ydotool) was
tried extensively in this session and proved unreliable in this sandbox
(miscalibrated coordinates, races against the compositor); this approach
exercises the exact same signal handlers a real click fires, without that
unreliability, and is fast enough to run on every change.

Run with:  python3 -m unittest discover -s tests -v
Or:        python3 tests/test_e2e.py

Tests that would change real hardware state (GPU MUX mode switch -> reboot,
actual reboot) are NEVER exercised for real -- either the underlying command
is mocked, or only the pure decision logic (e.g. gpu_mode_needs_reboot) is
checked. Tests that read real hardware (sysfs, asusd D-Bus state) are safe,
read-only, and used to catch drift (e.g. hwmon renumbering) rather than
assert exact values that could change between runs.
"""
import json
import os
import sys
import unittest
import unittest.mock as mock

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import gi
gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
from gi.repository import Adw, Gdk, GLib, Gtk

Adw.init()

import config
import main


def pump(ms=50):
    """Drain pending GLib main-loop iterations for `ms` milliseconds -- lets
    GLib.timeout_add/idle_add callbacks (debouncers, async probes) actually
    fire during a test."""
    loop = GLib.MainLoop()
    GLib.timeout_add(ms, loop.quit)
    loop.run()


# ---------------------------------------------------------------------------
# Pure logic -- no GTK widgets, fast, no hardware access.
# ---------------------------------------------------------------------------

class TestSysfsHelpers(unittest.TestCase):
    def test_read_sysfs_missing_file_returns_default(self):
        self.assertEqual(main.read_sysfs("/no/such/path", "fallback"), "fallback")

    def test_read_sysfs_none_path_returns_default(self):
        # Regression guard: read_sysfs must not raise TypeError on a None
        # path (a resolver like _resolve_hwmon_paths can legitimately return
        # None if a sensor isn't found).
        self.assertIsNone(main.read_sysfs(None))
        self.assertEqual(main.read_sysfs(None, "x"), "x")

    def test_read_sysfs_int_non_numeric_returns_default(self):
        with mock.patch.object(main, "read_sysfs", return_value="unavailable"):
            self.assertEqual(main.read_sysfs_int("/whatever", -1), -1)


class TestHwmonResolution(unittest.TestCase):
    """Regression test for the CPU-temp-reads-RAM-sensor bug: CPU_TEMP_PATH
    must resolve to the coretemp driver, never a hardcoded hwmonN index."""

    def test_cpu_temp_path_is_coretemp_not_hardcoded(self):
        if main.CPU_TEMP_PATH is None:
            self.skipTest("no coretemp hwmon device on this machine")
        name_path = os.path.join(os.path.dirname(main.CPU_TEMP_PATH), "name")
        with open(name_path) as f:
            self.assertEqual(f.read().strip(), "coretemp")

    def test_fan_paths_are_asus_driver(self):
        for path in (main.CPU_FAN_PATH, main.GPU_FAN_PATH):
            if path is None:
                self.skipTest("no asus hwmon fan device on this machine")
                continue
            name_path = os.path.join(os.path.dirname(path), "name")
            with open(name_path) as f:
                self.assertEqual(f.read().strip(), "asus")

    def test_resolved_sensors_actually_readable(self):
        for path in (main.CPU_TEMP_PATH, main.CPU_FAN_PATH, main.GPU_FAN_PATH):
            if path is None:
                continue
            self.assertIsNotNone(main.read_sysfs_int(path), f"{path} did not yield an int")


class TestColourHelpers(unittest.TestCase):
    def test_hex_gdk_rgba_round_trip(self):
        for hexval in ("ff0000", "00ff00", "112233", "f2bc90"):
            rgba = main.hex_to_gdk_rgba(hexval)
            self.assertEqual(main.gdk_rgba_to_hex(rgba), hexval)

    def test_theme_colours_match_live_scheme(self):
        try:
            with open(main.SCHEME_JSON_PATH) as f:
                data = json.load(f)
        except OSError:
            self.skipTest("caelestia scheme.json not present on this machine")
        expected_primary = data["colours"]["primary"].lstrip("#")
        expected_secondary = data["colours"]["secondary"].lstrip("#")
        self.assertEqual(main.theme_primary_hex(), expected_primary)
        self.assertEqual(main.theme_secondary_hex(), expected_secondary)


class TestAuraModeMapping(unittest.TestCase):
    """AURA_MODE_INDEX/AURA_INDEX_MODE were derived empirically (asusctl has
    no documented mapping) -- guard that the two stay exact inverses and
    every AURA_EFFECTS key has an index."""

    def test_index_maps_are_exact_inverses(self):
        for name, idx in main.AURA_MODE_INDEX.items():
            self.assertEqual(main.AURA_INDEX_MODE[idx], name)
        self.assertEqual(len(main.AURA_MODE_INDEX), len(main.AURA_INDEX_MODE))

    def test_every_effect_has_a_mode_index(self):
        for effect in main.AURA_EFFECTS:
            self.assertIn(effect, main.AURA_MODE_INDEX, f"{effect} missing from AURA_MODE_INDEX")


class TestGpuModeLogic(unittest.TestCase):
    """Pure decision logic only -- never actually flips gpu_mux_mode (that
    needs a reboot on real hardware)."""

    def test_current_gpu_mode_ultimate_when_mux_zero(self):
        cfg = {"automation": {"gpu_optimized": False}}
        with mock.patch.object(main, "read_sysfs_int", return_value=0):
            self.assertEqual(main.current_gpu_mode(cfg), "Ultimate")

    def test_current_gpu_mode_optimized_when_flag_set_and_mux_one(self):
        cfg = {"automation": {"gpu_optimized": True}}

        def fake_read(path, default=None):
            return 1 if path == main.GPU_MUX_PATH else 0

        with mock.patch.object(main, "read_sysfs_int", side_effect=fake_read):
            self.assertEqual(main.current_gpu_mode(cfg), "Optimized")

    def test_current_gpu_mode_eco_vs_standard_by_dgpu_disable(self):
        cfg = {"automation": {"gpu_optimized": False}}

        def make_reader(dgpu_disable_val):
            def fake_read(path, default=None):
                if path == main.GPU_MUX_PATH:
                    return 1
                if path == main.DGPU_DISABLE_PATH:
                    return dgpu_disable_val
                return default
            return fake_read

        with mock.patch.object(main, "read_sysfs_int", side_effect=make_reader(1)):
            self.assertEqual(main.current_gpu_mode(cfg), "Eco")
        with mock.patch.object(main, "read_sysfs_int", side_effect=make_reader(0)):
            self.assertEqual(main.current_gpu_mode(cfg), "Standard")

    def test_gpu_mode_needs_reboot_only_for_mux_change(self):
        with mock.patch.object(main, "read_sysfs_int", return_value=0):  # currently Ultimate
            self.assertFalse(main.gpu_mode_needs_reboot("Ultimate"))
            self.assertTrue(main.gpu_mode_needs_reboot("Standard"))
            self.assertTrue(main.gpu_mode_needs_reboot("Eco"))
            self.assertTrue(main.gpu_mode_needs_reboot("Optimized"))
        with mock.patch.object(main, "read_sysfs_int", return_value=1):  # currently hybrid
            self.assertTrue(main.gpu_mode_needs_reboot("Ultimate"))
            self.assertFalse(main.gpu_mode_needs_reboot("Standard"))
            self.assertFalse(main.gpu_mode_needs_reboot("Eco"))
            self.assertFalse(main.gpu_mode_needs_reboot("Optimized"))

    def test_apply_gpu_mode_never_touches_mux_when_already_correct(self):
        """If mux already matches the target, apply_gpu_mode must not shell
        out to change it (only Ultimate<->hybrid transitions need a reboot;
        toggling among Eco/Standard/Optimized while already hybrid must not
        trigger one)."""
        calls = []
        with mock.patch.object(main, "read_sysfs_int", return_value=1):
            with mock.patch.object(main, "run_cmd", side_effect=lambda cmd, on_done=None: calls.append(cmd)):
                main.apply_gpu_mode("Standard", on_battery=False)
        mux_calls = [c for c in calls if "gpu_mux_mode" in c]
        self.assertEqual(mux_calls, [], "apply_gpu_mode changed MUX when it was already correct")


class TestProfilePowerPlan(unittest.TestCase):
    def test_every_profile_has_a_power_plan(self):
        for p in main.PROFILES:
            self.assertIn(p, main.PROFILE_POWER_PLAN)

    def test_apply_profile_pairs_power_profile(self):
        calls = []
        with mock.patch.object(main, "run_cmd", side_effect=lambda cmd, on_done=None: calls.append(cmd)):
            with mock.patch.object(main, "POWERPROFILESCTL", "/usr/bin/powerprofilesctl"):
                main.apply_profile("Quiet")
        self.assertIn(["asusctl", "profile", "set", "Quiet"], calls)
        self.assertIn(["/usr/bin/powerprofilesctl", "set", "power-saver"], calls)


class TestDebouncer(unittest.TestCase):
    def test_rapid_triggers_coalesce_to_one_call(self):
        calls = []
        d = main.Debouncer(50, lambda: calls.append(1))
        for _ in range(20):
            d.trigger()
        pump(300)
        self.assertEqual(len(calls), 1)

    def test_separated_triggers_each_fire(self):
        calls = []
        d = main.Debouncer(30, lambda: calls.append(1))
        d.trigger()
        pump(150)
        d.trigger()
        pump(150)
        self.assertEqual(len(calls), 2)


class TestNvidiaSmiExitCodeLie(unittest.TestCase):
    """Regression test for the confirmed bug: `nvidia-smi -pl` returns exit
    0 even when it explicitly refuses the change."""

    def test_not_supported_text_treated_as_failure_despite_exit_zero(self):
        fake_result = mock.Mock(returncode=0, stdout="Changing power management limit is not supported in current scope for GPU: 0000\nAll done.\n", stderr="")
        with mock.patch("subprocess.run", return_value=fake_result):
            ok, err = main._nvidia_smi_pl(150)
        self.assertFalse(ok)
        self.assertIn("not supported", err)

    def test_real_success_with_exit_zero_and_no_warning(self):
        fake_result = mock.Mock(returncode=0, stdout="150\n", stderr="")
        with mock.patch("subprocess.run", return_value=fake_result):
            ok, err = main._nvidia_smi_pl(150)
        self.assertTrue(ok)


class TestFanCurveGraphMonotonicSpeed(unittest.TestCase):
    """Regression test: asusctl rejects a fan curve where speed isn't
    monotonically non-decreasing with temperature ("Invalid percentage,
    previous value X is higher than next value Y", confirmed directly against
    the real asusctl binary). Dragging a point must clamp its speed between
    its neighbours, the same way temperature is already clamped, or the user
    can produce a curve that silently fails to apply."""

    def test_drag_cannot_produce_non_monotonic_speed(self):
        graph = main.FanCurveGraph()
        graph.points = [(40, 20), (50, 30), (55, 40), (60, 50), (65, 60), (70, 75), (80, 90), (95, 100)]
        graph._dragging = 2  # the (55, 40) point
        graph._drag_origin = (0, 0)
        w, h = 400, 220

        def fake_geometry():
            return w, h
        graph._geometry = fake_geometry

        # Try to drag point 2's speed to 95% -- far above its neighbours (30,
        # 50) -- by feeding _from_px a pixel position that would decode to a
        # very high speed if unclamped.
        with mock.patch.object(main.FanCurveGraph, "_from_px", return_value=(55, 95)):
            graph._on_drag_update(None, 0, 0)

        temp, speed = graph.points[2]
        prev_speed = graph.points[1][1]
        next_speed = graph.points[3][1]
        self.assertLessEqual(speed, next_speed, "point's speed was allowed above the next point's speed")
        self.assertGreaterEqual(speed, prev_speed, "point's speed was allowed below the previous point's speed")

    def test_resulting_curve_is_accepted_by_the_real_monotonic_check(self):
        """Belt-and-suspenders: build a curve via the same clamp logic and
        confirm it's actually well-formed (non-decreasing pwm alongside
        non-decreasing temp), independent of asusctl being installed."""
        graph = main.FanCurveGraph()
        points = [(40, 20), (50, 30), (55, 40), (60, 50), (65, 60), (70, 75), (80, 90), (95, 100)]
        graph.points = list(points)
        for i in range(len(points)):
            graph._dragging = i
            graph._drag_origin = (0, 0)
            graph._geometry = lambda: (400, 220)
            with mock.patch.object(main.FanCurveGraph, "_from_px", return_value=(points[i][0], 999)):
                graph._on_drag_update(None, 0, 0)
        speeds = [s for _t, s in graph.points]
        self.assertEqual(speeds, sorted(speeds), "curve produced by dragging every point to the max is not monotonic")


class TestFactoryDefaultsOnlyResetsActiveProfile(unittest.TestCase):
    """Regression test: `asusctl fan-curve --default` only ever resets the
    *currently active* profile's curve -- `--mod-profile` is accepted
    alongside it but silently ignored for this specific operation (confirmed
    directly: running `--mod-profile Quiet --default` while Performance was
    active left Quiet's curve completely untouched). The editor must refuse
    rather than send a --default call that would silently reset the wrong
    (active) profile instead of the one actually being edited."""

    def test_refuses_when_edited_profile_is_not_active(self):
        calls = []
        editor = main.FanCurveEditor("cpu", lambda *_a: None, lambda: "Quiet")
        editor.toast_cb = lambda msg: calls.append(msg)
        with mock.patch.object(main, "read_sysfs", return_value="performance"), \
             mock.patch.object(main, "run_cmd") as run_cmd_mock:
            editor._reset(None)
        run_cmd_mock.assert_not_called()
        self.assertTrue(calls and "Quiet" in calls[0] and "Performance" in calls[0])

    def test_proceeds_when_edited_profile_is_active(self):
        editor = main.FanCurveEditor("cpu", lambda *_a: None, lambda: "Performance")
        with mock.patch.object(main, "read_sysfs", return_value="performance"), \
             mock.patch.object(main, "run_cmd") as run_cmd_mock:
            editor._reset(None)
        run_cmd_mock.assert_called_once()
        self.assertIn("--default", run_cmd_mock.call_args[0][0])


class TestFanCurveParsing(unittest.TestCase):
    """Regression test for the --get-enabled-ignores-profile bug: the parser
    must read the RON-ish `--mod-profile <Name>` output format correctly."""

    SAMPLE = """
Fan curves for Balanced

[
    (
        fan: CPU,
        pwm: (20, 66, 79, 96, 119, 127, 183, 255),
        temp: (40, 54, 58, 62, 95, 95, 95, 97),
        enabled: true,
    ),
    (
        fan: GPU,
        pwm: (36, 56, 89, 105, 117, 153, 168, 255),
        temp: (47, 51, 55, 60, 65, 70, 76, 97),
        enabled: false,
    ),
]
"""

    def test_parses_cpu_curve(self):
        points, enabled = main.FanCurveEditor._parse_curve(self.SAMPLE, "cpu")
        self.assertTrue(enabled)
        self.assertEqual(points[0], (40, round(20 / 255 * 100)))
        self.assertEqual(points[-1], (97, 100))
        self.assertEqual(len(points), 8)

    def test_parses_gpu_curve_independently(self):
        points, enabled = main.FanCurveEditor._parse_curve(self.SAMPLE, "gpu")
        self.assertFalse(enabled)
        self.assertEqual(points[0], (47, round(36 / 255 * 100)))
        self.assertNotEqual(points, main.FanCurveEditor._parse_curve(self.SAMPLE, "cpu")[0])

    def test_missing_fan_falls_back_to_default(self):
        points, enabled = main.FanCurveEditor._parse_curve("garbage, no data here", "cpu")
        self.assertFalse(enabled)
        self.assertEqual(len(points), 8)


class TestConfigPersistence(unittest.TestCase):
    def test_defaults_present_after_load_with_empty_file(self):
        cfg = config._merge_defaults({}, config.DEFAULTS)
        self.assertIn("automation", cfg)
        for key in ("gpu_optimized", "screen_auto", "profile_battery", "profile_ac", "kbd_battery", "kbd_ac"):
            self.assertIn(key, cfg["automation"])

    def test_merge_preserves_existing_values(self):
        existing = {"automation": {"gpu_optimized": True}}
        merged = config._merge_defaults(existing, config.DEFAULTS)
        self.assertTrue(merged["automation"]["gpu_optimized"])
        self.assertIn("screen_auto", merged["automation"])  # backfilled

    def test_save_load_round_trip(self):
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            with mock.patch.object(config, "CONFIG_DIR", tmp), \
                 mock.patch.object(config, "CONFIG_PATH", os.path.join(tmp, "config.json")):
                cfg = config.load()
                cfg["automation"]["profile_ac"] = "Performance"
                config.save(cfg)
                reloaded = config.load()
        self.assertEqual(reloaded["automation"]["profile_ac"], "Performance")


# ---------------------------------------------------------------------------
# Widget construction -- every panel/window must build without exception.
# Uses a disposable Gtk.Window as a synthetic top-level so widgets that need
# get_root()/present() have somewhere to attach without touching the real
# desktop (none of these are ever .present()-ed to screen).
# ---------------------------------------------------------------------------

def make_cfg():
    # Delegates to the real default-merging logic instead of hand-duplicating
    # the schema here -- a hand-duplicated copy is exactly what went stale
    # and broke every FansPowerWindow-constructing test the moment
    # config.DEFAULTS grew a new "power_limits" section.
    return config._merge_defaults({}, config.DEFAULTS)


class TestWidgetConstruction(unittest.TestCase):
    def setUp(self):
        self.cfg = make_cfg()
        self.toast_cb = lambda *_a: None
        self.save_cb = lambda: None
        self.get_on_battery = lambda: False

    def test_performance_panel(self):
        main.PerformancePanel(self.toast_cb, self.cfg, self.save_cb, self.get_on_battery, lambda: None)

    def test_gpu_panel(self):
        main.GpuPanel(self.toast_cb, self.cfg, self.save_cb, self.get_on_battery, lambda: None)

    def test_screen_panel(self):
        main.ScreenPanel(self.toast_cb, self.cfg, self.save_cb)

    def test_keyboard_panel(self):
        main.KeyboardPanel(self.toast_cb, self.cfg, self.save_cb, self.get_on_battery, lambda: None)

    def test_battery_panel(self):
        main.BatteryPanel(self.toast_cb)

    def test_startup_panel(self):
        main.StartupPanel(self.toast_cb)

    def test_footer_panel(self):
        main.FooterPanel(lambda: None, lambda: None)

    def test_fans_power_window(self):
        win = main.FansPowerWindow(None, self.toast_cb, self.cfg, self.save_cb)
        pump(400)  # let async nvidia probes + power-mode fetch settle
        self.assertIsNotNone(win)

    def test_lighting_window(self):
        main.LightingWindow(None, self.toast_cb, lambda: None)

    def test_extra_window(self):
        main.ExtraWindow(None, self.toast_cb)

    def test_full_rog_popup(self):
        app = Adw.Application(application_id="com.test.ConstructionCheck")
        app.register()  # real app only ever builds windows post-startup; match that here
        popup = main.RogPopup(app, self.cfg, self.save_cb, self.get_on_battery, lambda: None)
        self.assertIsNotNone(popup)


# ---------------------------------------------------------------------------
# Regression tests for specific confirmed-and-fixed bugs, using the real
# widget classes.
# ---------------------------------------------------------------------------

class TestGpuTempNoFlicker(unittest.TestCase):
    """Regression test: GpuPanel.refresh() must never blank the stat label
    to a placeholder before the async nvidia-smi fetch lands."""

    def test_refresh_never_shows_placeholder_after_first_real_reading(self):
        panel = main.GpuPanel(lambda *_a: None, make_cfg(), lambda: None, lambda: False, lambda: None)
        pump(500)  # let the first real async fetch land
        seen_blank = []

        def track():
            if "GPU: --  Fan:" in panel.header.stat_label.get_label():
                seen_blank.append(True)
            panel.refresh()
            return True

        source = GLib.timeout_add(100, track)
        pump(1200)
        GLib.source_remove(source)
        self.assertEqual(seen_blank, [], "GPU stat label blanked to placeholder during refresh")


class TestModeSelectorVisibility(unittest.TestCase):
    """Regression test: the fan-curve profile selector must only exist
    inside whichever fan sub-page is actually visible."""

    def test_hidden_on_power_shown_on_fan_curve(self):
        win = main.FansPowerWindow(None, lambda *_a: None, make_cfg(), lambda: None)
        pump(50)
        self.assertFalse(win.mode_row.get_visible())

        win.cpu_fan_btn.set_active(True)
        self.assertTrue(win.mode_row.get_visible())
        self.assertIs(win.mode_row.get_parent(), win.cpu_fan_page)

        win.gpu_nav.set_active(True)
        self.assertFalse(win.mode_row.get_visible(), "GPU tab defaults to Power view, selector should hide")

        win.gpu_fan_btn.set_active(True)
        self.assertTrue(win.mode_row.get_visible())
        self.assertIs(win.mode_row.get_parent(), win.gpu_fan_page)


class TestSliderDebounce(unittest.TestCase):
    """Regression test: a rapid slider drag (many value-changed ticks) must
    collapse to exactly one applied command, not one per tick."""

    def test_pl1_rapid_drag_sends_one_command(self):
        calls = []
        with mock.patch.object(main, "run_cmd", side_effect=lambda cmd, on_done=None: calls.append(cmd)):
            win = main.FansPowerWindow(None, lambda *_a: None, make_cfg(), lambda: None)
            pump(50)
            win._suppress = False
            for v in range(10, 40):
                win.pl1.scale.set_value(v)
            pump(400)
        pl1_calls = [c for c in calls if "ppt_pl1_spl" in c]
        self.assertEqual(len(pl1_calls), 1, f"expected 1 debounced call, got {len(pl1_calls)}")
        self.assertEqual(pl1_calls[0][-1], "39")


class TestRgbDebounceAndPreservation(unittest.TestCase):
    """Regression tests for the RGB race condition and the "reset to theme
    silently no-ops on colourless effects" bug -- against mocked run_cmd
    (deterministic, no real hardware writes, safe to run repeatedly)."""

    def test_rapid_multi_field_change_sends_one_command_with_final_values(self):
        calls = []
        with mock.patch.object(main, "run_cmd", side_effect=lambda cmd, on_done=None: calls.append(cmd)):
            win = main.LightingWindow(None, lambda *_a: None, lambda: None)
            pump(50)
            effects = list(main.AURA_EFFECTS.keys())
            win.effect_dropdown.set_selected(effects.index("breathe"))
            win.colour1_btn.set_rgba(main.hex_to_gdk_rgba("112233"))
            win.colour2_btn.set_rgba(main.hex_to_gdk_rgba("445566"))
            win.speed_dropdown.set_selected(2)
            pump(400)
        effect_calls = [c for c in calls if c[:3] == ["asusctl", "aura", "effect"]]
        self.assertEqual(len(effect_calls), 1, f"expected 1 debounced apply, got {len(effect_calls)}")
        cmd = effect_calls[0]
        self.assertIn("112233", cmd)
        self.assertIn("445566", cmd)
        self.assertIn("high", cmd)

    def test_reset_to_theme_switches_to_static_when_effect_has_no_colour(self):
        calls = []
        with mock.patch.object(main, "run_cmd", side_effect=lambda cmd, on_done=None: calls.append(cmd)):
            win = main.LightingWindow(None, lambda *_a: None, lambda: None)
            pump(50)
            effects = list(main.AURA_EFFECTS.keys())
            win.effect_dropdown.set_selected(effects.index("rainbow-cycle"))
            pump(50)
            win._reset_to_theme(None)
            pump(400)
        effect_calls = [c for c in calls if c[:3] == ["asusctl", "aura", "effect"]]
        self.assertTrue(effect_calls, "reset-to-theme sent no command at all")
        self.assertEqual(effect_calls[-1][3], "static", "should have switched to an effect that uses colour")
        self.assertIn(main.theme_primary_hex(), effect_calls[-1])


class TestPowerLimitsPersistAcrossReopen(unittest.TestCase):
    """Regression test: PL1/PL2/boost/temp_target/core_clock/mem_clock/
    gpu_power/apply_limits have no real hardware value to read back on this
    laptop, and FansPowerWindow is destroyed and rebuilt fresh every time
    it's closed and reopened -- without saving the user's last-set value to
    config, the sliders always reset to their construction default, reported
    directly as "I applied power limits to max but everything resets"."""

    def test_pl1_value_survives_window_close_and_reopen(self):
        cfg = config._merge_defaults({}, config.DEFAULTS)
        saved = []
        save_cb = lambda: saved.append(json.loads(json.dumps(cfg)))  # deep-ish snapshot

        with mock.patch.object(main, "run_cmd"):
            win1 = main.FansPowerWindow(None, lambda *_a: None, cfg, save_cb)
            pump(50)
            win1._suppress = False
            win1.pl1.scale.set_value(77)
            pump(300)  # let the debounced apply (which persists) fire

            self.assertEqual(cfg["power_limits"]["pl1"], 77)

            # Simulate close+reopen: a brand new instance, same cfg dict --
            # exactly what RogPopup.open_fans() does (fans_window set to
            # None on close, rebuilt on next open).
            win2 = main.FansPowerWindow(None, lambda *_a: None, cfg, save_cb)
            pump(50)

        self.assertEqual(win2.pl1.scale.get_value(), 77, "PL1 reset instead of restoring the last-set value")

    def test_apply_limits_switch_persists(self):
        cfg = config._merge_defaults({}, config.DEFAULTS)
        with mock.patch.object(main, "run_cmd"):
            win1 = main.FansPowerWindow(None, lambda *_a: None, cfg, lambda: None)
            pump(50)
            win1._suppress = False
            win1._on_apply_limits_toggled(win1.apply_limits_row.switch, True)
            self.assertTrue(cfg["power_limits"]["apply_limits"])

            win2 = main.FansPowerWindow(None, lambda *_a: None, cfg, lambda: None)
            pump(50)
        self.assertTrue(win2.apply_limits_row.switch.get_active())


class TestBatteryScaleRange(unittest.TestCase):
    def test_range_matches_asusctl_supported_range(self):
        panel = main.BatteryPanel(lambda *_a: None)
        adj = panel.scale.get_adjustment()
        self.assertEqual(adj.get_lower(), 20)
        self.assertEqual(adj.get_upper(), 100)


class TestNoDynamicDockingLeftover(unittest.TestCase):
    """Regression guard: the unreliable runtime docking approach was removed
    in favour of a static Hyprland window rule -- it must not silently come
    back."""

    def test_open_dock_left_function_removed(self):
        self.assertFalse(hasattr(main, "open_dock_left"), "open_dock_left should have been removed")


class TestHyprlandRulesSyntax(unittest.TestCase):
    """Sanity-check the Hyprland Lua rules file this app depends on for
    window placement: balanced parens/braces, and the app's rules present."""

    RULES_PATH = os.path.expanduser("~/.config/hypr/custom/rules.lua")

    def test_rules_file_parses_with_balanced_delimiters(self):
        if not os.path.exists(self.RULES_PATH):
            self.skipTest("rules.lua not present on this machine")
        with open(self.RULES_PATH) as f:
            text = f.read()
        for open_ch, close_ch in (("(", ")"), ("{", "}")):
            self.assertEqual(text.count(open_ch), text.count(close_ch), f"unbalanced {open_ch}{close_ch} in rules.lua")

    def test_main_window_and_companion_rules_present(self):
        if not os.path.exists(self.RULES_PATH):
            self.skipTest("rules.lua not present on this machine")
        with open(self.RULES_PATH) as f:
            text = f.read()
        self.assertIn('title = "^ROG Control$"', text)
        self.assertIn("Fans and Power", text)


if __name__ == "__main__":
    unittest.main(verbosity=2)
