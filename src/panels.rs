//! The main window's panels: Performance, GPU, Screen, Keyboard, Battery, Startup and the footer.
//!
//! Each panel is a vertical box that mirrors hardware state in `refresh()` and applies user input
//! through the shared apply functions in [`crate::hw`]. While a panel syncs widgets from
//! hardware its `suppress` flag is raised so the resulting change signals are not mistaken for a
//! user action. Anything that shells out runs off the main thread.

use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use serde_json::json;

use crate::config::{self, Cfg, SaveCb};
use crate::hw::*;
use crate::sys::{capitalize, py_round, read_sysfs, read_sysfs_int, run_capture, run_cmd, spawn_bg, DoneCb};
use crate::theme::{gdk_rgba_to_hex, hex_to_gdk_rgba, theme_primary_hex, theme_secondary_hex};
use crate::widgets::*;

fn vbox(spacing: i32) -> gtk::Box {
    gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(spacing).build()
}

fn button_row() -> gtk::Box {
    gtk::Box::builder().spacing(6).homogeneous(true).hexpand(true).build()
}

/// Reports a failed command as a toast; ignores success.
fn report_cb(toast: &ToastCb) -> DoneCb {
    let toast = toast.clone();
    Box::new(move |ok, err| {
        if !ok {
            toast(&failure_text("Command failed", &err));
        }
    })
}

fn temp_text(millideg: Option<i64>) -> String {
    match millideg.filter(|t| *t != 0) {
        Some(t) => format!("{:.0}°C", t as f64 / 1000.0),
        None => "--".to_string(),
    }
}

fn rpm_text(rpm: Option<i64>) -> String {
    rpm.map_or_else(|| "--".to_string(), |r| format!("{r} RPM"))
}

// ---------------------------------------------------------------------------
// Performance
// ---------------------------------------------------------------------------

pub struct PerformancePanel {
    pub root: gtk::Box,
    pub header: SectionHeader,
    pub buttons: Vec<(&'static str, gtk::ToggleButton)>,
    pub fans_button: gtk::Button,
    suppress: Suppress,
    toast: ToastCb,
    cfg: Cfg,
    save: SaveCb,
    on_battery: Rc<dyn Fn() -> bool>,
}

impl PerformancePanel {
    pub fn new(
        toast: ToastCb,
        cfg: Cfg,
        save: SaveCb,
        on_battery: Rc<dyn Fn() -> bool>,
        open_fans: Rc<dyn Fn()>,
    ) -> Rc<Self> {
        let root = vbox(6);
        let header = section_header("speedometer-symbolic", "Performance Mode");
        root.append(&header.root);

        let row = button_row();
        let hint = hint_label();
        let mut buttons = Vec::new();
        let mut group: Option<gtk::ToggleButton> = None;
        for name in PROFILES {
            let (btn, _) = mode_toggle(profile_icon(name), profile_label(name), Some(profile_accent_class(name)), group.as_ref());
            group = group.or_else(|| Some(btn.clone()));
            add_hover_hint(&btn, &hint, profile_hint(name));
            row.append(&btn);
            buttons.push((name, btn));
        }
        let fans_button = mode_action_button("weather-windy-symbolic", "Fans + Power", "rog-accent-custom");
        fans_button.connect_clicked(move |_| open_fans());
        add_hover_hint(&fans_button, &hint, "Custom power limits and fan curves per mode");
        row.append(&fans_button);
        root.append(&row);
        root.append(&hint);

        let this = Rc::new(Self { root, header, buttons, fans_button, suppress: Suppress::new(false), toast, cfg, save, on_battery });
        for (name, btn) in &this.buttons {
            let (weak, name) = (Rc::downgrade(&this), *name);
            btn.connect_toggled(move |b| {
                if let Some(s) = weak.upgrade() {
                    s.on_profile_toggled(b, name);
                }
            });
        }
        this.refresh();
        this
    }

