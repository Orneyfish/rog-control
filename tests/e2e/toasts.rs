#![allow(non_snake_case)]
//! FR-23: a failed hardware write is reported as a toast that carries the tool's own error text,
//! and a successful one stays silent. Every control that writes to hardware has a case here:
//! the tool is made to fail for that one command, the control is driven, and the toast must be
//! exactly `<prefix>: <tool text>`. The same control is then driven with the tool succeeding, so a
//! toast that fires on success (or a case that never reaches the write) cannot pass either.
//!
//! The list is closed by `toasts__every_failure_site_in_src_has_a_case`: adding a failure toast to
//! the code without adding its case here fails that test.

use std::any::Any;
use std::rc::Rc;

use adw::prelude::*;
use rog_control::extra::ExtraWindow;
use rog_control::fans::{FanCurveEditor, FansPowerWindow, DEFAULT_CURVE};
use rog_control::hw::*;
use rog_control::lighting::LightingWindow;
use rog_control::panels::*;
use rog_control::popup::RogPopup;
use rog_control::sys::{clear_tool_overrides, hook_sysfs, override_tool, CmdOut};

use crate::common::*;
use crate::extra::{asusd_says_static_red, effect_index, monitor_fake, recording_capture, seen, sysfs_map};

pub fn tests() -> Vec<(&'static str, fn())> {
    macro_rules! t {
        ($($name:ident),* $(,)?) => { vec![$((stringify!($name), $name as fn())),*] };
    }
    t![
        toasts__main_window_panels_report_failed_writes,
        toasts__gpu_panel_reports_failed_and_refused_writes,
        toasts__companion_windows_report_failed_writes,
        toasts__fan_curve_editor_reports_failed_writes,
        toasts__popup_routes_panel_and_companion_toasts_to_its_overlay,
        toasts__every_failure_site_in_src_has_a_case,
    ]
}

/// Number of `failure_text(` call sites in `src/` (outside its definition) that the tests below cover.
const FAILURE_SITES: usize = 11;

/// Drives one control twice against the same tool: once with every write succeeding, once with the
/// write that starts with `write` answering `failure`. The write must run both times; the first
/// run must show no toast and the second exactly `expected`. `act` builds the widget on the given
/// toast sink, drives the control and returns the widget so it stays alive while callbacks land.
fn check(case: &str, write: &'static str, failure: CmdOut, expected: &str, act: impl Fn(ToastFn) -> Box<dyn Any>) {
    for failing in [false, true] {
        let failure = failure.clone();
        let (log, _tool) = recording_capture(move |argv| if failing && argv.join(" ").starts_with(write) { failure.clone() } else { ok_out("") });
        let (toasts, toast) = toast_sink();
        let _widget = act(toast);
        pump_until(|| seen(&log).iter().any(|c| c.starts_with(write)) && (!failing || !toasts.borrow().is_empty()));
        assert!(seen(&log).iter().any(|c| c.starts_with(write)), "{case}: `{write}` never ran: {:?}", seen(&log));
        let want: Vec<String> = if failing { vec![expected.to_string()] } else { vec![] };
        assert_eq!(*toasts.borrow(), want, "{case}: tool {}", if failing { "failing" } else { "succeeding" });
    }
}

/// Pumps the main loop until `done` holds (at most 3 s), then a moment longer so that a duplicate
/// or late toast would show up in the assertions that follow.
fn pump_until(done: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while !done() && std::time::Instant::now() < deadline {
        pump(20);
    }
    pump(80);
}

