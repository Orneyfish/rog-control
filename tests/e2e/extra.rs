#![allow(non_snake_case)]
//! Guards for rules the Python suite never tested (it only constructed most widgets). Each one
//! pins behaviour the port must keep: the safety rules around the GPU MUX, the commands each
//! control sends, error reporting, profile cycling direction, config safety. These are
//! deliberately not `ports:` entries.

use std::cell::RefCell;
use std::io;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use adw::prelude::*;
use rog_control::automation::AutomationManager;
use rog_control::config;
use rog_control::control::{dispatch, method_for_args, ControlHandlers};
use rog_control::tray::TrayHandlers;
use rog_control::extra::ExtraWindow;
use rog_control::fans::{FanCurveEditor, FansPowerWindow, DEFAULT_CURVE};
use rog_control::hw::*;
use rog_control::lighting::LightingWindow;
use rog_control::panels::*;
use rog_control::sys::{clear_tool_overrides, hook_sysfs, override_tool, push_capture_hook, run_cmd, CmdOut, SysfsGuard};
use rog_control::theme::{hex_to_gdk_rgba, theme_primary_hex};
use rog_control::widgets::ConfirmFn;
use serde_json::json;

use crate::common::*;

pub fn tests() -> Vec<(&'static str, fn())> {
    macro_rules! t {
        ($($name:ident),* $(,)?) => { vec![$((stringify!($name), $name as fn())),*] };
    }
    t![
        run_cmd__reports_failure_text_and_success,
        run_cmd__reports_spawn_and_timeout_errors,
        gpu_panel__mux_change_waits_for_confirmation_then_reboots_after_success,
        gpu_panel__declined_confirmation_changes_nothing,
        gpu_panel__real_dialog_blocks_until_answered,
        widgets__confirm_dialog_runs_only_on_ok,
        gpu_panel__failed_mux_write_toasts_and_never_reboots,
        gpu_panel__switch_without_mux_change_applies_at_once_and_never_reboots,
        profile_cycle__direction_and_wrap,
        control__dispatch_maps_each_method_and_rejects_bad_input,
        control__flags_select_the_method,
        control__cycle_methods_step_forward_and_back,
        tray__scroll_always_cycles_forward,
        config__save_is_atomic_and_keeps_unknown_keys,
        config__corrupt_missing_and_misshapen_files_give_defaults,
        config__first_save_keeps_a_copy_of_an_unusable_file,
        install__refuses_a_prefix_that_would_corrupt_the_unit,
        startup_panel__switch_runs_enable_and_disable,
        keyboard_panel__level_buttons_send_leds_set_and_are_remembered_per_power_source,
        performance_panel__profile_buttons_apply_pair_and_remember,
        battery_panel__limit_is_debounced_and_oneshot_command,
        battery_panel__refresh_mid_debounce_keeps_the_users_limit,
        keyboard_panel__refresh_mid_debounce_keeps_the_users_edit,
        lighting_window__refresh_mid_debounce_keeps_the_users_edit,
        fans_window__refresh_mid_debounce_keeps_every_slider_value,
        screen_panel__presets_apply_and_auto_only_remembers,
        extra_window__controls_send_armoury_commands_and_failures_toast,
        fans_window__failed_write_toasts_and_power_mode_pairs_with_ppd,
        fan_curve_editor__apply_and_enable_commands,
        nvidia__power_limit_parsing_and_clock_commands,
        automation__applies_remembered_state_per_power_source,
        automation__gpu_optimized_guards,
        tray__properties_match_the_wire_contract,
        aura__led_mode_data_and_effect_commands,
        helpers__python_compatible_rounding_and_formatting,
        battery_text__charging_and_health,
        theme__filters_only_known_environment_noise,
    ]
}

/// A capture hook that records every non-read command and answers it with `reply`.
fn recording_capture(
    reply: impl Fn(&[String]) -> CmdOut + Send + Sync + 'static,
) -> (Arc<Mutex<Vec<String>>>, rog_control::sys::CaptureGuard) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let guard = fake_capture(move |argv| {
        if is_read_only(argv) {
            None
        } else {
            sink.lock().expect("lock").push(argv.join(" "));
            Some(reply(argv))
        }
    });
    (seen, guard)
}

fn seen(log: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    log.lock().expect("lock").clone()
}

fn sysfs_map(pairs: &'static [(&'static str, &'static str)]) -> SysfsGuard {
    hook_sysfs(
        move |p| pairs.iter().find(|(k, _)| *k == p).map(|(_, v)| (*v).to_string()),
        |_| Some(vec![]),
    )
}

// guards: run_cmd hands the tool's stderr to the completion callback and signals success
fn run_cmd__reports_failure_text_and_success() {
    let _c = fake_capture(|argv| match argv.get(1).map(String::as_str) {
        Some("bad") => Some(failed_out("boom")),
        Some("good") => Some(ok_out("")),
        _ => None,
    });
    let results: Rc<RefCell<Vec<(bool, String)>>> = Rc::default();
    for arg in ["bad", "good"] {
        let sink = results.clone();
        run_cmd(&["asusctl", arg], Some(Box::new(move |ok, err| sink.borrow_mut().push((ok, err)))));
    }
    pump(200);
    let mut r = results.borrow().clone();
    r.sort();
    assert_eq!(r, [(false, "boom".to_string()), (true, String::new())]);
}

// guards: a tool that cannot be started or times out is reported, not swallowed
fn run_cmd__reports_spawn_and_timeout_errors() {
    let _c = push_capture_hook(|argv| {
        (argv.get(1).map(String::as_str) == Some("timeout")).then(|| Err(io::Error::new(io::ErrorKind::TimedOut, "timed out")))
    });
    let result: Rc<RefCell<Option<(bool, String)>>> = Rc::default();
    let sink = result.clone();
    run_cmd(&["asusctl", "timeout"], Some(Box::new(move |ok, err| *sink.borrow_mut() = Some((ok, err)))));
    pump(200);
    assert_eq!(result.borrow().clone(), Some((false, "timed out".to_string())));
}

/// One recorded confirmation: heading, body, OK label and the not-yet-run `on_confirm`.
type Confirm = (String, String, String, Option<Box<dyn FnOnce()>>);

/// A GPU panel over a fake sysfs, with a recording confirmation instead of a real dialog.
struct GpuRig {
    panel: Rc<GpuPanel>,
    confirms: Rc<RefCell<Vec<Confirm>>>,
    toasts: Toasts,
    cfg: config::Cfg,
}