    pub fn refresh(&self) {
        self.suppress.during(|| {
            let current = capitalize(&read_sysfs(Some(PLATFORM_PROFILE_PATH)).unwrap_or_else(|| "balanced".to_string()));
            if let Some((_, btn)) = self.buttons.iter().find(|(n, _)| *n == current) {
                btn.set_active(true);
            }
            let hw = hwmon();
            let temp = temp_text(read_sysfs_int(hw.cpu_temp.as_deref()));
            let fan = rpm_text(read_sysfs_int(hw.cpu_fan.as_deref()));
            self.header.stat.set_label(&format!("CPU: {temp}  Fan: {fan}"));
        });
    }

    fn on_profile_toggled(&self, btn: &gtk::ToggleButton, name: &str) {
        if self.suppress.get() || !btn.is_active() {
            return;
        }
        let key = if (self.on_battery)() { "profile_battery" } else { "profile_ac" };
        config::set(&self.cfg, "automation", key, json!(name));
        (self.save)();
        apply_profile(name, Some(report_cb(&self.toast)));
    }
}

// ---------------------------------------------------------------------------
// GPU
// ---------------------------------------------------------------------------

pub struct GpuPanel {
    pub root: gtk::Box,
    pub header: SectionHeader,
    pub buttons: Vec<(&'static str, gtk::ToggleButton)>,
    pub reboot_notice: gtk::Label,
    suppress: Suppress,
    last_temp: RefCell<String>,
    toast: ToastCb,
    cfg: Cfg,
    save: SaveCb,
    on_battery: Rc<dyn Fn() -> bool>,
    confirm: RefCell<ConfirmFn>,
}

impl GpuPanel {
    pub fn new(
        toast: ToastCb,
        cfg: Cfg,
        save: SaveCb,
        on_battery: Rc<dyn Fn() -> bool>,
        get_root: Rc<dyn Fn() -> Option<gtk::Widget>>,
    ) -> Rc<Self> {
        let root = vbox(6);
        let header = section_header("video-display-symbolic", "GPU Mode");
        root.append(&header.root);

        let row = button_row();
        let hint = hint_label();
        let mut buttons = Vec::new();
        let mut group: Option<gtk::ToggleButton> = None;
        for name in GPU_MODES {
            let (btn, _) = mode_toggle(gpu_icon(name), name, Some(gpu_accent_class(name)), group.as_ref());
            group = group.or_else(|| Some(btn.clone()));
            add_hover_hint(&btn, &hint, gpu_hint(name));
            row.append(&btn);
            buttons.push((name, btn));
        }
        root.append(&row);
        root.append(&hint);

        let reboot_notice = gtk::Label::new(Some(""));
        reboot_notice.add_css_class("rog-subtitle");
        reboot_notice.set_halign(gtk::Align::Start);
        root.append(&reboot_notice);

        // The real confirmation: a modal "Reboot now?" dialog over the main window.
        let confirm: ConfirmFn = Rc::new(move |heading, body, ok, on_confirm| {
            confirm_dialog(get_root().as_ref(), heading, body, ok, on_confirm, true);
        });
        let this = Rc::new(Self {
            root,
            header,
            buttons,
            reboot_notice,
            suppress: Suppress::new(false),
            last_temp: RefCell::new("--".to_string()),
            toast,
            cfg,
            save,
            on_battery,
            confirm: RefCell::new(confirm),
        });
        for (name, btn) in &this.buttons {
            let (weak, name) = (Rc::downgrade(&this), *name);
            btn.connect_toggled(move |b| {
                if let Some(s) = weak.upgrade() {
                    s.on_gpu_toggled(b, name);
                }
            });
        }
        this.refresh();
        this
    }

    /// Test seam: replace the confirmation dialog.
    pub fn set_confirm(&self, confirm: ConfirmFn) {
        *self.confirm.borrow_mut() = confirm;
    }

