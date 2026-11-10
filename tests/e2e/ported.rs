#![allow(non_snake_case)]
//! Rust ports of the Python scenarios in `tests/test_e2e.py` @ 509d5e4, one test each.
//! Every test carries a `ports:` comment naming its Python original.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gtk::gio;
use gtk::prelude::*;
use rog_control::config;
use rog_control::extra::ExtraWindow;
use rog_control::fans::{parse_curve, FanCurveEditor, FanCurveGraph, FansPowerWindow, DEFAULT_CURVE};
use rog_control::hw::*;
use rog_control::lighting::LightingWindow;
use rog_control::panels::*;
use rog_control::popup::RogPopup;
use rog_control::sys::{hook_sysfs, py_round, read_sysfs, read_sysfs_int, SysfsGuard};
use rog_control::theme::*;
use rog_control::widgets::Debouncer;
use serde_json::json;

use crate::common::*;

/// Every sysfs path (and every directory listing) answers `value` / nothing.
fn sysfs_all(value: &'static str) -> SysfsGuard {
    hook_sysfs(move |_| Some(value.to_string()), |_| Some(vec![]))
}

fn is(argv: &[String], expect: &[&str]) -> bool {
    argv.iter().map(String::as_str).eq(expect.iter().copied())
}

fn monitors_capture(text: String) -> rog_control::sys::CaptureGuard {
    fake_capture(move |argv| is(argv, &["hyprctl", "monitors", "-j"]).then(|| ok_out(&text)))
}

pub fn tests() -> Vec<(&'static str, fn())> {
    macro_rules! t {
        ($($name:ident),* $(,)?) => { vec![$((stringify!($name), $name as fn())),*] };
    }
    t![
        sysfs_helpers__read_sysfs_missing_file_returns_default,
        sysfs_helpers__read_sysfs_none_path_returns_default,
        sysfs_helpers__read_sysfs_int_non_numeric_returns_default,
        hwmon_resolution__cpu_temp_path_is_coretemp_not_hardcoded,
        hwmon_resolution__fan_paths_are_asus_driver,
        hwmon_resolution__resolved_sensors_actually_readable,
        colour_helpers__hex_gdk_rgba_round_trip,
        colour_helpers__theme_colours_match_live_scheme,
        aura_mode_mapping__index_maps_are_exact_inverses,
        aura_mode_mapping__every_effect_has_a_mode_index,
        current_monitor__picks_edp_even_when_listed_second,
        current_monitor__picks_edp_even_when_listed_first,
        current_monitor__falls_back_to_first_monitor_when_no_internal_panel,
        screen_preset__uses_hyprctl_eval_not_keyword,
        screen_preset__preserves_position_and_output_name,
        gpu_mode__current_gpu_mode_ultimate_when_mux_zero,
        gpu_mode__current_gpu_mode_optimized_when_flag_set_and_mux_one,
        gpu_mode__current_gpu_mode_eco_vs_standard_by_dgpu_disable,
        gpu_mode__needs_reboot_only_for_mux_change,
        gpu_mode__apply_never_touches_mux_when_already_correct,
        gpu_mode__apply_never_touches_dgpu_disable_in_same_call_as_a_mux_change,
        gpu_mode__refuses_to_disable_dgpu_when_external_display_attached,
        gpu_mode__proceeds_normally_when_no_external_display_on_dgpu,
        external_display__matches_real_hardware_state,
        external_display__internal_panel_alone_is_not_treated_as_external,
        external_display__non_edp_connected_on_nvidia_card_is_external,
        external_display__ignores_non_nvidia_cards,
        profile_power_plan__every_profile_has_a_power_plan,
        profile_power_plan__apply_profile_pairs_power_profile,
        debouncer__rapid_triggers_coalesce_to_one_call,
        debouncer__separated_triggers_each_fire,
        nvidia_smi__not_supported_text_treated_as_failure_despite_exit_zero,
        nvidia_smi__real_success_with_exit_zero_and_no_warning,
        fan_curve_graph__drag_cannot_produce_non_monotonic_speed,
        fan_curve_graph__resulting_curve_is_accepted_by_the_real_monotonic_check,
        factory_defaults__refuses_when_edited_profile_is_not_active,
        factory_defaults__proceeds_when_edited_profile_is_active,
        fan_curve_parsing__parses_cpu_curve,
        fan_curve_parsing__parses_gpu_curve_independently,
        fan_curve_parsing__missing_fan_falls_back_to_default,
        config_persistence__defaults_present_after_load_with_empty_file,
        config_persistence__merge_preserves_existing_values,
        config_persistence__save_load_round_trip,
        widget_construction__performance_panel,
        widget_construction__gpu_panel,
        widget_construction__screen_panel,
        widget_construction__keyboard_panel,
        widget_construction__battery_panel,
        widget_construction__startup_panel,
        widget_construction__footer_panel,
        widget_construction__fans_power_window,
        widget_construction__lighting_window,
        widget_construction__extra_window,
        widget_construction__full_rog_popup,
        gpu_temp__refresh_never_shows_placeholder_after_first_real_reading,
        mode_selector__hidden_on_power_shown_on_fan_curve,
        slider_debounce__pl1_rapid_drag_sends_one_command,
        rgb__rapid_multi_field_change_sends_one_command_with_final_values,
        rgb__reset_to_theme_switches_to_static_when_effect_has_no_colour,
        power_limits_persist__pl1_value_survives_window_close_and_reopen,
        power_limits_persist__apply_limits_switch_persists,
        battery_scale__range_matches_asusctl_supported_range,
        no_dynamic_docking__open_dock_left_function_removed,
        hyprland_rules__rules_file_parses_with_balanced_delimiters,
        hyprland_rules__main_window_and_companion_rules_present,
    ]
    .into_iter()
    .chain(crate::startup::ported_tests())
    .collect()
}