fn gpu_rig() -> GpuRig {
    let (toasts, toast) = toast_sink();
    let cfg = make_cfg();
    let panel = GpuPanel::new(toast, cfg.clone(), no_save(), on_ac(), Rc::new(|| None));
    let confirms: Rc<RefCell<Vec<Confirm>>> = Rc::default();
    let sink = confirms.clone();
    let confirm: ConfirmFn = Rc::new(move |heading, body, ok, on_confirm| {
        sink.borrow_mut().push((heading.to_string(), body.to_string(), ok.to_string(), Some(on_confirm)));
    });
    panel.set_confirm(confirm);
    GpuRig { panel, confirms, toasts, cfg }
}

fn click(panel: &GpuPanel, mode: &str) {
    panel.buttons.iter().find(|(n, _)| *n == mode).expect("mode button").1.set_active(true);
}

// guards: FR-17 - no MUX write and no reboot before the user confirms; reboot only after success
fn gpu_panel__mux_change_waits_for_confirmation_then_reboots_after_success() {
    let _s = sysfs_map(&[(GPU_MUX_PATH, "0"), (DGPU_DISABLE_PATH, "0")]); // Ultimate right now
    let (log, _c) = recording_capture(|_| ok_out(""));
    let rig = gpu_rig();
    click(&rig.panel, "Standard"); // leaving Ultimate needs a MUX change
    pump(300);
    assert!(seen(&log).is_empty(), "something ran before the user confirmed: {:?}", seen(&log));
    assert_eq!(rig.confirms.borrow().len(), 1);
    {
        let confirms = rig.confirms.borrow();
        assert_eq!(confirms[0].0, "Reboot now?");
        assert_eq!(confirms[0].1, "Switching off Ultimate Mode requires restart");
        assert_eq!(confirms[0].2, "Reboot");
    }
    let on_confirm = rig.confirms.borrow_mut()[0].3.take().expect("confirm callback");
    on_confirm();
    pump(400);
    let log = seen(&log);
    assert_eq!(log.first().map(String::as_str), Some("asusctl armoury set gpu_mux_mode 1"), "{log:?}");
    assert!(!log.iter().any(|c| c.contains("dgpu_disable")), "dGPU touched in the same step as the MUX: {log:?}");
    assert_eq!(log.iter().filter(|c| *c == "systemctl reboot").count(), 1, "{log:?}");
    assert!(!config::get_bool(&rig.cfg, "automation", "gpu_optimized"));
}

// guards: FR-17 - declining the confirmation leaves the hardware alone
fn gpu_panel__declined_confirmation_changes_nothing() {
    let _s = sysfs_map(&[(GPU_MUX_PATH, "1"), (DGPU_DISABLE_PATH, "0")]); // hybrid: Ultimate needs the MUX
    let (log, _c) = recording_capture(|_| ok_out(""));
    let rig = gpu_rig();
    click(&rig.panel, "Ultimate");
    assert_eq!(rig.confirms.borrow().len(), 1);
    assert_eq!(rig.confirms.borrow()[0].1, "Ultimate Mode requires restart");
    rig.confirms.borrow_mut()[0].3 = None; // dialog cancelled: the callback is dropped unused
    pump(400);
    assert!(seen(&log).is_empty(), "{:?}", seen(&log));
    // ...and the buttons were resynced to the real, unchanged state.
    let active: Vec<&str> = rig.panel.buttons.iter().filter(|(_, b)| b.is_active()).map(|(n, _)| *n).collect();
    assert_eq!(active, ["Standard"]);
}

// guards: FR-17 - with the real dialog (not a stand-in) nothing happens until it is answered
fn gpu_panel__real_dialog_blocks_until_answered() {
    let _s = sysfs_map(&[(GPU_MUX_PATH, "1"), (DGPU_DISABLE_PATH, "0")]);
    let (log, _c) = recording_capture(|_| ok_out(""));
    let parent = gtk::Window::new();
    let root = parent.clone();
    let panel = GpuPanel::new(no_toast(), make_cfg(), no_save(), on_ac(), Rc::new(move || Some(root.clone().upcast::<gtk::Widget>())));
    click(&panel, "Ultimate");
    pump(400);
    assert!(seen(&log).is_empty(), "the MUX was written while the dialog was still open: {:?}", seen(&log));
}

// guards: FR-17 - the confirmation dialog defaults to Cancel and only OK runs the action
fn widgets__confirm_dialog_runs_only_on_ok() {
    use rog_control::widgets::confirm_dialog;
    let parent = gtk::Window::new();
    let ran = Rc::new(RefCell::new(0));
    let counter = ran.clone();
    let dialog = confirm_dialog(Some(parent.upcast_ref()), "Reboot now?", "Needs a restart", "Reboot", Box::new(move || *counter.borrow_mut() += 1), true);
    assert_eq!(*ran.borrow(), 0, "ran before any answer");
    assert_eq!(dialog.default_response().as_deref(), Some("cancel"));
    assert_eq!(dialog.close_response(), "cancel");
    assert_eq!(dialog.response_appearance("ok"), adw::ResponseAppearance::Destructive);
    dialog.emit_by_name::<()>("response", &[&"cancel"]);
    assert_eq!(*ran.borrow(), 0, "Cancel ran the action");
    dialog.emit_by_name::<()>("response", &[&"ok"]);
    assert_eq!(*ran.borrow(), 1, "OK did not run the action");
}

// guards: FR-17/23 - a refused or failed MUX write reports a toast and never reboots
fn gpu_panel__failed_mux_write_toasts_and_never_reboots() {
    let _s = sysfs_map(&[(GPU_MUX_PATH, "0"), (DGPU_DISABLE_PATH, "0")]);
    let (log, _c) = recording_capture(|argv| if argv.iter().any(|a| a == "gpu_mux_mode") { failed_out("boom") } else { ok_out("") });
    let rig = gpu_rig();
    click(&rig.panel, "Eco");
    let on_confirm = rig.confirms.borrow_mut()[0].3.take().expect("confirm callback");
    on_confirm();
    pump(400);
    assert!(rig.toasts.borrow().iter().any(|t| t == "GPU mode failed: boom"), "{:?}", rig.toasts.borrow());
    assert!(!seen(&log).iter().any(|c| c == "systemctl reboot"), "{:?}", seen(&log));
}

// guards: FR-17 - switching among hybrid modes never asks, never touches the MUX, never reboots
fn gpu_panel__switch_without_mux_change_applies_at_once_and_never_reboots() {
    let _s = sysfs_map(&[(GPU_MUX_PATH, "1"), (DGPU_DISABLE_PATH, "0")]);
    let (log, _c) = recording_capture(|_| ok_out(""));
    let rig = gpu_rig();
    click(&rig.panel, "Eco");
    pump(300);
    assert!(rig.confirms.borrow().is_empty());
    let log = seen(&log);
    assert_eq!(log, ["asusctl armoury set dgpu_disable 1"], "{log:?}");
    assert!(!config::get_bool(&rig.cfg, "automation", "gpu_optimized"));
    click(&rig.panel, "Optimized");
    assert!(config::get_bool(&rig.cfg, "automation", "gpu_optimized"));
}