fn press(buttons: &[(&'static str, gtk::ToggleButton)], name: &str) {
    buttons.iter().find(|(n, _)| *n == name).unwrap_or_else(|| panic!("no {name} button")).1.set_active(true);
}

// guards: FR-23 - Performance, Screen, Keyboard, Battery and Startup toast a failed write with the tool's text
fn toasts__main_window_panels_report_failed_writes() {
    let boom = || failed_out("boom");

    // report_cb: Performance profile
    {
        let _s = sysfs_map(&[(PLATFORM_PROFILE_PATH, "balanced")]);
        check("performance profile", "asusctl profile set Performance", boom(), "Command failed: boom", |toast| {
            let panel = PerformancePanel::new(toast, make_cfg(), no_save(), on_ac(), Rc::new(|| {}));
            press(&panel.buttons, "Performance");
            Box::new(panel)
        });
    }

    // report_cb: Screen preset, both steps of the combined "<Hz> + OD" write
    {
        let _m = monitor_fake(240.0);
        let screen = |toast: ToastFn| {
            let panel = ScreenPanel::new(toast, make_cfg(), no_save());
            pump(100); // the first sync reads the monitor and must not be mistaken for a click
            press(&panel.buttons, "60");
            Box::new(panel) as Box<dyn Any>
        };
        {
            let _s = sysfs_map(&[]); // no panel_overdrive attribute: the display step is the only write
            check("screen preset, display step", "hyprctl eval", boom(), "Command failed: boom", screen);
        }
        let _s = sysfs_map(&[(PANEL_OVERDRIVE_PATH, "1")]);
        check("screen preset, display step before overdrive", "hyprctl eval", boom(), "Command failed: boom", screen);
        check("screen preset, overdrive step", "asusctl armoury set panel_overdrive 0", boom(), "Command failed: boom", screen);
    }

    // report_cb: Screen preset when the monitor cannot be read at all
    {
        let _s = sysfs_map(&[]);
        let _m = fake_capture(|argv| (argv.get(1).map(String::as_str) == Some("monitors")).then(|| ok_out("")));
        let (log, _tool) = recording_capture(|_| ok_out(""));
        let (toasts, toast) = toast_sink();
        let panel = ScreenPanel::new(toast, make_cfg(), no_save());
        pump(100);
        press(&panel.buttons, "max"); // with no monitor to read, "60" would already be the selected one
        pump_until(|| !toasts.borrow().is_empty());
        assert_eq!(*toasts.borrow(), ["Command failed: could not read monitor info"]);
        assert!(seen(&log).is_empty(), "wrote to the display without knowing which one: {:?}", seen(&log));
    }

    // report_cb: Keyboard brightness
    {
        let _s = sysfs_map(&[(KBD_BRIGHTNESS_PATH, "0")]);
        check("keyboard level", "asusctl leds set med", boom(), "Command failed: boom", |toast| {
            let panel = KeyboardPanel::new(toast, make_cfg(), no_save(), on_ac(), Rc::new(|| {}));
            press(&panel.buttons, "med");
            Box::new(panel)
        });
    }

    // Keyboard effect
    {
        let _a = asusd_says_static_red();
        check("keyboard effect", "asusctl aura effect", boom(), "Effect failed: boom", |toast| {
            let panel = KeyboardPanel::new(toast, make_cfg(), no_save(), on_ac(), Rc::new(|| {}));
            pump(50);
            panel.effect_dropdown.set_selected(effect_index("breathe"));
            Box::new(panel)
        });
    }

    // Battery: the debounced limit and the Full button share one failure callback
    {
        let _s = sysfs_map(&[(BATTERY_LIMIT_PATH, "100")]);
        check("battery limit", "asusctl battery limit 60", boom(), "Command failed: boom", |toast| {
            let panel = BatteryPanel::new(toast);
            panel.scale.set_value(60.0);
            Box::new(panel)
        });
        check("battery full charge", "asusctl battery oneshot", boom(), "Command failed: boom", |toast| {
            let panel = BatteryPanel::new(toast);
            panel.oneshot_button.emit_clicked();
            Box::new(panel)
        });
    }

    // report_cb: Run on Startup
    {
        let _state = fake_capture(|argv| (argv.get(2).map(String::as_str) == Some("is-enabled")).then(|| ok_out("disabled\n")));
        check("run on startup", "systemctl --user enable rog-control.service", boom(), "Command failed: boom", |toast| {
            let panel = StartupPanel::new(toast);
            pump(100);
            panel.switch.set_active(true);
            Box::new(panel)
        });
    }
}

// guards: FR-17/23 - a failed dgpu_disable write and a refused one (external display) both toast
fn toasts__gpu_panel_reports_failed_and_refused_writes() {
    let gpu = |toast: ToastFn, mode: &'static str| {
        let panel = GpuPanel::new(toast, make_cfg(), no_save(), on_ac(), Rc::new(|| None));
        press(&panel.buttons, mode);
        Box::new(panel) as Box<dyn Any>
    };
    {
        let _s = sysfs_map(&[(GPU_MUX_PATH, "1"), (DGPU_DISABLE_PATH, "0")]); // hybrid: no MUX change, no dialog
        check("gpu mode, dgpu_disable", "asusctl armoury set dgpu_disable 1", failed_out("boom"), "GPU mode failed: boom", |toast| gpu(toast, "Eco"));
    }
    // The refusal text is built by apply_gpu_mode itself, not by a tool, so there is no write to fail.
    let _s = hook_sysfs(
        |p| match p {
            GPU_MUX_PATH => Some("1".to_string()),
            p if p.ends_with("/device/vendor") => Some("0x10de".to_string()),
            p if p.ends_with("/status") => Some("connected".to_string()),
            _ => None,
        },
        |dir| (dir == "/sys/class/drm").then(|| vec!["card2".to_string(), "card2-HDMI-A-1".to_string()]),
    );
    let (log, _tool) = recording_capture(|_| ok_out(""));
    let (toasts, toast) = toast_sink();
    let panel = gpu(toast, "Eco");
    pump_until(|| !toasts.borrow().is_empty());
    let toasts = toasts.borrow();
    assert_eq!(toasts.len(), 1, "{toasts:?}");
    assert!(toasts[0].starts_with("GPU mode failed: An external display is connected directly to the dGPU"), "{toasts:?}");
    assert_eq!(toasts[0].chars().count(), "GPU mode failed: ".len() + 80, "a toast shows the first 80 characters of the error: {toasts:?}");
    assert!(seen(&log).is_empty(), "disabled the dGPU out from under an external display: {:?}", seen(&log));
    drop(panel);
}

fn nvidia_and_ppd_tools() -> rog_control::sys::CaptureGuard {
    let sample = "Current Power Limit : 150.00 W\nMin Power Limit : 100.00 W\nMax Power Limit : 175.00 W\n";
    override_tool("nvidia-settings", true);
    override_tool("powerprofilesctl", true);
    fake_capture(move |argv| match argv.join(" ").as_str() {
        "nvidia-smi -q -d POWER" => Some(ok_out(sample)),
        "powerprofilesctl get" => Some(ok_out("balanced\n")),
        a if a.starts_with("nvidia-settings -q ") => Some(ok_out("")),
        _ => None,
    })
}

// guards: FR-23 - Extra, Fans and Power (every slider and the power-mode menu) and Lighting toast a failed write
fn toasts__companion_windows_report_failed_writes() {
    let boom = || failed_out("boom");

    {
        let _s = sysfs_map(&[(BOOT_SOUND_PATH, "1"), (CHARGE_MODE_PATH, "1")]);
        check("extra, boot sound", "asusctl armoury set boot_sound 0", boom(), "Command failed: boom", |toast| {
            let win = ExtraWindow::new(None, toast);
            win.boot_sound_switch.set_active(false);
            Box::new(win)
        });
        check("extra, charging mode", "asusctl armoury set charge_mode 2", boom(), "Command failed: boom", |toast| {
            let win = ExtraWindow::new(None, toast);
            press(&win.charge_mode_buttons, "Max Lifespan");
            Box::new(win)
        });
    }

    {
        let _t = nvidia_and_ppd_tools();
        let _s = sysfs_map(&[(PPT_PL1_PATH, "40"), (PPT_PL2_PATH, "60"), (NV_BOOST_PATH, "5"), (NV_TEMP_TARGET_PATH, "80")]);
        let fans = |write: &'static str, expected: &str, failure: CmdOut, act: fn(&FansPowerWindow)| {
            check(write, write, failure, expected, move |toast| {
                let win = FansPowerWindow::new(None, toast, make_cfg(), no_save());
                pump(150); // the NVIDIA probe and the power-plan read land and sync the controls
                act(&win);
                Box::new(win)
            });
        };
        let cmd = "Command failed: boom";
        fans("powerprofilesctl set power-saver", cmd, boom(), |w| w.power_mode_dropdown.set_selected(0));
        fans("asusctl armoury set ppt_pl1_spl 70", cmd, boom(), |w| w.pl1.scale.set_value(70.0));
        fans("asusctl armoury set ppt_pl2_sppt 100", cmd, boom(), |w| w.pl2.scale.set_value(100.0));
        fans("asusctl armoury set nv_dynamic_boost 15", cmd, boom(), |w| w.boost.scale.set_value(15.0));
        fans("asusctl armoury set nv_temp_target 90", cmd, boom(), |w| w.temp_target.scale.set_value(90.0));
        fans("nvidia-settings -a [gpu:0]/GPUGraphicsClockOffset[3]=120", cmd, boom(), |w| w.core_clock.scale.set_value(120.0));
        fans("nvidia-settings -a [gpu:0]/GPUMemoryTransferRateOffset[3]=400", cmd, boom(), |w| w.mem_clock.scale.set_value(400.0));
        fans("nvidia-smi -pl 120", cmd, boom(), |w| w.gpu_power.scale.set_value(120.0));
        // nvidia-smi exits 0 when it refuses; the refusal text on stdout is what the user must read.
        let refusal = "Changing power management limit is not supported in current scope";
        let refused = CmdOut { ok: true, stdout: refusal.to_string(), stderr: String::new() };
        check("gpu power limit, refused with exit 0", "nvidia-smi -pl 120", refused, &format!("Command failed: {refusal}"), |toast| {
            let win = FansPowerWindow::new(None, toast, make_cfg(), no_save());
            pump(150);
            win.gpu_power.scale.set_value(120.0);
            Box::new(win)
        });
        clear_tool_overrides();
    }

    {
        let _a = asusd_says_static_red();
        check("lighting window", "asusctl aura effect", boom(), "Effect failed: boom", |toast| {
            let win = LightingWindow::new(None, toast, None);
            pump(50);
            win.effect_dropdown.set_selected(effect_index("breathe"));
            Box::new(win)
        });
    }
}