// ---------------------------------------------------------------------------
// Pure logic -- no GTK widgets, no hardware access.
// ---------------------------------------------------------------------------

// ports: TestSysfsHelpers.test_read_sysfs_missing_file_returns_default
fn sysfs_helpers__read_sysfs_missing_file_returns_default() {
    assert_eq!(read_sysfs(Some("/no/such/path")), None);
    assert_eq!(read_sysfs(Some("/no/such/path")).unwrap_or_else(|| "fallback".to_string()), "fallback");
}

// ports: TestSysfsHelpers.test_read_sysfs_none_path_returns_default
fn sysfs_helpers__read_sysfs_none_path_returns_default() {
    // A resolver may legitimately find no sensor and hand back no path at all.
    assert_eq!(read_sysfs(None), None);
    assert_eq!(read_sysfs(None).unwrap_or_else(|| "x".to_string()), "x");
    assert_eq!(read_sysfs_int(None), None);
}

// ports: TestSysfsHelpers.test_read_sysfs_int_non_numeric_returns_default
fn sysfs_helpers__read_sysfs_int_non_numeric_returns_default() {
    let _s = sysfs_all("unavailable");
    assert_eq!(read_sysfs_int(Some("/whatever")), None);
    assert_eq!(read_sysfs_int(Some("/whatever")).unwrap_or(-1), -1);
}

// ports: TestHwmonResolution.test_cpu_temp_path_is_coretemp_not_hardcoded
fn hwmon_resolution__cpu_temp_path_is_coretemp_not_hardcoded() {
    let Some(path) = hwmon().cpu_temp.clone() else { return }; // no coretemp device: nothing to check
    let dir = std::path::Path::new(&path).parent().expect("sensor dir").join("name");
    assert_eq!(read_sysfs(dir.to_str()).as_deref(), Some("coretemp"));
}

// ports: TestHwmonResolution.test_fan_paths_are_asus_driver
fn hwmon_resolution__fan_paths_are_asus_driver() {
    for path in [hwmon().cpu_fan.clone(), hwmon().gpu_fan.clone()].into_iter().flatten() {
        let dir = std::path::Path::new(&path).parent().expect("sensor dir").join("name");
        assert_eq!(read_sysfs(dir.to_str()).as_deref(), Some("asus"), "{path}");
    }
}

// ports: TestHwmonResolution.test_resolved_sensors_actually_readable
fn hwmon_resolution__resolved_sensors_actually_readable() {
    let hw = hwmon();
    for path in [&hw.cpu_temp, &hw.cpu_fan, &hw.gpu_fan].into_iter().flatten() {
        assert!(read_sysfs_int(Some(path)).is_some(), "{path} did not yield an int");
    }
}

// ports: TestColourHelpers.test_hex_gdk_rgba_round_trip
fn colour_helpers__hex_gdk_rgba_round_trip() {
    for hexval in ["ff0000", "00ff00", "112233", "f2bc90"] {
        assert_eq!(gdk_rgba_to_hex(&hex_to_gdk_rgba(hexval)), hexval);
    }
}

// ports: TestColourHelpers.test_theme_colours_match_live_scheme
fn colour_helpers__theme_colours_match_live_scheme() {
    let Ok(text) = std::fs::read_to_string(scheme_json_path()) else { return }; // no caelestia scheme here
    let data: serde_json::Value = serde_json::from_str(&text).expect("scheme.json parses");
    let live = |name: &str| data["colours"][name].as_str().expect("colour present").trim_start_matches('#').to_string();
    assert_eq!(theme_primary_hex(), live("primary"));
    assert_eq!(theme_secondary_hex(), live("secondary"));
}

// ports: TestAuraModeMapping.test_index_maps_are_exact_inverses
fn aura_mode_mapping__index_maps_are_exact_inverses() {
    for (name, idx) in AURA_MODE_INDEX {
        assert_eq!(aura_index_mode(idx), Some(name));
    }
    let mut indices: Vec<u32> = AURA_MODE_INDEX.iter().map(|(_, i)| *i).collect();
    indices.sort_unstable();
    indices.dedup();
    assert_eq!(indices.len(), AURA_MODE_INDEX.len(), "two effects share a mode index");
}

// ports: TestAuraModeMapping.test_every_effect_has_a_mode_index
fn aura_mode_mapping__every_effect_has_a_mode_index() {
    for effect in aura_effect_names() {
        assert!(AURA_MODE_INDEX.iter().any(|(n, _)| *n == effect), "{effect} missing from AURA_MODE_INDEX");
    }
}

fn monitor_json(entries: &[(&str, i64, i64)]) -> String {
    let list: Vec<_> = entries
        .iter()
        .map(|(name, x, y)| json!({"name": name, "width": 2560, "height": 1440, "x": x, "y": y, "scale": 1}))
        .collect();
    serde_json::to_string(&list).expect("json")
}