// guards: FR-3 - forward and backward cycling go the right way and wrap
fn profile_cycle__direction_and_wrap() {
    assert_eq!(cycle_target("Balanced", 1), "Performance");
    assert_eq!(cycle_target("Balanced", -1), "Quiet");
    assert_eq!(cycle_target("Performance", 1), "Quiet");
    assert_eq!(cycle_target("Quiet", -1), "Performance");
    assert_eq!(cycle_target("Nonsense", 1), "Balanced", "an unknown profile counts as the first");
    override_tool("powerprofilesctl", true);
    for (sysfs, step, expect_profile, expect_plan) in [
        ("balanced", 1, "Performance", "performance"),
        ("balanced", -1, "Quiet", "power-saver"),
        ("performance", 1, "Quiet", "power-saver"),
        ("quiet", -1, "Performance", "performance"),
    ] {
        let value: &'static str = sysfs;
        let _s = hook_sysfs(move |_| Some(value.to_string()), |_| Some(vec![]));
        let (calls, _g) = record_run_cmd();
        cycle_profile(step, None);
        let calls = calls.borrow();
        assert_eq!(calls.len(), 2, "{sysfs} {step}: {calls:?}");
        assert!(calls.contains(&["asusctl", "profile", "set", expect_profile].map(String::from).to_vec()), "{sysfs} {step}: {calls:?}");
        assert!(calls.contains(&["powerprofilesctl", "set", expect_plan].map(String::from).to_vec()), "{sysfs} {step}: {calls:?}");
    }
    clear_tool_overrides();
}

// guards: FR-3/6/7 - each D-Bus method does its own thing; bad input is an error, not an action
fn control__dispatch_maps_each_method_and_rejects_bad_input() {
    let log: Rc<RefCell<Vec<String>>> = Rc::default();
    let mk = |name: &'static str| {
        let log = log.clone();
        Box::new(move || log.borrow_mut().push(name.to_string())) as Box<dyn Fn()>
    };
    let window_log = log.clone();
    let handlers = ControlHandlers {
        open: mk("open"),
        cycle: mk("cycle"),
        cycle_back: mk("cycle_back"),
        open_window: Box::new(move |name| {
            if ["main", "fans", "lighting", "extra"].contains(&name) {
                window_log.borrow_mut().push(format!("window:{name}"));
                Ok(())
            } else {
                Err(format!("unknown window '{name}'"))
            }
        }),
    };
    let none = ().to_variant();
    for method in ["Open", "CycleProfile", "CycleProfileBack"] {
        dispatch(&handlers, method, &none).expect(method);
    }
    dispatch(&handlers, "OpenWindow", &("fans",).to_variant()).expect("OpenWindow fans");
    assert_eq!(*log.borrow(), ["open", "cycle", "cycle_back", "window:fans"]);
    for bad in ["", "nonsense", "../../etc/passwd"] {
        assert!(dispatch(&handlers, "OpenWindow", &(bad,).to_variant()).is_err(), "{bad:?} accepted");
    }
    assert!(dispatch(&handlers, "NoSuchMethod", &none).is_err());
    assert_eq!(log.borrow().len(), 4, "a rejected call still did something: {:?}", log.borrow());
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}

// guards: FR-3 - the flags select the right method (the ROG key opens, it never cycles)
fn control__flags_select_the_method() {
    assert_eq!(method_for_args(&strings(&["--open"])), Some("Open"));
    assert_eq!(method_for_args(&strings(&["--cycle-profile"])), Some("CycleProfile"));
    assert_eq!(method_for_args(&strings(&["--cycle-profile-back"])), Some("CycleProfileBack"));
    assert_eq!(method_for_args(&strings(&["--bogus"])), None);
    assert_eq!(method_for_args(&[]), None);
}

// guards: FR-3 - CycleProfile steps forward, CycleProfileBack steps backward
fn control__cycle_methods_step_forward_and_back() {
    let steps: Rc<RefCell<Vec<i32>>> = Rc::default();
    let sink = steps.clone();
    let handlers = ControlHandlers::new(|| {}, move |step| sink.borrow_mut().push(step), |_| Ok(()));
    let none = ().to_variant();
    dispatch(&handlers, "CycleProfile", &none).expect("CycleProfile");
    dispatch(&handlers, "CycleProfileBack", &none).expect("CycleProfileBack");
    assert_eq!(*steps.borrow(), [1, -1]);
}

// guards: FR-9 - any scroll on the tray icon cycles to the next profile
fn tray__scroll_always_cycles_forward() {
    let steps: Rc<RefCell<Vec<i32>>> = Rc::default();
    let sink = steps.clone();
    let handlers = TrayHandlers::new(|| {}, || {}, move |step| sink.borrow_mut().push(step));
    (handlers.scroll)(1);
    (handlers.scroll)(-1);
    assert_eq!(*steps.borrow(), [1, 1]);
}

// guards: FR-29 - saves are atomic and never drop keys this version does not know
fn config__save_is_atomic_and_keeps_unknown_keys() {
    let tmp = TempDir::new("cfgsafe");
    let path = tmp.0.join("config.json");
    std::fs::write(&path, r#"{"automation": {"gpu_optimized": true, "future_flag": 7}, "extra_section": {"keep": "me"}}"#).expect("seed");
    let mut cfg = config::load_from(&path);
    cfg["automation"]["profile_ac"] = json!("Balanced");
    config::save_to(&path, &cfg).expect("save");
    let back: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("valid json");
    assert_eq!(back["automation"]["future_flag"], json!(7), "unknown nested key dropped");
    assert_eq!(back["extra_section"]["keep"], json!("me"), "unknown section dropped");
    assert_eq!(back["automation"]["profile_ac"], json!("Balanced"));
    assert!(!tmp.0.join("config.json.tmp").exists(), "temp file left behind");

    // A failure while writing the temp file must leave the live file intact: only a
    // write-temp-then-rename save is safe against that. Block the temp path with a directory.
    let before = std::fs::read_to_string(&path).expect("read");
    std::fs::create_dir(tmp.0.join("config.json.tmp")).expect("block temp path");
    let mut cfg2 = cfg.clone();
    cfg2["automation"]["profile_ac"] = json!("Quiet");
    assert!(config::save_to(&path, &cfg2).is_err(), "save wrote in place instead of via a temp file");
    assert_eq!(std::fs::read_to_string(&path).expect("read"), before, "a failed save damaged the live config");
}

// guards: FR-29 - corrupt / missing / wrongly shaped files give defaults and are never rewritten by loading
fn config__corrupt_missing_and_misshapen_files_give_defaults() {
    let tmp = TempDir::new("cfgload");
    let defaults = config::defaults();
    assert_eq!(config::load_from(&tmp.0.join("missing.json")), defaults);
    for (name, body) in [("corrupt.json", "{not json"), ("array.json", "[1, 2]"), ("scalar.json", "5"), ("nulls.json", r#"{"automation": null, "power_limits": 3}"#)] {
        let path = tmp.0.join(name);
        std::fs::write(&path, body).expect("seed");
        assert_eq!(config::load_from(&path), defaults, "{name}");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), body, "{name} was rewritten by loading");
    }
}