    pub fn refresh(self: &Rc<Self>) {
        self.suppress.during(|| {
            let mode = current_gpu_mode(&self.cfg);
            if let Some((_, btn)) = self.buttons.iter().find(|(n, _)| *n == mode) {
                btn.set_active(true);
            }
            let pending = read_sysfs(Some(PENDING_REBOOT_PATH)).unwrap_or_else(|| "0".to_string()) == "1";
            self.reboot_notice.set_label(if pending { "Reboot required to apply a pending GPU mode change" } else { "" });
            // Fan RPM is a cheap sysfs read and updates at once; the temperature needs nvidia-smi
            // and arrives asynchronously. The label is composed from the last known temperature
            // rather than blanked to a placeholder first, which used to make every 2 s poll
            // flicker "GPU: --".
            self.update_stat_label();
        });
        self.refresh_gpu_temp();
    }

    fn update_stat_label(&self) {
        let fan = rpm_text(read_sysfs_int(hwmon().gpu_fan.as_deref()));
        self.header.stat.set_label(&format!("GPU: {}  Fan: {fan}", self.last_temp.borrow()));
    }

    fn refresh_gpu_temp(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        spawn_bg(
            || {
                run_capture(&["nvidia-smi", "--query-gpu=temperature.gpu", "--format=csv,noheader"], Duration::from_secs(5))
                    .map(|o| o.stdout.trim().to_string())
                    .unwrap_or_default()
            },
            move |temp| {
                if let Some(s) = weak.upgrade() {
                    *s.last_temp.borrow_mut() = if temp.is_empty() { "--".to_string() } else { format!("{temp}°C") };
                    s.update_stat_label();
                }
            },
        );
    }

    fn on_gpu_toggled(self: &Rc<Self>, btn: &gtk::ToggleButton, name: &'static str) {
        if self.suppress.get() || !btn.is_active() {
            return;
        }
        if gpu_mode_needs_reboot(name) {
            let body = if name == "Ultimate" { "Ultimate Mode requires restart" } else { "Switching off Ultimate Mode requires restart" };
            let weak = Rc::downgrade(self);
            let confirm = self.confirm.borrow().clone();
            confirm(
                "Reboot now?",
                body,
                "Reboot",
                Box::new(move || {
                    if let Some(s) = weak.upgrade() {
                        s.apply(name, true);
                    }
                }),
            );
            // Nothing applied yet: resync the buttons to the real (unchanged) state.
            let weak = Rc::downgrade(self);
            glib::idle_add_local_once(move || {
                if let Some(s) = weak.upgrade() {
                    s.refresh();
                }
            });
        } else {
            self.apply(name, false);
        }
    }

    /// Applies `name`; with `reboot` (only ever after an explicit confirmation) reboots once the
    /// change has succeeded.
    pub fn apply(self: &Rc<Self>, name: &str, reboot: bool) {
        config::set(&self.cfg, "automation", "gpu_optimized", json!(name == "Optimized"));
        (self.save)();
        let weak: Weak<Self> = Rc::downgrade(self);
        let done: DoneCb = Box::new(move |ok, err| {
            if let Some(s) = weak.upgrade() {
                if !ok {
                    (s.toast)(&failure_text("GPU mode failed", &err));
                } else if reboot {
                    run_cmd(&["systemctl", "reboot"], None);
                }
                s.refresh();
            }
        });
        apply_gpu_mode(name, (self.on_battery)(), Some(done));
    }
}

// ---------------------------------------------------------------------------
// Screen
// ---------------------------------------------------------------------------

pub struct ScreenPanel {
    pub root: gtk::Box,
    pub buttons: Vec<(&'static str, gtk::ToggleButton)>,
    max_label: gtk::Label,
    suppress: Suppress,
    toast: ToastCb,
    cfg: Cfg,
    save: SaveCb,
}

impl ScreenPanel {
    pub fn new(toast: ToastCb, cfg: Cfg, save: SaveCb) -> Rc<Self> {
        let root = vbox(6);
        root.append(&section_header("preferences-desktop-display-symbolic", "Screen").root);

        let row = button_row();
        let hint = hint_label();
        let mut buttons = Vec::new();
        let mut group: Option<gtk::ToggleButton> = None;
        let mut max_label = None;
        for (key, label) in [("auto", "Auto"), ("60", "60Hz"), ("max", "60Hz + OD")] {
            let icon = if key == "auto" { "emblem-synchronizing-symbolic" } else { "video-display-symbolic" };
            let (btn, text) = mode_toggle(icon, label, None, group.as_ref());
            group = group.or_else(|| Some(btn.clone()));
            add_hover_hint(&btn, &hint, screen_hint(key));
            row.append(&btn);
            if key == "max" {
                max_label = Some(text);
            }
            buttons.push((key, btn));
        }
        root.append(&row);
        root.append(&hint);

        let this = Rc::new(Self {
            root,
            buttons,
            max_label: max_label.unwrap_or_default(),
            suppress: Suppress::new(false),
            toast,
            cfg,
            save,
        });
        for (key, btn) in &this.buttons {
            let (weak, key) = (Rc::downgrade(&this), *key);
            btn.connect_toggled(move |b| {
                if let Some(s) = weak.upgrade() {
                    s.on_toggled(b, key);
                }
            });
        }
        this.sync(true);
        this
    }