// ports: TestCurrentMonitorPicksInternalPanel.test_picks_edp_even_when_listed_second
fn current_monitor__picks_edp_even_when_listed_second() {
    let _c = monitors_capture(monitor_json(&[("HDMI-A-1", 1600, 0), ("eDP-1", -1, -1)]));
    assert_eq!(current_monitor_blocking().expect("a monitor")["name"], "eDP-1");
}

// ports: TestCurrentMonitorPicksInternalPanel.test_picks_edp_even_when_listed_first
fn current_monitor__picks_edp_even_when_listed_first() {
    let _c = monitors_capture(monitor_json(&[("eDP-1", 0, 0), ("HDMI-A-1", 1600, 0)]));
    assert_eq!(current_monitor_blocking().expect("a monitor")["name"], "eDP-1");
}

// ports: TestCurrentMonitorPicksInternalPanel.test_falls_back_to_first_monitor_when_no_internal_panel
fn current_monitor__falls_back_to_first_monitor_when_no_internal_panel() {
    // A docked / external-only setup (no eDP at all) must not give up.
    let _c = monitors_capture(monitor_json(&[("DP-1", 0, 0)]));
    assert_eq!(current_monitor_blocking().expect("a monitor")["name"], "DP-1");
}

/// Runs `apply_screen_preset(preset)` against a fake eDP-1 at (x, y) and returns the commands it issued.
fn screen_preset_commands(preset: &str, x: i64, y: i64) -> Calls {
    let _c = monitors_capture(monitor_json(&[("eDP-1", x, y)]));
    let _s = hook_sysfs(|_| None, |_| Some(vec![])); // no overdrive attribute: only the hyprctl call
    let (calls, _g) = record_run_cmd();
    apply_screen_preset(preset, None);
    pump(300);
    calls
}

// ports: TestApplyScreenPresetUsesEval.test_uses_hyprctl_eval_not_keyword
fn screen_preset__uses_hyprctl_eval_not_keyword() {
    let calls = screen_preset_commands("60", -1, -1);
    let calls = calls.borrow();
    assert_eq!(calls.len(), 1, "{calls:?}");
    let cmd = &calls[0];
    assert_eq!(cmd[0], "hyprctl");
    assert_eq!(cmd[1], "eval");
    assert!(cmd[2].contains("hl.monitor("), "{}", cmd[2]);
    assert!(!cmd.iter().any(|a| a == "keyword"));
}

// ports: TestApplyScreenPresetUsesEval.test_preserves_position_and_output_name
fn screen_preset__preserves_position_and_output_name() {
    let calls = screen_preset_commands("max", -1, -1);
    let lua = calls.borrow()[0][2].clone();
    assert!(lua.contains("output = \"eDP-1\""), "{lua}");
    assert!(lua.contains("position = \"-1x-1\""), "{lua}");
}

fn cfg_with_optimized(on: bool) -> config::Cfg {
    config::new_cfg(json!({"automation": {"gpu_optimized": on}}))
}

// ports: TestGpuModeLogic.test_current_gpu_mode_ultimate_when_mux_zero
fn gpu_mode__current_gpu_mode_ultimate_when_mux_zero() {
    let _s = sysfs_all("0");
    assert_eq!(current_gpu_mode(&cfg_with_optimized(false)), "Ultimate");
}

// ports: TestGpuModeLogic.test_current_gpu_mode_optimized_when_flag_set_and_mux_one
fn gpu_mode__current_gpu_mode_optimized_when_flag_set_and_mux_one() {
    let _s = hook_sysfs(|p| Some(if p == GPU_MUX_PATH { "1" } else { "0" }.to_string()), |_| Some(vec![]));
    assert_eq!(current_gpu_mode(&cfg_with_optimized(true)), "Optimized");
}

// ports: TestGpuModeLogic.test_current_gpu_mode_eco_vs_standard_by_dgpu_disable
fn gpu_mode__current_gpu_mode_eco_vs_standard_by_dgpu_disable() {
    let reader = |dgpu_disable: &'static str| {
        hook_sysfs(
            move |p| match p {
                GPU_MUX_PATH => Some("1".to_string()),
                DGPU_DISABLE_PATH => Some(dgpu_disable.to_string()),
                _ => None,
            },
            |_| Some(vec![]),
        )
    };
    {
        let _s = reader("1");
        assert_eq!(current_gpu_mode(&cfg_with_optimized(false)), "Eco");
    }
    let _s = reader("0");
    assert_eq!(current_gpu_mode(&cfg_with_optimized(false)), "Standard");
}

// ports: TestGpuModeLogic.test_gpu_mode_needs_reboot_only_for_mux_change
fn gpu_mode__needs_reboot_only_for_mux_change() {
    {
        let _s = sysfs_all("0"); // currently Ultimate
        assert!(!gpu_mode_needs_reboot("Ultimate"));
        for mode in ["Standard", "Eco", "Optimized"] {
            assert!(gpu_mode_needs_reboot(mode), "{mode}");
        }
    }
    let _s = sysfs_all("1"); // currently hybrid
    assert!(gpu_mode_needs_reboot("Ultimate"));
    for mode in ["Standard", "Eco", "Optimized"] {
        assert!(!gpu_mode_needs_reboot(mode), "{mode}");
    }
}