// guards: FR-24 - the Startup switch runs systemctl enable / disable
fn startup_panel__switch_runs_enable_and_disable() {
    let (log, _c) = recording_capture(|_| ok_out(""));
    let _c2 = fake_capture(|argv| (argv.get(2).map(String::as_str) == Some("is-enabled")).then(|| ok_out("disabled\n")));
    let panel = StartupPanel::new(no_toast());
    pump(300);
    assert!(!panel.switch.is_active());
    assert!(seen(&log).is_empty(), "syncing the switch must not run enable/disable: {:?}", seen(&log));
    panel.switch.set_active(true);
    pump(200); // the two commands run on separate worker threads: let the first land first
    panel.switch.set_active(false);
    pump(200);
    assert_eq!(seen(&log), ["systemctl --user enable rog-control.service", "systemctl --user disable rog-control.service"]);
}

// guards: FR-19 - brightness buttons send `leds set <level>` and are remembered per power source
fn keyboard_panel__level_buttons_send_leds_set_and_are_remembered_per_power_source() {
    let _s = sysfs_map(&[(KBD_BRIGHTNESS_PATH, "0")]);
    for (on_battery, key) in [(false, "kbd_ac"), (true, "kbd_battery")] {
        let (calls, _g) = record_run_cmd();
        let cfg = make_cfg();
        let saved = Rc::new(RefCell::new(0));
        let counter = saved.clone();
        let panel = KeyboardPanel::new(no_toast(), cfg.clone(), Rc::new(move || *counter.borrow_mut() += 1), Rc::new(move || on_battery), Rc::new(|| {}));
        panel.buttons.iter().find(|(l, _)| *l == "med").expect("med button").1.set_active(true);
        assert_eq!(*calls.borrow(), vec![strings(&["asusctl", "leds", "set", "med"])]);
        assert_eq!(config::get_str(&cfg, "automation", key).as_deref(), Some("med"));
        assert_eq!(*saved.borrow(), 1);
    }
}

// guards: FR-16 - profile buttons run asusctl + the paired power plan and remember the choice
fn performance_panel__profile_buttons_apply_pair_and_remember() {
    override_tool("powerprofilesctl", true);
    let _s = sysfs_map(&[(PLATFORM_PROFILE_PATH, "balanced")]);
    for (on_battery, key) in [(false, "profile_ac"), (true, "profile_battery")] {
        let (calls, _g) = record_run_cmd();
        let cfg = make_cfg();
        let panel = PerformancePanel::new(no_toast(), cfg.clone(), no_save(), Rc::new(move || on_battery), Rc::new(|| {}));
        panel.buttons.iter().find(|(n, _)| *n == "Performance").expect("Turbo").1.set_active(true);
        let calls = calls.borrow();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert!(calls.contains(&strings(&["asusctl", "profile", "set", "Performance"])));
        assert!(calls.contains(&strings(&["powerprofilesctl", "set", "performance"])));
        assert_eq!(config::get_str(&cfg, "automation", key).as_deref(), Some("Performance"));
    }
    clear_tool_overrides();
}

// guards: FR-20/27 - the battery slider is debounced; Full runs `battery oneshot`
fn battery_panel__limit_is_debounced_and_oneshot_command() {
    let _s = sysfs_map(&[(BATTERY_LIMIT_PATH, "100")]);
    let (calls, _g) = record_run_cmd();
    let panel = BatteryPanel::new(no_toast());
    for v in (25..=60).step_by(5) {
        panel.scale.set_value(f64::from(v));
    }
    assert_eq!(panel.header.title.label(), "Battery Charge Limit: 60%", "the title follows the slider at once");
    assert!(calls.borrow().is_empty(), "sent before the quiet period: {:?}", calls.borrow());
    pump(400);
    assert_eq!(*calls.borrow(), vec![strings(&["asusctl", "battery", "limit", "60"])]);
    panel.oneshot_button.emit_clicked();
    assert_eq!(calls.borrow().last(), Some(&strings(&["asusctl", "battery", "oneshot"])));
}

// guards: FR-27 - the 2 s poll's refresh() must not overwrite a limit that is still waiting out its debounce
fn battery_panel__refresh_mid_debounce_keeps_the_users_limit() {
    let _s = sysfs_map(&[(BATTERY_LIMIT_PATH, "80")]);
    let (calls, _g) = record_run_cmd();
    let panel = BatteryPanel::new(no_toast());
    assert_eq!(panel.scale.value(), 80.0);
    panel.scale.set_value(60.0);
    pump(60);
    panel.refresh(); // the poll lands inside the 150 ms quiet period; hardware still says 80
    assert_eq!(panel.scale.value(), 60.0, "the poll replaced the user's value with the stale hardware one");
    assert_eq!(panel.header.title.label(), "Battery Charge Limit: 60%");
    pump(400);
    assert_eq!(*calls.borrow(), vec![strings(&["asusctl", "battery", "limit", "60"])], "the stale limit was sent");
    panel.refresh(); // nothing pending any more: hardware is the truth again
    assert_eq!(panel.scale.value(), 80.0, "refresh stayed disabled after the debounce fired");
}

fn effect_index(name: &str) -> u32 {
    aura_effect_names().iter().position(|e| *e == name).expect("known effect") as u32
}

/// What asusd "reports" in these tests, so a refresh that reverts to it is detectable whatever
/// keyboard the machine has: a static red effect at low speed.
fn asusd_says_static_red() -> rog_control::hw::AuraGuard {
    hook_aura_state(|| {
        Some(AuraState { effect: "static".into(), colour1: "ff0000".into(), colour2: "00ff00".into(), speed: "low".into(), direction: "left".into() })
    })
}

fn aura_effect_cmds(calls: &Calls) -> Vec<Vec<String>> {
    calls.borrow().iter().filter(|c| c.starts_with(&strings(&["asusctl", "aura", "effect"]))).cloned().collect()
}

