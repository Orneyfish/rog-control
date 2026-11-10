//! The "Extra" companion window, matching G-Helper's Extra popout. Only the settings with a real
//! Linux/asusctl backing live here (boot sound, charging mode); G-Helper's Windows-only items
//! have no safe equivalent on this stack and are left out rather than faked.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::hw::*;
use crate::sys::{read_sysfs_int, run_cmd};
use crate::widgets::*;

pub struct ExtraWindow {
    pub window: adw::Window,
    pub boot_sound_row: gtk::Box,
    pub boot_sound_switch: gtk::Switch,
    pub charge_mode_row: gtk::Box,
    pub charge_mode_buttons: Vec<(&'static str, gtk::ToggleButton)>,
    pub empty_notice: gtk::Label,
    suppress: Rc<Suppress>,
    toast: ToastCb,
}

impl ExtraWindow {
    pub fn new(app: Option<&adw::Application>, toast: ToastCb) -> Rc<Self> {
        let window = adw::Window::new();
        if let Some(app) = app {
            window.set_application(Some(app));
        }
        window.set_default_size(380, 320);
        window.set_title(Some("Extra"));

        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&adw::HeaderBar::builder().title_widget(&adw::WindowTitle::new("Extra", "")).build());
        window.set_content(Some(&toolbar_view));

        let outer = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).build();
        outer.set_margin_top(12);
        outer.set_margin_bottom(14);
        outer.set_margin_start(14);
        outer.set_margin_end(14);

        let suppress = Rc::new(Suppress::new(true));
        let (handler_suppress, handler_toast) = (suppress.clone(), toast.clone());
        let boot = switch_row("Boot (POST) sound", move |_, state| {
            if !handler_suppress.get() {
                let toast = handler_toast.clone();
                run_cmd(
                    &["asusctl", "armoury", "set", "boot_sound", if state { "1" } else { "0" }],
                    Some(Box::new(move |ok, err| {
                        if !ok {
                            toast(&failure_text("Command failed", &err));
                        }
                    })),
                );
            }
            glib::Propagation::Proceed
        });
        outer.append(&boot.root);

        outer.append(&section_label("Charging mode"));
        let charge_mode_row = gtk::Box::builder().spacing(6).homogeneous(true).hexpand(true).build();
        let mut charge_mode_buttons = Vec::new();
        let mut group: Option<gtk::ToggleButton> = None;
        for name in CHARGE_MODES {
            let btn = gtk::ToggleButton::builder().label(name).hexpand(true).build();
            if let Some(g) = &group {
                btn.set_group(Some(g));
            } else {
                group = Some(btn.clone());
            }
            charge_mode_row.append(&btn);
            charge_mode_buttons.push((name, btn));
        }
        outer.append(&charge_mode_row);

        let empty_notice = gtk::Label::new(Some("No additional firmware controls exposed on this laptop."));
        empty_notice.add_css_class("rog-empty");
        outer.append(&empty_notice);
        toolbar_view.set_content(Some(&outer));

        let this = Rc::new(Self {
            window,
            boot_sound_row: boot.root,
            boot_sound_switch: boot.switch,
            charge_mode_row,
            charge_mode_buttons,
            empty_notice,
            suppress,
            toast,
        });
        for (idx, (_, btn)) in this.charge_mode_buttons.iter().enumerate() {
            let weak = Rc::downgrade(&this);
            btn.connect_toggled(move |b| {
                if let Some(s) = weak.upgrade() {
                    s.on_charge_mode_toggled(b, idx);
                }
            });
        }
        this.suppress.set(false);
        this.refresh();
        this
    }

    pub fn refresh(&self) {
        self.suppress.during(|| {
            let mut any_shown = false;
            let boot_sound = read_sysfs_int(Some(BOOT_SOUND_PATH));
            self.boot_sound_row.set_visible(boot_sound.is_some());
            if let Some(v) = boot_sound {
                self.boot_sound_switch.set_active(v == 1);
                any_shown = true;
            }
            let charge_mode = read_sysfs_int(Some(CHARGE_MODE_PATH));
            self.charge_mode_row.set_visible(charge_mode.is_some());
            if let Some(mode) = charge_mode {
                if let Some((_, btn)) = usize::try_from(mode).ok().and_then(|i| self.charge_mode_buttons.get(i)) {
                    btn.set_active(true);
                    any_shown = true;
                }
            }
            self.empty_notice.set_visible(!any_shown);
        });
    }

    fn on_charge_mode_toggled(&self, btn: &gtk::ToggleButton, idx: usize) {
        if self.suppress.get() || !btn.is_active() {
            return;
        }
        let toast = self.toast.clone();
        run_cmd(
            &["asusctl", "armoury", "set", "charge_mode", &idx.to_string()],
            Some(Box::new(move |ok, err| {
                if !ok {
                    toast(&failure_text("Command failed", &err));
                }
            })),
        );
    }
}