// ports: TestGpuModeLogic.test_apply_gpu_mode_never_touches_mux_when_already_correct
fn gpu_mode__apply_never_touches_mux_when_already_correct() {
    // If the MUX already matches, apply_gpu_mode must not change it: toggling among
    // Eco/Standard/Optimized while hybrid must never trigger a reboot.
    let _s = sysfs_all("1");
    let (calls, _g) = record_run_cmd();
    apply_gpu_mode("Standard", false, None);
    assert!(cmds_with(&calls, "gpu_mux_mode").is_empty(), "apply_gpu_mode changed the MUX when it was already correct");
}

// ports: TestGpuModeLogic.test_apply_gpu_mode_never_touches_dgpu_disable_in_same_call_as_a_mux_change
fn gpu_mode__apply_never_touches_dgpu_disable_in_same_call_as_a_mux_change() {
    // Safety-critical: a MUX change only takes effect after a reboot, so disabling the dGPU in the
    // same call could blank a display the dGPU still drives.
    let _s = sysfs_all("0"); // currently Ultimate: a mismatch is guaranteed
    let (calls, _g) = record_run_cmd();
    apply_gpu_mode("Optimized", true, None);
    assert_eq!(calls.borrow().len(), 1, "expected only the MUX-change call, got {:?}", calls.borrow());
    assert!(calls.borrow()[0].iter().any(|a| a == "gpu_mux_mode"));
    assert!(cmds_with(&calls, "dgpu_disable").is_empty(), "dgpu_disable touched in the same call as an unconfirmed MUX change");
}

/// A fake sysfs where the MUX is hybrid and an NVIDIA card drives `connector`, `status`.
fn drm_sysfs(vendor: &'static str, card: &'static str, connector: &'static str, status: &'static str) -> SysfsGuard {
    hook_sysfs(
        move |p| {
            if p == GPU_MUX_PATH {
                Some("1".to_string())
            } else if p.ends_with("/device/vendor") {
                Some(vendor.to_string())
            } else if p.ends_with("/status") {
                Some(status.to_string())
            } else {
                None
            }
        },
        move |dir| (dir == "/sys/class/drm").then(|| vec![card.to_string(), format!("{card}-{connector}")]),
    )
}

// ports: TestGpuModeLogic.test_refuses_to_disable_dgpu_when_external_display_attached
fn gpu_mode__refuses_to_disable_dgpu_when_external_display_attached() {
    for (mode, on_battery) in [("Eco", false), ("Optimized", true)] {
        let _s = drm_sysfs("0x10de", "card2", "HDMI-A-1", "connected");
        let (calls, _g) = record_run_cmd();
        let result: Rc<RefCell<Vec<(bool, String)>>> = Rc::default();
        let sink = result.clone();
        apply_gpu_mode(mode, on_battery, Some(Box::new(move |ok, err| sink.borrow_mut().push((ok, err)))));
        assert!(calls.borrow().is_empty(), "{mode}: ran {:?}", calls.borrow());
        let result = result.borrow();
        assert_eq!(result.len(), 1, "{mode}");
        assert!(!result[0].0, "{mode}");
        assert!(result[0].1.to_lowercase().contains("external display"), "{mode}: {}", result[0].1);
    }
}

// ports: TestGpuModeLogic.test_proceeds_normally_when_no_external_display_on_dgpu
fn gpu_mode__proceeds_normally_when_no_external_display_on_dgpu() {
    let _s = hook_sysfs(|p| (p == GPU_MUX_PATH).then(|| "1".to_string()), |_| Some(vec![]));
    let (calls, _g) = record_run_cmd();
    apply_gpu_mode("Eco", false, None);
    assert!(!cmds_with(&calls, "dgpu_disable").is_empty());
}

// ports: TestExternalDisplayOnDgpu.test_matches_real_hardware_state
fn external_display__matches_real_hardware_state() {
    // Whether a monitor is plugged in varies run to run, so compare against an independent walk of
    // the real /sys/class/drm: the function must agree with it, not just return a bool.
    let mut expected = false;
    for entry in std::fs::read_dir("/sys/class/drm").into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some((card, connector)) = name.split_once('-') else { continue };
        if !card.starts_with("card") || connector.starts_with("eDP") {
            continue;
        }
        let read = |p: String| std::fs::read_to_string(p).map(|s| s.trim().to_string()).unwrap_or_default();
        if read(format!("/sys/class/drm/{card}/device/vendor")) == "0x10de" && read(format!("/sys/class/drm/{name}/status")) == "connected" {
            expected = true;
        }
    }
    assert_eq!(external_display_on_dgpu(), expected);
}

// ports: TestExternalDisplayOnDgpu.test_internal_panel_alone_is_not_treated_as_external
fn external_display__internal_panel_alone_is_not_treated_as_external() {
    let _s = drm_sysfs("0x10de", "card0", "eDP-1", "connected");
    assert!(!external_display_on_dgpu());
}

// ports: TestExternalDisplayOnDgpu.test_non_edp_connected_on_nvidia_card_is_external
fn external_display__non_edp_connected_on_nvidia_card_is_external() {
    let _s = drm_sysfs("0x10de", "card2", "HDMI-A-1", "connected");
    assert!(external_display_on_dgpu());
}

// ports: TestExternalDisplayOnDgpu.test_ignores_non_nvidia_cards
fn external_display__ignores_non_nvidia_cards() {
    let _s = drm_sysfs("0x8086", "card1", "HDMI-A-1", "connected"); // Intel iGPU
    assert!(!external_display_on_dgpu());
}