// guards: FR-27 - a refresh (window shown, profile cycled, power source changed) must not revert an effect still waiting out its debounce
fn keyboard_panel__refresh_mid_debounce_keeps_the_users_edit() {
    let _a = asusd_says_static_red();
    let (calls, _g) = record_run_cmd();
    let panel = KeyboardPanel::new(no_toast(), make_cfg(), no_save(), on_ac(), Rc::new(|| {}));
    pump(50);
    assert_eq!(panel.effect_dropdown.selected(), effect_index("static"));
    let mine = "breathe";
    panel.effect_dropdown.set_selected(effect_index(mine));
    panel.colour_btn.set_rgba(&hex_to_gdk_rgba("112233"));
    pump(60);
    panel.refresh();
    assert_eq!(panel.effect_dropdown.selected(), effect_index(mine), "the refresh reverted the effect to asusd's");
    pump(400);
    let sent = aura_effect_cmds(&calls);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0][3], mine, "the old effect was sent: {:?}", sent[0]);
    assert!(sent[0].iter().any(|a| a == "112233"), "the picked colour was lost: {:?}", sent[0]);
    panel.refresh(); // nothing pending any more: asusd is the truth again
    assert_eq!(panel.effect_dropdown.selected(), effect_index("static"), "refresh stayed disabled after the debounce fired");
}

// guards: FR-27 - same for the Lighting window: refresh() must keep edits that have not been sent yet
fn lighting_window__refresh_mid_debounce_keeps_the_users_edit() {
    let _a = asusd_says_static_red();
    let (calls, _g) = record_run_cmd();
    let win = LightingWindow::new(None, no_toast(), None);
    pump(50);
    assert_eq!(win.effect_dropdown.selected(), effect_index("static"));
    let mine = "breathe";
    win.effect_dropdown.set_selected(effect_index(mine));
    win.speed_dropdown.set_selected(2); // "high"; asusd says "low"
    win.colour1_btn.set_rgba(&hex_to_gdk_rgba("112233"));
    pump(60);
    win.refresh();
    assert_eq!(win.effect_dropdown.selected(), effect_index(mine), "the refresh reverted the effect to asusd's");
    assert_eq!(win.speed_dropdown.selected(), 2, "the refresh reverted the speed to asusd's");
    pump(400);
    let sent = aura_effect_cmds(&calls);
    assert_eq!(sent.len(), 1, "{sent:?}");
    for needle in [mine, "112233", "high"] {
        assert!(sent[0].iter().any(|a| a == needle), "{needle} missing from {:?}", sent[0]);
    }
    win.refresh(); // nothing pending any more: asusd is the truth again
    assert_eq!(win.effect_dropdown.selected(), effect_index("static"), "refresh stayed disabled after the debounce fired");
}

// guards: FR-27 - every slider of Fans and Power (synced by refresh() and by the async NVIDIA probe) keeps a value still waiting out its debounce
fn fans_window__refresh_mid_debounce_keeps_every_slider_value() {
    let sample = "Current Power Limit : 150.00 W\nMin Power Limit : 100.00 W\nMax Power Limit : 175.00 W\n";
    let _c = fake_capture(move |argv| match argv.join(" ").as_str() {
        "nvidia-smi -q -d POWER" => Some(ok_out(sample)),
        a if a.starts_with("nvidia-settings -q ") => Some(ok_out("")),
        _ => None,
    });
    override_tool("nvidia-settings", true);
    let _s = sysfs_map(&[(PPT_PL1_PATH, "40"), (PPT_PL2_PATH, "60"), (NV_BOOST_PATH, "5"), (NV_TEMP_TARGET_PATH, "80")]);
    let (_calls, _g) = record_run_cmd();
    let cfg = make_cfg();
    let win = FansPowerWindow::new(None, no_toast(), cfg.clone(), no_save());
    pump(300); // the probe lands: later refreshes apply its state at once
    let sliders = [
        ("pl1", &win.pl1, 70.0),
        ("pl2", &win.pl2, 100.0),
        ("boost", &win.boost, 15.0),
        ("temp_target", &win.temp_target, 90.0),
        ("core_clock", &win.core_clock, 120.0),
        ("mem_clock", &win.mem_clock, 400.0),
        ("gpu_power", &win.gpu_power, 120.0),
    ];
    for (_, slider, value) in &sliders {
        slider.scale.set_value(*value);
    }
    pump(60);
    win.refresh(); // inside every slider's quiet period
    for (key, slider, value) in &sliders {
        assert_eq!(slider.scale.value(), *value, "refresh() replaced the user's {key} value");
    }
    pump(400);
    for (key, _, value) in &sliders {
        assert_eq!(cfg.borrow()["power_limits"][*key].as_f64(), Some(*value), "the stale {key} value was applied");
    }
    win.refresh(); // nothing pending any more: hardware is the truth again
    assert_eq!(win.pl1.scale.value(), 40.0, "refresh stayed disabled after the debounce fired");
    clear_tool_overrides();
}

// guards: FR-29 - the first save over an unusable file keeps the original as config.json.corrupt, once
fn config__first_save_keeps_a_copy_of_an_unusable_file() {
    let tmp = TempDir::new("cfgkeep");
    let path = tmp.0.join("config.json");
    let backup = tmp.0.join("config.json.corrupt");
    let read = |p: &std::path::Path| std::fs::read(p).expect("read");

    std::fs::write(&path, "{not json").expect("seed");
    let mut cfg = config::load_from(&path);
    assert!(!backup.exists(), "loading must not create anything");
    assert_eq!(read(&path), b"{not json", "loading must not rewrite the file");
    cfg["automation"]["profile_ac"] = json!("Balanced");
    config::save_to(&path, &cfg).expect("save");
    assert_eq!(read(&backup), b"{not json", "the user's original file was not kept");
    assert_eq!(config::load_from(&path)["automation"]["profile_ac"], json!("Balanced"));

    // A later unusable file never replaces the first copy, and a healthy file never makes one.
    std::fs::write(&path, "[1, 2]").expect("seed again");
    config::save_to(&path, &cfg).expect("save");
    assert_eq!(read(&backup), b"{not json", "an existing .corrupt was overwritten");
    let healthy = TempDir::new("cfgkeep2");
    let hpath = healthy.0.join("config.json");
    std::fs::write(&hpath, r#"{"automation": {"gpu_optimized": true}}"#).expect("seed");
    config::save_to(&hpath, &config::load_from(&hpath)).expect("save");
    std::fs::write(&hpath, "   \n").expect("empty file");
    config::save_to(&hpath, &cfg).expect("save");
    assert!(!healthy.0.join("config.json.corrupt").exists(), "a copy was made of a file with nothing in it");
}

// guards: FR-31 - a PREFIX/HOME that systemd would misread is refused before anything is built or written
fn install__refuses_a_prefix_that_would_corrupt_the_unit() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    let root = TempDir::new("install");
    let fakebin = root.0.join("fakebin");
    std::fs::create_dir_all(&fakebin).expect("fakebin");
    let ran = root.0.join("cargo-ran");
    // A cargo that only records it was called and fails: even a broken check cannot build or install here.
    let cargo = fakebin.join("cargo");
    std::fs::write(&cargo, format!("#!/bin/sh\ntouch '{}'\nexit 1\n", ran.display())).expect("fake cargo");
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let path = format!("{}:{}", fakebin.display(), std::env::var("PATH").unwrap_or_default());
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/install.sh");
    let run = |var: &str, value: &str| {
        let home = root.0.join("home");
        let mut cmd = Command::new("bash");
        cmd.arg(script).arg("--no-enable").env_clear().env("PATH", &path).env("HOME", &home);
        match var {
            "HOME" => cmd.env("HOME", root.0.join(value)),
            _ => cmd.env("PREFIX", root.0.join(value)),
        };
        let out = cmd.output().expect("run install.sh");
        (out.status.code(), String::from_utf8_lossy(&out.stderr).into_owned())
    };
    let nothing_written = || !root.0.join("home").exists() && !ran.exists();

    for (var, value) in [("PREFIX", "pre fix"), ("PREFIX", "50%off"), ("PREFIX", "q\"uote"), ("PREFIX", "back\\slash"), ("PREFIX", "dollar$x"), ("HOME", "my home")] {
        let (code, err) = run(var, value);
        assert_eq!(code, Some(2), "{var}={value:?} was not refused: {err}");
        assert!(err.contains("cannot install") && err.contains("PREFIX"), "{var}={value:?}: the message does not name the problem: {err}");
        assert!(nothing_written(), "{var}={value:?}: files were written or the build started");
    }
    // Control: a plain path passes the check and goes on to the build (our fake cargo fails it).
    let (code, err) = run("PREFIX", "plain");
    assert_eq!(code, Some(1), "a plain PREFIX was refused: {err}");
    assert!(ran.exists(), "a plain PREFIX never reached the build: {err}");
}