    fn select(&self, key: &str) {
        if let Some((_, btn)) = self.buttons.iter().find(|(k, _)| *k == key) {
            self.suppress.during(|| btn.set_active(true));
        }
    }

    /// Mirrors the active refresh preset; `hyprctl` is asked in the background, so the button
    /// label ("<max>Hz + OD") and the selection settle a moment after construction.
    fn sync(self: &Rc<Self>, update_label: bool) {
        let auto = config::get_bool(&self.cfg, "automation", "screen_auto");
        if auto {
            self.select("auto");
        }
        let weak = Rc::downgrade(self);
        current_monitor(move |monitor| {
            let Some(s) = weak.upgrade() else { return };
            if update_label {
                s.max_label.set_label(&format!("{}Hz + OD", max_refresh_rate(monitor.as_ref())));
            }
            if !auto && !config::get_bool(&s.cfg, "automation", "screen_auto") {
                let hz = monitor.map_or(0, |m| py_round(m["refreshRate"].as_f64().unwrap_or(0.0)));
                s.select(if hz <= 60 { "60" } else { "max" });
            }
        });
    }

    pub fn refresh(self: &Rc<Self>) {
        self.sync(false);
    }

    fn on_toggled(&self, btn: &gtk::ToggleButton, key: &str) {
        if self.suppress.get() || !btn.is_active() {
            return;
        }
        config::set(&self.cfg, "automation", "screen_auto", json!(key == "auto"));
        (self.save)();
        if key == "auto" {
            (self.toast)("Screen: auto (60Hz on battery, max Hz when plugged)");
            return;
        }
        apply_screen_preset(key, Some(report_cb(&self.toast)));
    }
}

// ---------------------------------------------------------------------------
// Keyboard
// ---------------------------------------------------------------------------

pub fn color_picker_button(initial_hex: &str) -> gtk::ColorDialogButton {
    // A native colour picker (full RGB, not a fixed palette): G-Helper's own keyboard colour
    // control is a button that opens a colour dialog, and this keyboard is a single zone.
    let dialog = gtk::ColorDialog::builder().with_alpha(false).build();
    let btn = gtk::ColorDialogButton::new(Some(dialog));
    btn.set_rgba(&hex_to_gdk_rgba(initial_hex));
    btn
}

pub struct KeyboardPanel {
    pub root: gtk::Box,
    pub buttons: Vec<(&'static str, gtk::ToggleButton)>,
    pub effect_dropdown: gtk::DropDown,
    pub colour_btn: gtk::ColorDialogButton,
    pub extra_button: gtk::Button,
    suppress: Suppress,
    apply_debouncer: Debouncer,
    toast: ToastCb,
    cfg: Cfg,
    save: SaveCb,
    on_battery: Rc<dyn Fn() -> bool>,
}

impl KeyboardPanel {
    pub fn new(
        toast: ToastCb,
        cfg: Cfg,
        save: SaveCb,
        on_battery: Rc<dyn Fn() -> bool>,
        open_lighting: Rc<dyn Fn()>,
    ) -> Rc<Self> {
        let root = vbox(6);
        root.append(&section_header("input-keyboard-symbolic", "Laptop Keyboard").root);

        let row = button_row();
        let mut buttons = Vec::new();
        let mut group: Option<gtk::ToggleButton> = None;
        for (level, label) in BRIGHTNESS_LEVELS.iter().zip(BRIGHTNESS_LABELS) {
            let btn = gtk::ToggleButton::builder().label(label).hexpand(true).build();
            btn.add_css_class("rog-mode-btn");
            if let Some(g) = &group {
                btn.set_group(Some(g));
            } else {
                group = Some(btn.clone());
            }
            row.append(&btn);
            buttons.push((*level, btn));
        }
        root.append(&row);

        // Compact RGB quick controls, as in G-Helper's main window: effect combo, colour swatch
        // and an Extra button for direction/speed/second colour.
        let rgb_row = gtk::Box::builder().spacing(6).hexpand(true).build();
        let effect_dropdown = gtk::DropDown::from_strings(&aura_effect_names());
        effect_dropdown.set_hexpand(true);
        let colour_btn = color_picker_button(&theme_primary_hex());
        let extra_button = gtk::Button::with_label("Extra");
        extra_button.add_css_class("rog-pill");
        extra_button.connect_clicked(move |_| open_lighting());
        rgb_row.append(&effect_dropdown);
        rgb_row.append(&colour_btn);
        rgb_row.append(&extra_button);
        root.append(&rgb_row);

        let this = Rc::new(Self {
            root,
            buttons,
            effect_dropdown,
            colour_btn,
            extra_button,
            // Raised before any signal can fire: GTK emits notify::selected during construction.
            suppress: Suppress::new(true),
            apply_debouncer: Debouncer::new(Debouncer::QUIET_MS),
            toast,
            cfg,
            save,
            on_battery,
        });
        let weak = Rc::downgrade(&this);
        this.apply_debouncer.bind(move || {
            if let Some(s) = weak.upgrade() {
                s.apply_effect();
            }
        });
        for (level, btn) in &this.buttons {
            let (weak, level) = (Rc::downgrade(&this), *level);
            btn.connect_toggled(move |b| {
                if let Some(s) = weak.upgrade() {
                    s.on_brightness_toggled(b, level);
                }
            });
        }
        let weak = Rc::downgrade(&this);
        this.effect_dropdown.connect_selected_notify(move |_| {
            if let Some(s) = weak.upgrade() {
                s.on_changed();
            }
        });
        let weak = Rc::downgrade(&this);
        this.colour_btn.connect_rgba_notify(move |_| {
            if let Some(s) = weak.upgrade() {
                s.on_changed();
            }
        });
        this.refresh();
        this.suppress.set(false);
        this
    }

