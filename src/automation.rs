//! Watches UPower's `OnBattery` property (system bus) and re-applies whichever automation is due
//! on every AC <-> battery transition, and once, silently, at startup.
//!
//! This matches G-Helper's real model: performance profile and keyboard level are implicit,
//! always-on, remembered once per power source (no opt-in); GPU "Optimized" and Screen "Auto" are
//! the two settings with a genuine opt-in, chosen among each setting's own normal options.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::gio::{self, prelude::*};
use gtk::glib;

use crate::config::{self, Cfg};
use crate::hw::*;
use crate::sys::DoneCb;

pub struct AutomationManager {
    cfg: Cfg,
    refresh_ui: Rc<dyn Fn()>,
    on_battery: Cell<Option<bool>>,
    /// GLib keeps the shared bus connection (and with it our subscription) alive only while
    /// somebody holds a reference, so this one must.
    _system_bus: RefCell<Option<gio::DBusConnection>>,
}

impl AutomationManager {
    /// Starts watching and applies the remembered automation once `OnBattery` is known. Without
    /// UPower there is nothing to automate and nothing is applied.
    pub fn start(cfg: Cfg, refresh_ui: Rc<dyn Fn()>) -> Rc<Self> {
        let this = Self::new(cfg, refresh_ui);
        if let Ok(bus) = gio::bus_get_sync(gio::BusType::System, gio::Cancellable::NONE) {
            let weak = Rc::downgrade(&this);
            // The subscription lives as long as the connection (the process); never unsubscribed.
            let _subscription = bus.signal_subscribe(
                None,
                Some("org.freedesktop.DBus.Properties"),
                Some("PropertiesChanged"),
                Some("/org/freedesktop/UPower"),
                None,
                gio::DBusSignalFlags::NONE,
                move |_, _, _, _, _, params| {
                    let changed = params.child_value(1);
                    let Some(new_state) = glib::VariantDict::new(Some(&changed)).lookup::<bool>("OnBattery").ok().flatten() else { return };
                    if let Some(s) = weak.upgrade() {
                        if s.on_battery.get() == Some(new_state) {
                            return;
                        }
                        s.on_battery.set(Some(new_state));
                        glib::idle_add_local_once(move || s.apply_all(true));
                    }
                },
            );
            let weak = Rc::downgrade(&this);
            bus.call(
                Some("org.freedesktop.UPower"),
                "/org/freedesktop/UPower",
                "org.freedesktop.DBus.Properties",
                "Get",
                Some(&("org.freedesktop.UPower", "OnBattery").to_variant()),
                None,
                gio::DBusCallFlags::NONE,
                5000,
                gio::Cancellable::NONE,
                move |reply| {
                    let state = reply.ok().and_then(|r| r.child_value(0).as_variant()).and_then(|v| v.get::<bool>());
                    if let (Some(s), Some(state)) = (weak.upgrade(), state) {
                        // A transition signal that beat the reply wins: it is newer.
                        if s.on_battery.get().is_none() {
                            s.on_battery.set(Some(state));
                            // Without this, saved automation would not apply until the next
                            // transition: a fresh login would sit on whatever raw state the
                            // hardware had. Silent: nothing actually "transitioned" here.
                            s.apply_all(false);
                        }
                    }
                },
            );
            *this._system_bus.borrow_mut() = Some(bus);
        }
        this
    }

    /// A manager that is not (yet) connected to UPower; the power source is set by hand.
    pub fn new(cfg: Cfg, refresh_ui: Rc<dyn Fn()>) -> Rc<Self> {
        Rc::new(Self { cfg, refresh_ui, on_battery: Cell::new(None), _system_bus: RefCell::new(None) })
    }

    /// `None` until UPower has answered (or when it is unavailable).
    pub fn on_battery(&self) -> Option<bool> {
        self.on_battery.get()
    }

    pub fn set_on_battery(&self, value: Option<bool>) {
        self.on_battery.set(value);
    }

    fn remembered(&self, key: &str, valid: &[&str]) -> Option<String> {
        config::get_str(&self.cfg, "automation", key).filter(|v| valid.contains(&v.as_str()))
    }

    pub fn apply_all(self: &Rc<Self>, notify: bool) {
        let Some(on_battery) = self.on_battery.get() else { return };
        let source = if on_battery { "battery" } else { "ac" };

        if let Some(profile) = self.remembered(&format!("profile_{source}"), &PROFILES) {
            let done = notify.then(|| -> DoneCb {
                let label = profile_label(&profile).to_string();
                Box::new(move |ok, _| {
                    if ok {
                        notify_osd("Performance mode", &label);
                    }
                })
            });
            apply_profile(&profile, done);
        }

        let this = self.clone();
        let rest = move || {
            if config::get_bool(&this.cfg, "automation", "screen_auto") {
                apply_screen_preset(if on_battery { "auto-battery" } else { "auto-ac" }, None);
            }
            if let Some(level) = this.remembered(&format!("kbd_{source}"), &BRIGHTNESS_LEVELS) {
                apply_kbd_level(&level, None);
            }
            if notify {
                notify_osd(if on_battery { "On battery" } else { "On AC power" }, "Automation applied");
            }
            (this.refresh_ui)();
        };

        if !config::get_bool(&self.cfg, "automation", "gpu_optimized") {
            rest();
        } else if gpu_mode_needs_reboot("Optimized") {
            // A MUX change only takes effect after a reboot and must be confirmed in the GPU
            // panel; a background component has no dialog, so it never changes the MUX itself.
            if notify {
                notify_osd("Optimized GPU mode", "Leaving Ultimate needs a reboot: switch in ROG Control to confirm");
            }
            rest();
        } else if on_battery {
            // Mirrors G-Helper's IsUsedGPU() guard: do not yank the dGPU out from under an active
            // render just because the power source changed.
            gpu_in_heavy_use(move |heavy| {
                if heavy {
                    if notify {
                        notify_osd("Eco Mode", "GPU is in heavy use, leaving dGPU on");
                    }
                } else {
                    apply_optimized(true, notify);
                }
                rest();
            });
        } else {
            apply_optimized(false, notify);
            rest();
        }
    }
}

/// apply_gpu_mode can refuse (e.g. an external display wired straight to the dGPU); there is no
/// toast here, only the notification.
fn apply_optimized(on_battery: bool, notify: bool) {
    let done: DoneCb = Box::new(move |ok, err| {
        if !ok && notify {
            notify_osd("Optimized GPU mode", &err);
        }
    });
    apply_gpu_mode("Optimized", on_battery, Some(done));
}