// ports: TestProfilePowerPlan.test_every_profile_has_a_power_plan
fn profile_power_plan__every_profile_has_a_power_plan() {
    for p in PROFILES {
        assert!(profile_power_plan(p).is_some(), "{p}");
    }
}

// ports: TestProfilePowerPlan.test_apply_profile_pairs_power_profile
fn profile_power_plan__apply_profile_pairs_power_profile() {
    rog_control::sys::override_tool("powerprofilesctl", true);
    let (calls, _g) = record_run_cmd();
    apply_profile("Quiet", None);
    rog_control::sys::clear_tool_overrides();
    let calls = calls.borrow();
    assert!(calls.contains(&["asusctl", "profile", "set", "Quiet"].map(String::from).to_vec()), "{calls:?}");
    // Tools are invoked by bare name through PATH, never by absolute path.
    assert!(calls.contains(&["powerprofilesctl", "set", "power-saver"].map(String::from).to_vec()), "{calls:?}");
}

// ports: TestDebouncer.test_rapid_triggers_coalesce_to_one_call
fn debouncer__rapid_triggers_coalesce_to_one_call() {
    let count = Rc::new(RefCell::new(0));
    let c = count.clone();
    let d = Debouncer::with_callback(50, move || *c.borrow_mut() += 1);
    for _ in 0..20 {
        d.trigger();
    }
    pump(300);
    assert_eq!(*count.borrow(), 1);
}

// ports: TestDebouncer.test_separated_triggers_each_fire
fn debouncer__separated_triggers_each_fire() {
    let count = Rc::new(RefCell::new(0));
    let c = count.clone();
    let d = Debouncer::with_callback(30, move || *c.borrow_mut() += 1);
    d.trigger();
    pump(150);
    d.trigger();
    pump(150);
    assert_eq!(*count.borrow(), 2);
}

// ports: TestNvidiaSmiExitCodeLie.test_not_supported_text_treated_as_failure_despite_exit_zero
fn nvidia_smi__not_supported_text_treated_as_failure_despite_exit_zero() {
    // `nvidia-smi -pl` exits 0 even when it explicitly refuses the change.
    let _c = fake_capture(|argv| {
        is(argv, &["nvidia-smi", "-pl", "150"])
            .then(|| ok_out("Changing power management limit is not supported in current scope for GPU: 0000\nAll done.\n"))
    });
    let (ok, err) = nvidia_smi_pl(150.0, Duration::from_secs(5));
    assert!(!ok);
    assert!(err.contains("not supported"), "{err}");
}

// ports: TestNvidiaSmiExitCodeLie.test_real_success_with_exit_zero_and_no_warning
fn nvidia_smi__real_success_with_exit_zero_and_no_warning() {
    let _c = fake_capture(|argv| is(argv, &["nvidia-smi", "-pl", "150"]).then(|| ok_out("150\n")));
    let (ok, _err) = nvidia_smi_pl(150.0, Duration::from_secs(5));
    assert!(ok);
}

// ports: TestFanCurveGraphMonotonicSpeed.test_drag_cannot_produce_non_monotonic_speed
fn fan_curve_graph__drag_cannot_produce_non_monotonic_speed() {
    // asusctl rejects a curve whose speed is not non-decreasing with temperature.
    let graph = FanCurveGraph::new(None, None);
    graph.set_points(&DEFAULT_CURVE);
    // Drag point 2 (55, 40) to 95 %: far above its neighbours (30, 50).
    graph.drag_to(2, 55.0, 95.0);
    let points = graph.points();
    assert!(points[2].1 <= points[3].1, "speed allowed above the next point's: {points:?}");
    assert!(points[2].1 >= points[1].1, "speed allowed below the previous point's: {points:?}");
}

// ports: TestFanCurveGraphMonotonicSpeed.test_resulting_curve_is_accepted_by_the_real_monotonic_check
fn fan_curve_graph__resulting_curve_is_accepted_by_the_real_monotonic_check() {
    let graph = FanCurveGraph::new(None, None);
    graph.set_points(&DEFAULT_CURVE);
    for (i, (temp, _)) in DEFAULT_CURVE.iter().enumerate() {
        graph.drag_to(i, f64::from(*temp), 999.0);
    }
    let speeds: Vec<i32> = graph.points().iter().map(|p| p.1).collect();
    let mut sorted = speeds.clone();
    sorted.sort_unstable();
    assert_eq!(speeds, sorted, "dragging every point to the max left a non-monotonic curve");
}

// ports: TestFactoryDefaultsOnlyResetsActiveProfile.test_refuses_when_edited_profile_is_not_active
fn factory_defaults__refuses_when_edited_profile_is_not_active() {
    // `--default` only resets the ACTIVE profile; sending it for another would reset the wrong one.
    let (toasts, toast) = toast_sink();
    let editor = FanCurveEditor::new("cpu", toast, Rc::new(|| "Quiet".to_string()));
    let _s = hook_sysfs(|_| Some("performance".to_string()), |_| Some(vec![]));
    let (calls, _g) = record_run_cmd();
    editor.reset();
    assert!(calls.borrow().is_empty(), "{:?}", calls.borrow());
    let toasts = toasts.borrow();
    assert!(toasts.first().is_some_and(|t| t.contains("Quiet") && t.contains("Performance")), "{toasts:?}");
}