// guards: FR-23 - Fan curve apply, enable and factory reset each toast under their own heading
fn toasts__fan_curve_editor_reports_failed_writes() {
    let _s = sysfs_map(&[(PLATFORM_PROFILE_PATH, "balanced")]);
    let _curve = fake_capture(|argv| {
        (argv.len() == 4 && argv[1] == "fan-curve")
            .then(|| ok_out("[(fan: GPU, pwm: (36, 56, 89, 105, 117, 153, 168, 255), temp: (47, 51, 55, 60, 65, 70, 76, 97), enabled: false,)]"))
    });
    let editor = |toast: ToastFn| {
        let editor = FanCurveEditor::new("gpu", toast, Rc::new(|| "Balanced".to_string()));
        pump(100);
        editor
    };
    let boom = failed_out("boom");
    check("fan curve, apply", "asusctl fan-curve --mod-profile Balanced --fan gpu --data", boom.clone(), "Fan curve failed: boom", |toast| {
        let e = editor(toast);
        e.graph.set_points(&DEFAULT_CURVE);
        e.auto_apply();
        Box::new(e)
    });
    check("fan curve, enable", "asusctl fan-curve --mod-profile Balanced --enable-fan-curve true", boom.clone(), "Enable failed: boom", |toast| {
        let e = editor(toast);
        e.enable_switch.set_active(true);
        Box::new(e)
    });
    check("fan curve, factory reset", "asusctl fan-curve --mod-profile Balanced --fan gpu --default", boom, "Reset failed: boom", |toast| {
        let e = editor(toast);
        e.reset();
        Box::new(e)
    });
}