fn monitor_fake(refresh: f64) -> rog_control::sys::CaptureGuard {
    let text = json!([{"name": "eDP-1", "width": 2560, "height": 1440, "x": 0, "y": 0, "scale": 1, "refreshRate": refresh,
        "availableModes": ["2560x1440@60.00Hz", "2560x1440@240.00Hz"]}])
    .to_string();
    fake_capture(move |argv| (argv.get(1).map(String::as_str) == Some("monitors")).then(|| ok_out(&text)))
}

// guards: FR-18 - presets use hyprctl eval; "Auto" only remembers the opt-in
fn screen_panel__presets_apply_and_auto_only_remembers() {
    let _m = monitor_fake(240.0);
    let _s = hook_sysfs(|_| None, |_| Some(vec![]));
    let (calls, _g) = record_run_cmd();
    let (toasts, toast) = toast_sink();
    let cfg = make_cfg();
    let panel = ScreenPanel::new(toast, cfg.clone(), no_save());
    pump(300);
    let active = |p: &ScreenPanel| p.buttons.iter().filter(|(_, b)| b.is_active()).map(|(k, _)| *k).collect::<Vec<_>>();
    assert_eq!(active(&panel), ["max"], "a 240 Hz panel shows the max preset");
    panel.buttons.iter().find(|(k, _)| *k == "60").expect("60Hz").1.set_active(true);
    pump(300);
    let eval = calls.borrow().iter().find(|c| c[1] == "eval").cloned().expect("hyprctl eval was not run");
    assert!(eval[2].contains("mode = \"2560x1440@60\""), "{}", eval[2]);
    let before = calls.borrow().len();
    panel.buttons.iter().find(|(k, _)| *k == "auto").expect("Auto").1.set_active(true);
    pump(300);
    assert_eq!(calls.borrow().len(), before, "Auto must not touch the display itself");
    assert!(config::get_bool(&cfg, "automation", "screen_auto"));
    assert!(toasts.borrow().iter().any(|t| t.starts_with("Screen: auto")), "{:?}", toasts.borrow());
}

// guards: FR-24/23 - boot sound and charging mode commands; a failed write toasts the tool's text
fn extra_window__controls_send_armoury_commands_and_failures_toast() {
    let _s = sysfs_map(&[(BOOT_SOUND_PATH, "1"), (CHARGE_MODE_PATH, "1")]);
    {
        let (calls, _g) = record_run_cmd();
        let win = ExtraWindow::new(None, no_toast());
        assert!(win.boot_sound_switch.is_active());
        assert!(win.charge_mode_buttons[1].1.is_active());
        assert!(!win.empty_notice.get_visible());
        win.charge_mode_buttons[2].1.set_active(true);
        win.boot_sound_switch.set_active(false);
        assert_eq!(*calls.borrow(), vec![strings(&["asusctl", "armoury", "set", "charge_mode", "2"]), strings(&["asusctl", "armoury", "set", "boot_sound", "0"])]);
    }
    let (_log, _c) = recording_capture(|_| failed_out("boom"));
    let (toasts, toast) = toast_sink();
    let win = ExtraWindow::new(None, toast);
    win.boot_sound_switch.set_active(false);
    pump(300);
    assert_eq!(*toasts.borrow(), ["Command failed: boom"]);
}

// guards: FR-21/23 - a failed power-limit write toasts; the power-mode dropdown drives powerprofilesctl
fn fans_window__failed_write_toasts_and_power_mode_pairs_with_ppd() {
    override_tool("powerprofilesctl", true);
    let (log, _c) = recording_capture(|argv| if argv.iter().any(|a| a == "ppt_pl2_sppt") { failed_out("not supported by firmware") } else { ok_out("") });
    let _c2 = fake_capture(|argv| (argv.get(1).map(String::as_str) == Some("get")).then(|| ok_out("balanced\n")));
    let (toasts, toast) = toast_sink();
    let win = FansPowerWindow::new(None, toast, make_cfg(), no_save());
    pump(300);
    assert_eq!(win.power_mode_dropdown.selected(), 1, "the dropdown shows the current plan");
    win.pl2.scale.set_value(120.0);
    pump(400);
    assert!(toasts.borrow().iter().any(|t| t == "Command failed: not supported by firmware"), "{:?}", toasts.borrow());
    assert!(win.pl2.scale.get_visible() && win.pl2.root.get_visible(), "an unavailable control must stay shown");
    win.power_mode_dropdown.set_selected(0);
    pump(200);
    assert!(seen(&log).contains(&"powerprofilesctl set power-saver".to_string()), "{:?}", seen(&log));
    clear_tool_overrides();
}