// ports: TestFactoryDefaultsOnlyResetsActiveProfile.test_proceeds_when_edited_profile_is_active
fn factory_defaults__proceeds_when_edited_profile_is_active() {
    let editor = FanCurveEditor::new("cpu", no_toast(), Rc::new(|| "Performance".to_string()));
    let _s = hook_sysfs(|_| Some("performance".to_string()), |_| Some(vec![]));
    let (calls, _g) = record_run_cmd();
    editor.reset();
    assert_eq!(calls.borrow().len(), 1);
    assert!(calls.borrow()[0].iter().any(|a| a == "--default"));
}

const SAMPLE_CURVES: &str = "
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
";

fn pct(pwm: f64) -> i32 {
    py_round(pwm / 255.0 * 100.0) as i32
}

// ports: TestFanCurveParsing.test_parses_cpu_curve
fn fan_curve_parsing__parses_cpu_curve() {
    let (points, enabled) = parse_curve(SAMPLE_CURVES, "cpu");
    assert!(enabled);
    assert_eq!(points[0], (40, pct(20.0)));
    assert_eq!(points[points.len() - 1], (97, 100));
    assert_eq!(points.len(), 8);
}

// ports: TestFanCurveParsing.test_parses_gpu_curve_independently
fn fan_curve_parsing__parses_gpu_curve_independently() {
    let (points, enabled) = parse_curve(SAMPLE_CURVES, "gpu");
    assert!(!enabled);
    assert_eq!(points[0], (47, pct(36.0)));
    assert_ne!(points, parse_curve(SAMPLE_CURVES, "cpu").0);
}

// ports: TestFanCurveParsing.test_missing_fan_falls_back_to_default
fn fan_curve_parsing__missing_fan_falls_back_to_default() {
    let (points, enabled) = parse_curve("garbage, no data here", "cpu");
    assert!(!enabled);
    assert_eq!(points.len(), 8);
    assert_eq!(points, DEFAULT_CURVE.to_vec());
}

// ports: TestConfigPersistence.test_defaults_present_after_load_with_empty_file
fn config_persistence__defaults_present_after_load_with_empty_file() {
    let cfg = config::merge_defaults(json!({}), &config::defaults());
    for key in ["gpu_optimized", "screen_auto", "profile_battery", "profile_ac", "kbd_battery", "kbd_ac"] {
        assert!(cfg["automation"].get(key).is_some(), "{key}");
    }
}

// ports: TestConfigPersistence.test_merge_preserves_existing_values
fn config_persistence__merge_preserves_existing_values() {
    let merged = config::merge_defaults(json!({"automation": {"gpu_optimized": true}}), &config::defaults());
    assert_eq!(merged["automation"]["gpu_optimized"], json!(true));
    assert!(merged["automation"].get("screen_auto").is_some(), "not backfilled");
}

// ports: TestConfigPersistence.test_save_load_round_trip
fn config_persistence__save_load_round_trip() {
    let tmp = TempDir::new("config");
    let path = tmp.0.join("config.json");
    let mut cfg = config::load_from(&path);
    cfg["automation"]["profile_ac"] = json!("Performance");
    config::save_to(&path, &cfg).expect("save");
    assert_eq!(config::load_from(&path)["automation"]["profile_ac"], json!("Performance"));
}

// ---------------------------------------------------------------------------
// Widget construction -- every panel / window must build, and have what the reference has.
// ---------------------------------------------------------------------------

fn button_texts(buttons: &[(&str, gtk::ToggleButton)]) -> Vec<String> {
    buttons.iter().map(|(name, _)| name.to_string()).collect()
}

// ports: TestWidgetConstruction.test_performance_panel
fn widget_construction__performance_panel() {
    let panel = PerformancePanel::new(no_toast(), make_cfg(), no_save(), on_ac(), Rc::new(|| {}));
    assert_eq!(button_texts(&panel.buttons), ["Quiet", "Balanced", "Performance"]);
    assert!(panel.header.stat.label().starts_with("CPU: "), "{}", panel.header.stat.label());
}

// ports: TestWidgetConstruction.test_gpu_panel
fn widget_construction__gpu_panel() {
    let panel = GpuPanel::new(no_toast(), make_cfg(), no_save(), on_ac(), Rc::new(|| None));
    assert_eq!(button_texts(&panel.buttons), ["Eco", "Standard", "Ultimate", "Optimized"]);
    assert!(panel.header.stat.label().starts_with("GPU: "), "{}", panel.header.stat.label());
}

// ports: TestWidgetConstruction.test_screen_panel
fn widget_construction__screen_panel() {
    let panel = ScreenPanel::new(no_toast(), make_cfg(), no_save());
    let keys: Vec<&str> = panel.buttons.iter().map(|(k, _)| *k).collect();
    assert_eq!(keys, ["auto", "60", "max"]);
    pump(300);
}

// ports: TestWidgetConstruction.test_keyboard_panel
fn widget_construction__keyboard_panel() {
    let panel = KeyboardPanel::new(no_toast(), make_cfg(), no_save(), on_ac(), Rc::new(|| {}));
    assert_eq!(button_texts(&panel.buttons), ["off", "low", "med", "high"]);
    assert_eq!(panel.effect_dropdown.model().expect("effect model").n_items() as usize, aura_effect_names().len());
}

// ports: TestWidgetConstruction.test_battery_panel
fn widget_construction__battery_panel() {
    let panel = BatteryPanel::new(no_toast());
    assert!(panel.header.title.label().starts_with("Battery Charge Limit: "), "{}", panel.header.title.label());
    assert_eq!(panel.oneshot_button.label().as_deref(), Some("100%"));
}