/// Every label text under `widget`: where a shown toast ends up.
fn label_texts(widget: &gtk::Widget, out: &mut Vec<String>) {
    if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        out.push(label.label().to_string());
    }
    let mut child = widget.first_child();
    while let Some(c) = child {
        label_texts(&c, out);
        child = c.next_sibling();
    }
}

// guards: FR-23 - the main window hands its toast overlay to every panel and to each companion it builds
fn toasts__popup_routes_panel_and_companion_toasts_to_its_overlay() {
    let _s = sysfs_map(&[
        (PLATFORM_PROFILE_PATH, "balanced"),
        (GPU_MUX_PATH, "1"), // hybrid, so Eco is a plain dgpu_disable write with no confirmation
        (DGPU_DISABLE_PATH, "0"),
        (KBD_BRIGHTNESS_PATH, "0"),
        (BATTERY_LIMIT_PATH, "100"),
        (BOOT_SOUND_PATH, "1"),
        (CHARGE_MODE_PATH, "1"),
        (PPT_PL1_PATH, "40"),
    ]);
    let _m = monitor_fake(240.0);
    let _state = fake_capture(|argv| (argv.get(2).map(String::as_str) == Some("is-enabled")).then(|| ok_out("disabled\n")));
    let _t = nvidia_and_ppd_tools();
    let _a = asusd_says_static_red();
    let (_log, _tool) = recording_capture(|_| failed_out("boom"));
    let shown = |popup: &RogPopup| {
        let mut texts = Vec::new();
        label_texts(popup.window.upcast_ref(), &mut texts);
        texts.iter().filter(|t| t.ends_with(": boom")).cloned().collect::<Vec<_>>()
    };
    // The overlay shows one toast at a time and queues the rest, so each source gets its own window.
    type Trigger = Box<dyn Fn(&Rc<RogPopup>)>;
    let sources: [(&str, Trigger); 9] = [
        ("Performance panel", Box::new(|p| press(&p.performance.buttons, "Performance"))),
        ("GPU panel", Box::new(|p| press(&p.gpu.buttons, "Eco"))),
        ("Screen panel", Box::new(|p| {
            pump(100); // the first sync reads the monitor and must not be mistaken for a click
            press(&p.screen.buttons, "60");
        })),
        ("Keyboard panel", Box::new(|p| press(&p.keyboard.buttons, "med"))),
        ("Battery panel", Box::new(|p| p.battery.oneshot_button.emit_clicked())),
        ("Startup panel", Box::new(|p| {
            pump(100);
            p.startup.switch.set_active(true);
        })),
        ("Extra window", Box::new(|p| p.ensure_extra().charge_mode_buttons[2].1.set_active(true))),
        ("Lighting window", Box::new(|p| p.ensure_lighting().effect_dropdown.set_selected(effect_index("breathe")))),
        ("Fans and Power window", Box::new(|p| p.ensure_fans().pl1.scale.set_value(70.0))),
    ];
    for (source, trigger) in sources {
        let popup = RogPopup::new(None, make_cfg(), no_save(), on_ac(), Rc::new(|| {}));
        assert!(shown(&popup).is_empty(), "{source}: a toast was already showing: {:?}", shown(&popup));
        trigger(&popup);
        pump_until(|| !shown(&popup).is_empty());
        assert_eq!(shown(&popup).len(), 1, "{source}: the failure never reached the main window's toast overlay: {:?}", shown(&popup));
    }
    clear_tool_overrides();
}

// guards: FR-23 - adding a failure toast to src/ means adding its case to this file
fn toasts__every_failure_site_in_src_has_a_case() {
    let mut sites = 0;
    for entry in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/src")).expect("read src").flatten() {
        if entry.file_name() == "widgets.rs" {
            continue; // where failure_text is defined
        }
        let text = std::fs::read_to_string(entry.path()).expect("read source");
        sites += text.matches("failure_text(").count();
    }
    assert_eq!(
        sites, FAILURE_SITES,
        "src/ now has {sites} failure_text call sites, these tests cover {FAILURE_SITES}: add a case that makes the write fail and asserts the toast, then update FAILURE_SITES"
    );
}