    pub fn refresh(&self) {
        self.suppress.during(|| {
            let idx = read_sysfs_int(Some(KBD_BRIGHTNESS_PATH)).unwrap_or(0);
            let level = usize::try_from(idx).ok().and_then(|i| BRIGHTNESS_LEVELS.get(i)).copied().unwrap_or("off");
            if let Some((_, btn)) = self.buttons.iter().find(|(l, _)| *l == level) {
                btn.set_active(true);
            }
            // Reflect whatever is actually active on the keyboard; colours fall back to the
            // desktop's Material You palette only when asusd has no state to report at all.
            let state = read_aura_state();
            let effects = aura_effect_names();
            let effect = state.as_ref().map_or("static", |s| s.effect.as_str());
            if let Some(i) = effects.iter().position(|e| *e == effect) {
                self.effect_dropdown.set_selected(i as u32);
            }
            let colour1 = state.map_or_else(theme_primary_hex, |s| s.colour1);
            self.colour_btn.set_rgba(&hex_to_gdk_rgba(&colour1));
        });
    }

    fn on_brightness_toggled(&self, btn: &gtk::ToggleButton, level: &str) {
        if self.suppress.get() || !btn.is_active() {
            return;
        }
        // Implicit automation, no opt-in (as G-Helper): remember this level for the current power
        // source; the automation manager restores it on transitions.
        let key = if (self.on_battery)() { "kbd_battery" } else { "kbd_ac" };
        config::set(&self.cfg, "automation", key, json!(level));
        (self.save)();
        apply_kbd_level(level, Some(report_cb(&self.toast)));
    }