// guards: FR-21 - the fan-curve editor writes the whole curve, enable and disable with exact arguments
fn fan_curve_editor__apply_and_enable_commands() {
    let (calls, _g) = record_run_cmd();
    // Whatever the real GPU curve is doing, this one starts disabled.
    let _c = fake_capture(|argv| {
        (argv.get(1).map(String::as_str) == Some("fan-curve"))
            .then(|| ok_out("[(fan: GPU, pwm: (36, 56, 89, 105, 117, 153, 168, 255), temp: (47, 51, 55, 60, 65, 70, 76, 97), enabled: false,)]"))
    });
    let editor = FanCurveEditor::new("gpu", no_toast(), Rc::new(|| "Balanced".to_string()));
    pump(300);
    assert!(!editor.enable_switch.is_active());
    editor.graph.set_points(&DEFAULT_CURVE);
    editor.auto_apply();
    editor.enable_switch.set_active(true);
    editor.enable_switch.set_active(false);
    let calls = calls.borrow();
    assert_eq!(
        calls[0],
        strings(&["asusctl", "fan-curve", "--mod-profile", "Balanced", "--fan", "gpu", "--data", "40c:20%,50c:30%,55c:40%,60c:50%,65c:60%,70c:75%,80c:90%,95c:100%"])
    );
    assert_eq!(calls[1], strings(&["asusctl", "fan-curve", "--mod-profile", "Balanced", "--enable-fan-curve", "true", "--fan", "gpu"]));
    assert_eq!(calls[2], strings(&["asusctl", "fan-curve", "--mod-profile", "Balanced", "--enable-fan-curve", "false", "--fan", "gpu"]));
}

// guards: FR-21 - power-limit bounds parse from nvidia-smi; clock offsets use the exact attribute
fn nvidia__power_limit_parsing_and_clock_commands() {
    let sample = "    Power Readings\n        Current Power Limit               : 150.00 W\n        Default Power Limit               : 125.00 W\n        Min Power Limit                   : 100.00 W\n        Max Power Limit                   : 175.00 W\n";
    assert_eq!(parse_power_limit_bounds(sample), Some((150.0, 100.0, 175.0)));
    assert_eq!(parse_power_limit_bounds("Current Power Limit : N/A\nMin Power Limit : 1 W\nMax Power Limit : 2 W"), None);
    assert_eq!(parse_power_limit_bounds(""), None);
    let (calls, _g) = record_run_cmd();
    apply_core_clock_offset(-50, None);
    apply_mem_clock_offset(300, None);
    assert_eq!(calls.borrow()[0], strings(&["nvidia-settings", "-a", "[gpu:0]/GPUGraphicsClockOffset[3]=-50"]));
    assert_eq!(calls.borrow()[1], strings(&["nvidia-settings", "-a", "[gpu:0]/GPUMemoryTransferRateOffset[3]=300"]));
}

fn automation_rig(cfg: serde_json::Value, on_battery: bool) -> (Rc<AutomationManager>, Calls) {
    override_tool("powerprofilesctl", true);
    override_tool("notify-send", true);
    let (calls, guard) = record_run_cmd();
    std::mem::forget(guard); // lives for the rest of the test via the returned calls handle
    let manager = AutomationManager::new(config::new_cfg(config::merge_defaults(cfg, &config::defaults())), Rc::new(|| {}));
    manager.set_on_battery(Some(on_battery));
    (manager, calls)
}

// guards: FR-28 - the remembered profile / keyboard level for the current power source is applied, and notified only on transitions
fn automation__applies_remembered_state_per_power_source() {
    let cfg = json!({"automation": {"profile_ac": "Performance", "profile_battery": "Quiet", "kbd_ac": "high", "kbd_battery": "off"}});
    let (ac, calls) = automation_rig(cfg.clone(), false);
    ac.apply_all(false);
    let c = calls.borrow().clone();
    assert!(c.contains(&strings(&["asusctl", "profile", "set", "Performance"])), "{c:?}");
    assert!(c.contains(&strings(&["powerprofilesctl", "set", "performance"])), "{c:?}");
    assert!(c.contains(&strings(&["asusctl", "leds", "set", "high"])), "{c:?}");
    assert!(!c.iter().any(|x| x[0] == "notify-send"), "a silent startup pass notified: {c:?}");
    ac.apply_all(true);
    pump(100);
    assert!(calls.borrow().iter().any(|x| x == &strings(&["notify-send", "-a", "ROG Control", "-t", "2000", "On AC power", "Automation applied"])), "{:?}", calls.borrow());

    let (battery, calls) = automation_rig(cfg, true);
    battery.apply_all(false);
    let c = calls.borrow().clone();
    assert!(c.contains(&strings(&["asusctl", "profile", "set", "Quiet"])), "{c:?}");
    assert!(c.contains(&strings(&["asusctl", "leds", "set", "off"])), "{c:?}");

    // A hand-edited config with garbage values must not reach the tools.
    let (bad, calls) = automation_rig(json!({"automation": {"profile_ac": "Turbo; reboot", "kbd_ac": "maximum"}}), false);
    bad.apply_all(false);
    assert!(calls.borrow().is_empty(), "{:?}", calls.borrow());

    // Without a known power source nothing is applied at all.
    let (unknown, calls) = automation_rig(cfg_profile_only(), false);
    unknown.set_on_battery(None);
    unknown.apply_all(false);
    assert!(calls.borrow().is_empty());
    clear_tool_overrides();
}

fn cfg_profile_only() -> serde_json::Value {
    json!({"automation": {"profile_ac": "Performance"}})
}

// guards: FR-17/28 - Optimized never changes the MUX by itself and honours the heavy-use guard
fn automation__gpu_optimized_guards() {
    let cfg = json!({"automation": {"gpu_optimized": true}});
    {
        // Ultimate (MUX 0): automation has no dialog, so it must not write the MUX.
        let _s = sysfs_map(&[(GPU_MUX_PATH, "0"), (DGPU_DISABLE_PATH, "0")]);
        let (m, calls) = automation_rig(cfg.clone(), false);
        m.apply_all(false);
        assert!(!calls.borrow().iter().any(|c| c.iter().any(|a| a == "gpu_mux_mode" || a == "dgpu_disable")), "{:?}", calls.borrow());
    }
    let _s = sysfs_map(&[(GPU_MUX_PATH, "1"), (DGPU_DISABLE_PATH, "0")]);
    {
        let (m, calls) = automation_rig(cfg.clone(), false); // on AC: dGPU stays on
        m.apply_all(false);
        assert_eq!(calls.borrow().iter().filter(|c| c.iter().any(|a| a == "dgpu_disable")).cloned().collect::<Vec<_>>(), [strings(&["asusctl", "armoury", "set", "dgpu_disable", "0"])]);
    }
    {
        // On battery with the GPU busy: leave it alone.
        let _c = fake_capture(|argv| argv.get(1).is_some_and(|a| a.starts_with("--query-gpu=utilization")).then(|| ok_out("90\n")));
        let (m, calls) = automation_rig(cfg.clone(), true);
        m.apply_all(false);
        pump(300);
        assert!(!calls.borrow().iter().any(|c| c.iter().any(|a| a == "dgpu_disable")), "{:?}", calls.borrow());
    }
    {
        // On battery and idle: disable it.
        let _c = fake_capture(|argv| argv.get(1).is_some_and(|a| a.starts_with("--query-gpu=utilization")).then(|| ok_out("3\n")));
        let (m, calls) = automation_rig(cfg, true);
        m.apply_all(false);
        pump(300);
        assert!(calls.borrow().contains(&strings(&["asusctl", "armoury", "set", "dgpu_disable", "1"])), "{:?}", calls.borrow());
    }
    clear_tool_overrides();
}