// ports: TestWidgetConstruction.test_startup_panel
fn widget_construction__startup_panel() {
    let panel = StartupPanel::new(no_toast());
    pump(300);
    assert!(panel.root.first_child().is_some());
}

// ports: TestWidgetConstruction.test_footer_panel
fn widget_construction__footer_panel() {
    let footer = FooterPanel::new(Rc::new(|| {}), Rc::new(|| {}));
    assert_eq!(footer.firmware_button.label().as_deref(), Some("Firmware"));
    assert_eq!(footer.quit_button.label().as_deref(), Some("Quit"));
}

// ports: TestWidgetConstruction.test_fans_power_window
fn widget_construction__fans_power_window() {
    let win = FansPowerWindow::new(None, no_toast(), make_cfg(), no_save());
    pump(400); // let the async nvidia probes and the power-mode fetch settle
    assert_eq!(win.window.title().as_deref(), Some("Fans and Power"));
}

// ports: TestWidgetConstruction.test_lighting_window
fn widget_construction__lighting_window() {
    let win = LightingWindow::new(None, no_toast(), None);
    assert_eq!(win.window.title().as_deref(), Some("Keyboard Lighting"));
}

// ports: TestWidgetConstruction.test_extra_window
fn widget_construction__extra_window() {
    let win = ExtraWindow::new(None, no_toast());
    assert_eq!(win.window.title().as_deref(), Some("Extra"));
}

// ports: TestWidgetConstruction.test_full_rog_popup
fn widget_construction__full_rog_popup() {
    // The real app only builds windows after startup; register an application to match that.
    let app = adw::Application::builder().application_id("com.test.ConstructionCheck").build();
    let registered = app.register(gio::Cancellable::NONE).is_ok();
    let popup = RogPopup::new(registered.then_some(&app), make_cfg(), no_save(), on_ac(), Rc::new(|| {}));
    assert_eq!(popup.window.title().as_deref(), Some("ROG Control"));
    assert!(!popup.window.is_resizable());
    assert!(!popup.is_visible());
}

// ---------------------------------------------------------------------------
// Regression tests for specific confirmed-and-fixed bugs, using the real widgets.
// ---------------------------------------------------------------------------

// ports: TestGpuTempNoFlicker.test_refresh_never_shows_placeholder_after_first_real_reading
fn gpu_temp__refresh_never_shows_placeholder_after_first_real_reading() {
    // refresh() must never blank the stat label to a placeholder before the async nvidia-smi
    // fetch lands. A slow fake nvidia-smi keeps that window open long enough to look inside it.
    let _c = fake_capture(|argv| {
        argv.get(1).is_some_and(|a| a.starts_with("--query-gpu=temperature")).then(|| {
            std::thread::sleep(Duration::from_millis(120));
            ok_out("55\n")
        })
    });
    let panel = GpuPanel::new(no_toast(), make_cfg(), no_save(), on_ac(), Rc::new(|| None));
    pump(500); // let the first real reading land
    assert!(panel.header.stat.label().contains("GPU: 55°C"), "{}", panel.header.stat.label());
    for _ in 0..6 {
        panel.refresh();
        let label = panel.header.stat.label();
        assert!(!label.contains("GPU: --  Fan:"), "stat label blanked during refresh: {label}");
        pump(200);
    }
    assert!(panel.header.stat.label().contains("GPU: 55°C"));
}

// ports: TestModeSelectorVisibility.test_hidden_on_power_shown_on_fan_curve
fn mode_selector__hidden_on_power_shown_on_fan_curve() {
    // The fan-curve profile selector must only exist inside whichever fan sub-page is visible.
    let win = FansPowerWindow::new(None, no_toast(), make_cfg(), no_save());
    pump(50);
    assert!(!win.mode_row.get_visible());

    win.cpu_fan_btn.set_active(true);
    assert!(win.mode_row.get_visible());
    assert_eq!(win.mode_row.parent().as_ref(), Some(win.cpu_fan_page.upcast_ref::<gtk::Widget>()));

    win.gpu_nav.set_active(true);
    assert!(!win.mode_row.get_visible(), "GPU tab defaults to Power view, the selector should hide");

    win.gpu_fan_btn.set_active(true);
    assert!(win.mode_row.get_visible());
    assert_eq!(win.mode_row.parent().as_ref(), Some(win.gpu_fan_page.upcast_ref::<gtk::Widget>()));
}

// ports: TestSliderDebounce.test_pl1_rapid_drag_sends_one_command
fn slider_debounce__pl1_rapid_drag_sends_one_command() {
    // A rapid drag (many value-changed ticks) must collapse to exactly one applied command.
    let (calls, _g) = record_run_cmd();
    let win = FansPowerWindow::new(None, no_toast(), make_cfg(), no_save());
    pump(50);
    for v in 10..40 {
        win.pl1.scale.set_value(f64::from(v));
    }
    pump(400);
    let pl1 = cmds_with(&calls, "ppt_pl1_spl");
    assert_eq!(pl1.len(), 1, "expected 1 debounced call, got {}", pl1.len());
    assert_eq!(pl1[0].last().map(String::as_str), Some("39"));
}

fn effect_index(name: &str) -> u32 {
    aura_effect_names().iter().position(|e| *e == name).expect("known effect") as u32
}

fn aura_calls(calls: &Calls) -> Vec<Vec<String>> {
    calls.borrow().iter().filter(|c| c.starts_with(&["asusctl".into(), "aura".into(), "effect".into()])).cloned().collect()
}