    fn on_changed(&self) {
        if !self.suppress.get() {
            self.apply_debouncer.trigger();
        }
    }

    fn apply_effect(&self) {
        let effects = aura_effect_names();
        let Some(effect) = effects.get(self.effect_dropdown.selected() as usize).copied() else { return };
        let colour1 = gdk_rgba_to_hex(&self.colour_btn.rgba());
        // This compact row only has effect + colour1; colour2/direction/speed come from what is
        // actually active (set via the Lighting window) so picking an effect here never
        // overwrites them with defaults.
        let state = read_aura_state();
        let colour2 = state.as_ref().map_or_else(theme_secondary_hex, |s| s.colour2.clone());
        let direction = state.as_ref().map(|s| s.direction.as_str()).filter(|d| DIRECTIONS.contains(d)).unwrap_or("right");
        let speed = state.as_ref().map(|s| s.speed.as_str()).filter(|s| SPEEDS.contains(s)).unwrap_or("med");
        let cmd = build_aura_cmd(effect, &colour1, &colour2, direction, speed);
        let toast = self.toast.clone();
        run_cmd_vec(&cmd, Some(Box::new(move |ok, err| {
            if !ok {
                toast(&failure_text("Effect failed", &err));
            }
        })));
    }
}

// ---------------------------------------------------------------------------
// Battery
// ---------------------------------------------------------------------------

pub struct BatteryPanel {
    pub root: gtk::Box,
    pub header: SectionHeader,
    pub scale: gtk::Scale,
    pub oneshot_button: gtk::Button,
    suppress: Suppress,
    limit_debouncer: Debouncer,
    toast: ToastCb,
}

impl BatteryPanel {
    pub fn new(toast: ToastCb) -> Rc<Self> {
        let root = vbox(6);
        let header = section_header("battery-good-symbolic", "Battery Charge Limit");
        root.append(&header.root);

        let row = gtk::Box::builder().spacing(8).build();
        // G-Helper's slider is 40-100, but asusctl's real supported range on this hardware is
        // 20-100: that backend fact wins over copying a Windows-specific UI range, which had
        // silently clamped a deliberately set 20 % to 40.
        let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 20.0, 100.0, 5.0);
        scale.set_draw_value(true);
        scale.set_value_pos(gtk::PositionType::Right);
        scale.set_hexpand(true);
        let oneshot_button = gtk::Button::with_label("100%");
        oneshot_button.add_css_class("rog-pill");
        row.append(&scale);
        row.append(&oneshot_button);
        root.append(&row);

        let this = Rc::new(Self {
            root,
            header,
            scale,
            oneshot_button,
            suppress: Suppress::new(false),
            limit_debouncer: Debouncer::new(Debouncer::QUIET_MS),
            toast,
        });
        let weak = Rc::downgrade(&this);
        this.limit_debouncer.bind(move || {
            if let Some(s) = weak.upgrade() {
                s.apply_limit();
            }
        });
        let weak = Rc::downgrade(&this);
        this.scale.connect_value_changed(move |scale| {
            if let Some(s) = weak.upgrade() {
                s.on_changed(scale);
            }
        });
        let weak = Rc::downgrade(&this);
        this.oneshot_button.connect_clicked(move |_| {
            if let Some(s) = weak.upgrade() {
                run_cmd(&["asusctl", "battery", "oneshot"], Some(s.result_cb()));
            }
        });
        let motion = gtk::EventControllerMotion::new();
        let weak = Rc::downgrade(&this);
        motion.connect_enter(move |_, _, _| {
            if let (Some(s), Some(health)) = (weak.upgrade(), battery_health_text()) {
                s.oneshot_button.set_tooltip_text(Some(&health));
            }
        });
        let weak = Rc::downgrade(&this);
        motion.connect_leave(move |_| {
            if let Some(s) = weak.upgrade() {
                s.show_limit_label();
            }
        });
        this.oneshot_button.add_controller(motion);
        this.refresh();
        this
    }