// guards: FR-8 - the tray properties carry the exact wire types and values
fn tray__properties_match_the_wire_contract() {
    use rog_control::tray::property;
    assert_eq!(property("Id", "").str(), Some("rog-control"));
    assert_eq!(property("Title", "").str(), Some("ROG Control"));
    assert_eq!(property("Category", "").str(), Some("Hardware"));
    assert_eq!(property("Status", "").str(), Some("Active"));
    assert_eq!(property("IconName", "").str(), Some("rog-control-symbolic"));
    assert_eq!(property("IconThemePath", "/x/icons").str(), Some("/x/icons"));
    assert_eq!(property("ItemIsMenu", "").get::<bool>(), Some(false));
    let tip = property("ToolTip", "");
    assert_eq!(tip.type_().as_str(), "(sa(iiay)ss)");
    assert_eq!(tip.child_value(2).str(), Some("ROG Control"));
    assert!(tip.child_value(3).str().is_some_and(|t| t.contains("Left-click")));
}

// guards: FR-19 - asusd's LedModeData is decoded, and each effect only sends the fields it takes
fn aura__led_mode_data_and_effect_commands() {
    let data = (1u32, 0u32, (247u8, 182u8, 185u8), (230u8, 189u8, 190u8), "Med", "Right").to_variant();
    let state = parse_led_mode_data(&data).expect("parses");
    assert_eq!(state, AuraState { effect: "breathe".into(), colour1: "f7b6b9".into(), colour2: "e6bdbe".into(), speed: "med".into(), direction: "right".into() });
    let unknown_mode = (9u32, 0u32, (0u8, 0u8, 0u8), (0u8, 0u8, 0u8), "Low", "Up").to_variant();
    assert_eq!(parse_led_mode_data(&unknown_mode).expect("parses").effect, "static", "an unmapped mode falls back to static");
    assert!(parse_led_mode_data(&"nope".to_variant()).is_none());

    assert_eq!(build_aura_cmd("static", "ff0000", "00ff00", "up", "low"), strings(&["asusctl", "aura", "effect", "static", "-c", "ff0000"]));
    assert_eq!(
        build_aura_cmd("breathe", "ff0000", "00ff00", "up", "high"),
        strings(&["asusctl", "aura", "effect", "breathe", "--colour", "ff0000", "--colour2", "00ff00", "--speed", "high"])
    );
    assert_eq!(build_aura_cmd("rainbow-wave", "ff0000", "00ff00", "left", "med"), strings(&["asusctl", "aura", "effect", "rainbow-wave", "--direction", "left", "--speed", "med"]));
    assert_eq!(build_aura_cmd("rain", "ff0000", "00ff00", "left", "low"), strings(&["asusctl", "aura", "effect", "rain", "--speed", "low"]));
    assert!(theme_primary_hex().len() == 6);
}

// guards: Python-parity helpers (banker's rounding, capitalize, {:.0} formatting) behave like Python's
fn helpers__python_compatible_rounding_and_formatting() {
    use rog_control::sys::{capitalize, py_round, truncate_chars};
    assert_eq!([0.5, 1.5, 2.5, 3.5, -0.5].map(py_round), [0, 2, 2, 4, 0]);
    assert_eq!(capitalize("performance"), "Performance");
    assert_eq!(capitalize("LOW-POWER"), "Low-power");
    assert_eq!(capitalize(""), "");
    assert_eq!(format!("{:.0}", 56.5), "56", "Python prints 56 for 56.5");
    assert_eq!(format!("{:.0}", 57.5), "58");
    assert_eq!(truncate_chars("héllo wörld", 5), "héllo");
    assert_eq!(max_refresh_rate(None), 60);
    let m = json!({"availableModes": ["2560x1440@60.00Hz", "2560x1440@239.99Hz"]});
    assert_eq!(max_refresh_rate(Some(&m)), 240);
    assert_eq!(max_refresh_rate(Some(&json!({"availableModes": ["garbage"]}))), 60);
}

// guards: FR-20 - live battery figures are formatted as the reference does
fn battery_text__charging_and_health() {
    {
        let _s = sysfs_map(&[(BATTERY_STATUS_PATH, "Discharging"), (BATTERY_POWER_NOW_PATH, "15500000")]);
        assert_eq!(battery_charging_text(), "Discharging: 15.5W");
    }
    {
        let _s = sysfs_map(&[(BATTERY_STATUS_PATH, "Charging"), (BATTERY_POWER_NOW_PATH, "42000000")]);
        assert_eq!(battery_charging_text(), "Charging: 42.0W");
    }
    {
        let _s = sysfs_map(&[(BATTERY_STATUS_PATH, "Full"), (BATTERY_POWER_NOW_PATH, "100")]);
        assert_eq!(battery_charging_text(), "Full");
    }
    {
        let _s = sysfs_map(&[(BATTERY_STATUS_PATH, "Not charging")]);
        assert_eq!(battery_charging_text(), "Not charging");
    }
    {
        let _s = sysfs_map(&[]);
        assert_eq!(battery_charging_text(), "--");
        assert_eq!(battery_health_text(), None);
    }
    let _s = sysfs_map(&[(BATTERY_ENERGY_FULL_PATH, "40000000"), (BATTERY_ENERGY_FULL_DESIGN_PATH, "50000000")]);
    assert_eq!(battery_health_text().as_deref(), Some("Battery Health: 80.0%"));
}

// guards: FR-12 - only the two known environment messages are dropped from the log, nothing else
fn theme__filters_only_known_environment_noise() {
    use rog_control::theme::is_environment_noise;
    assert!(is_environment_noise("Theme parser error: gtk.css:312:16-28: Unknown name of pseudo-class"));
    assert!(is_environment_noise("Unable to connect to the accessibility bus at 'unix:path=/run/user/1000/at-spi/bus_1': refused"));
    assert!(!is_environment_noise("Theme parser error: <data>:3:1-5: Invalid property"), "errors in the app's own CSS must show");
    assert!(!is_environment_noise("Attempting to read the value of a nonexistent property"));
    assert!(!is_environment_noise(""));
}