// ports: TestRgbDebounceAndPreservation.test_rapid_multi_field_change_sends_one_command_with_final_values
fn rgb__rapid_multi_field_change_sends_one_command_with_final_values() {
    let (calls, _g) = record_run_cmd();
    let win = LightingWindow::new(None, no_toast(), None);
    pump(50);
    win.effect_dropdown.set_selected(effect_index("breathe"));
    win.colour1_btn.set_rgba(&hex_to_gdk_rgba("112233"));
    win.colour2_btn.set_rgba(&hex_to_gdk_rgba("445566"));
    win.speed_dropdown.set_selected(2);
    pump(400);
    let effect_calls = aura_calls(&calls);
    assert_eq!(effect_calls.len(), 1, "expected 1 debounced apply, got {effect_calls:?}");
    let cmd = &effect_calls[0];
    for needle in ["112233", "445566", "high"] {
        assert!(cmd.iter().any(|a| a == needle), "{needle} missing from {cmd:?}");
    }
}

// ports: TestRgbDebounceAndPreservation.test_reset_to_theme_switches_to_static_when_effect_has_no_colour
fn rgb__reset_to_theme_switches_to_static_when_effect_has_no_colour() {
    let (calls, _g) = record_run_cmd();
    let win = LightingWindow::new(None, no_toast(), None);
    pump(50);
    win.effect_dropdown.set_selected(effect_index("rainbow-cycle"));
    pump(50);
    win.reset_to_theme();
    pump(400);
    let effect_calls = aura_calls(&calls);
    let last = effect_calls.last().expect("reset-to-theme sent no command at all");
    assert_eq!(last[3], "static", "should have switched to an effect that uses colour");
    assert!(last.iter().any(|a| *a == theme_primary_hex()), "{last:?}");
}

// ports: TestPowerLimitsPersistAcrossReopen.test_pl1_value_survives_window_close_and_reopen
fn power_limits_persist__pl1_value_survives_window_close_and_reopen() {
    // The window is destroyed and rebuilt on every close/reopen; without saving the last value to
    // config the sliders would always reset.
    let cfg = make_cfg();
    let (_calls, _g) = record_run_cmd();
    let win1 = FansPowerWindow::new(None, no_toast(), cfg.clone(), no_save());
    pump(50);
    win1.pl1.scale.set_value(77.0);
    pump(300); // let the debounced apply (which persists) fire
    assert_eq!(cfg.borrow()["power_limits"]["pl1"], json!(77));

    // Close + reopen: a brand-new instance over the same config.
    let win2 = FansPowerWindow::new(None, no_toast(), cfg, no_save());
    pump(50);
    assert_eq!(win2.pl1.scale.value(), 77.0, "PL1 reset instead of restoring the last-set value");
}

// ports: TestPowerLimitsPersistAcrossReopen.test_apply_limits_switch_persists
fn power_limits_persist__apply_limits_switch_persists() {
    let cfg = make_cfg();
    let (_calls, _g) = record_run_cmd();
    let win1 = FansPowerWindow::new(None, no_toast(), cfg.clone(), no_save());
    pump(50);
    win1.apply_limits_switch.set_active(true);
    assert_eq!(cfg.borrow()["power_limits"]["apply_limits"], json!(true));

    let win2 = FansPowerWindow::new(None, no_toast(), cfg, no_save());
    pump(50);
    assert!(win2.apply_limits_switch.is_active());
}

// ports: TestBatteryScaleRange.test_range_matches_asusctl_supported_range
fn battery_scale__range_matches_asusctl_supported_range() {
    let panel = BatteryPanel::new(no_toast());
    let adj = panel.scale.adjustment();
    assert_eq!(adj.lower(), 20.0);
    assert_eq!(adj.upper(), 100.0);
}

// ports: TestNoDynamicDockingLeftover.test_open_dock_left_function_removed
fn no_dynamic_docking__open_dock_left_function_removed() {
    // The unreliable runtime docking approach was replaced by a static Hyprland window rule; it
    // must not silently come back.
    let needle = ["movewindow", "pixel"].concat();
    let mut stack = vec![std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))];
    let mut files = 0;
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read src").flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                files += 1;
                let text = std::fs::read_to_string(&path).expect("read source");
                assert!(!text.contains(&needle), "{} brings back dynamic docking", path.display());
            }
        }
    }
    assert!(files > 5, "scanned only {files} source files");
}

fn rules_text() -> Option<String> {
    std::fs::read_to_string(rog_control::sys::home_join(".config/hypr/custom/rules.lua")).ok()
}

// ports: TestHyprlandRulesSyntax.test_rules_file_parses_with_balanced_delimiters
fn hyprland_rules__rules_file_parses_with_balanced_delimiters() {
    let Some(text) = rules_text() else { return }; // rules.lua not present on this machine
    for (open, close) in [('(', ')'), ('{', '}')] {
        assert_eq!(text.matches(open).count(), text.matches(close).count(), "unbalanced {open}{close} in rules.lua");
    }
}

// ports: TestHyprlandRulesSyntax.test_main_window_and_companion_rules_present
fn hyprland_rules__main_window_and_companion_rules_present() {
    let Some(text) = rules_text() else { return };
    assert!(text.contains("title = \"^ROG Control$\""));
    assert!(text.contains("Fans and Power"));
}