    fn show_limit_label(&self) {
        self.oneshot_button.set_label("100%");
        self.oneshot_button.set_tooltip_text(Some("One time charge to 100%"));
    }

    pub fn refresh(&self) {
        self.suppress.during(|| {
            let limit = read_sysfs_int(Some(BATTERY_LIMIT_PATH)).unwrap_or(100);
            self.scale.set_value(limit as f64);
            self.header.title.set_label(&format!("Battery Charge Limit: {limit}%"));
            self.header.stat.set_label(&battery_charging_text());
            self.show_limit_label();
        });
    }

    fn on_changed(&self, scale: &gtk::Scale) {
        if self.suppress.get() {
            return;
        }
        self.header.title.set_label(&format!("Battery Charge Limit: {}%", scale.value() as i64));
        self.limit_debouncer.trigger();
    }

    fn apply_limit(self: &Rc<Self>) {
        let value = self.scale.value() as i64;
        run_cmd(&["asusctl", "battery", "limit", &value.to_string()], Some(self.result_cb()));
    }

    /// Failure toasts; success re-reads the real limit and status.
    fn result_cb(self: &Rc<Self>) -> DoneCb {
        let weak = Rc::downgrade(self);
        Box::new(move |ok, err| {
            if let Some(s) = weak.upgrade() {
                if ok {
                    s.refresh();
                } else {
                    (s.toast)(&failure_text("Command failed", &err));
                }
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Startup
// ---------------------------------------------------------------------------

pub struct StartupPanel {
    pub root: gtk::Box,
    pub switch: gtk::Switch,
    suppress: Rc<Suppress>,
    toast: ToastCb,
}

impl StartupPanel {
    pub fn new(toast: ToastCb) -> Rc<Self> {
        let suppress = Rc::new(Suppress::new(false));
        let handler_suppress = suppress.clone();
        let handler_toast = toast.clone();
        // This is a checkbox in G-Helper's designer; a switch reads better in this app's control
        // language, with the same on/off semantics.
        let row = switch_row("Run on Startup", move |_, state| {
            if !handler_suppress.get() {
                let verb = if state { "enable" } else { "disable" };
                run_cmd(&["systemctl", "--user", verb, "rog-control.service"], Some(report_cb(&handler_toast)));
            }
            glib::Propagation::Proceed
        });
        let root = vbox(4);
        root.append(&row.root);
        let this = Rc::new(Self { root, switch: row.switch, suppress, toast });
        this.refresh();
        this
    }

    pub fn refresh(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        spawn_bg(
            || run_capture(&["systemctl", "--user", "is-enabled", "rog-control.service"], Duration::from_secs(5)).ok(),
            move |out| {
                if let (Some(s), Some(out)) = (weak.upgrade(), out) {
                    s.suppress.during(|| s.switch.set_active(out.stdout.trim() == "enabled"));
                }
            },
        );
    }

    /// Test seam: the toast sink this panel reports failures to.
    pub fn toast(&self) -> &ToastCb {
        &self.toast
    }
}

// ---------------------------------------------------------------------------
// Footer
// ---------------------------------------------------------------------------

pub struct FooterPanel {
    pub root: gtk::Box,
    pub firmware_button: gtk::Button,
    pub quit_button: gtk::Button,
}

impl FooterPanel {
    pub fn new(on_extra: Rc<dyn Fn()>, on_quit: Rc<dyn Fn()>) -> Self {
        let root = vbox(4);
        let version = gtk::Label::new(Some("ROG Control · asusctl backend"));
        version.add_css_class("rog-subtitle");
        version.set_halign(gtk::Align::Start);
        root.append(&version);

        let row = button_row();
        let firmware_button = gtk::Button::with_label("Firmware");
        firmware_button.connect_clicked(move |_| on_extra());
        let quit_button = gtk::Button::with_label("Quit");
        quit_button.add_css_class("destructive-action");
        quit_button.connect_clicked(move |_| on_quit());
        row.append(&firmware_button);
        row.append(&quit_button);
        root.append(&row);
        Self { root, firmware_button, quit_button }
    }
}
