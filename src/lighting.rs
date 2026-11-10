//! The "Keyboard Lighting" companion window: full effect / colour / speed / direction editing,
//! as in G-Helper's keyboard popout (the main window only has an effect combo, a swatch and an
//! Extra button). This keyboard reports no basic zones, so effects apply to the whole keyboard as
//! one zone.

use std::rc::Rc;

use adw::prelude::*;

use crate::hw::*;
use crate::panels::color_picker_button;
use crate::sys::DoneCb;
use crate::theme::{gdk_rgba_to_hex, hex_to_gdk_rgba, theme_primary_hex, theme_secondary_hex};
use crate::widgets::*;

pub struct LightingWindow {
    pub window: adw::Window,
    pub effect_dropdown: gtk::DropDown,
    pub colour1_row: gtk::Box,
    pub colour1_btn: gtk::ColorDialogButton,
    pub colour2_row: gtk::Box,
    pub colour2_btn: gtk::ColorDialogButton,
    pub direction_row: gtk::Box,
    pub direction_dropdown: gtk::DropDown,
    pub speed_row: gtk::Box,
    pub speed_dropdown: gtk::DropDown,
    pub reset_button: gtk::Button,
    suppress: Suppress,
    apply_debouncer: Debouncer,
    toast: ToastCb,
    on_changed: Option<Rc<dyn Fn()>>,
}

fn labelled_row(label: &str) -> gtk::Box {
    let row = gtk::Box::builder().spacing(8).build();
    row.append(&gtk::Label::builder().label(label).hexpand(true).halign(gtk::Align::Start).build());
    row
}

impl LightingWindow {
    pub fn new(app: Option<&adw::Application>, toast: ToastCb, on_changed: Option<Rc<dyn Fn()>>) -> Rc<Self> {
        let window = adw::Window::new();
        if let Some(app) = app {
            window.set_application(Some(app));
        }
        window.set_default_size(380, 480);
        window.set_title(Some("Keyboard Lighting"));

        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&adw::HeaderBar::builder().title_widget(&adw::WindowTitle::new("Keyboard Lighting", "")).build());
        window.set_content(Some(&toolbar_view));

        let scroller = gtk::ScrolledWindow::new();
        scroller.set_vexpand(true);
        let outer = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).build();
        outer.set_margin_top(12);
        outer.set_margin_bottom(14);
        outer.set_margin_start(14);
        outer.set_margin_end(14);
        scroller.set_child(Some(&outer));

        outer.append(&section_label("Effect"));
        let effect_dropdown = gtk::DropDown::from_strings(&aura_effect_names());
        effect_dropdown.set_hexpand(true);
        outer.append(&effect_dropdown);

        let colour1_row = labelled_row("Colour");
        let colour1_btn = color_picker_button(&theme_primary_hex());
        colour1_row.append(&colour1_btn);
        outer.append(&colour1_row);

        let colour2_row = labelled_row("Second colour");
        let colour2_btn = color_picker_button(&theme_secondary_hex());
        colour2_row.append(&colour2_btn);
        outer.append(&colour2_row);

        let reset_button = gtk::Button::with_label("Reset colours to desktop theme");
        outer.append(&reset_button);

        let direction_row = labelled_row("Direction");
        let direction_dropdown = gtk::DropDown::from_strings(&DIRECTIONS);
        direction_row.append(&direction_dropdown);
        outer.append(&direction_row);

        let speed_row = labelled_row("Speed");
        let speed_dropdown = gtk::DropDown::from_strings(&SPEEDS);
        speed_dropdown.set_selected(1);
        speed_row.append(&speed_dropdown);
        outer.append(&speed_row);

        toolbar_view.set_content(Some(&scroller));

        let this = Rc::new(Self {
            window,
            effect_dropdown,
            colour1_row,
            colour1_btn,
            colour2_row,
            colour2_btn,
            direction_row,
            direction_dropdown,
            speed_row,
            speed_dropdown,
            reset_button,
            // Raised before any signal is wired: GTK can emit notify::selected / notify::rgba
            // while a widget is being built, and that must never apply a half-built effect.
            suppress: Suppress::new(true),
            apply_debouncer: Debouncer::new(Debouncer::QUIET_MS),
            toast,
            on_changed,
        });
        this.wire();
        this.refresh();
        this.suppress.set(false);
        this
    }

    fn wire(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.apply_debouncer.bind(move || {
            if let Some(s) = weak.upgrade() {
                s.do_apply();
            }
        });
        let weak = Rc::downgrade(self);
        self.effect_dropdown.connect_selected_notify(move |_| {
            if let Some(s) = weak.upgrade() {
                s.update_effect_fields();
                s.changed();
            }
        });
        let changed = |weak: std::rc::Weak<Self>| move || {
            if let Some(s) = weak.upgrade() {
                s.changed();
            }
        };
        let f = changed(Rc::downgrade(self));
        self.colour1_btn.connect_rgba_notify(move |_| f());
        let f = changed(Rc::downgrade(self));
        self.colour2_btn.connect_rgba_notify(move |_| f());
        let f = changed(Rc::downgrade(self));
        self.direction_dropdown.connect_selected_notify(move |_| f());
        let f = changed(Rc::downgrade(self));
        self.speed_dropdown.connect_selected_notify(move |_| f());
        let weak = Rc::downgrade(self);
        self.reset_button.connect_clicked(move |_| {
            if let Some(s) = weak.upgrade() {
                s.reset_to_theme();
            }
        });
    }

    pub fn refresh(&self) {
        self.suppress.during(|| {
            // Edits still waiting out their debounce are not in asusd yet; syncing from asusd
            // would replace them and do_apply() would send the old values.
            let state = if self.apply_debouncer.is_pending() { None } else { read_aura_state() };
            if let Some(state) = state {
                if let Some(i) = aura_effect_names().iter().position(|e| *e == state.effect) {
                    self.effect_dropdown.set_selected(i as u32);
                }
                self.colour1_btn.set_rgba(&hex_to_gdk_rgba(&state.colour1));
                self.colour2_btn.set_rgba(&hex_to_gdk_rgba(&state.colour2));
                if let Some(i) = DIRECTIONS.iter().position(|d| *d == state.direction) {
                    self.direction_dropdown.set_selected(i as u32);
                }
                if let Some(i) = SPEEDS.iter().position(|s| *s == state.speed) {
                    self.speed_dropdown.set_selected(i as u32);
                }
            }
            self.update_effect_fields();
        });
    }

    pub fn current_effect(&self) -> &'static str {
        aura_effect_names().get(self.effect_dropdown.selected() as usize).copied().unwrap_or("static")
    }

    fn update_effect_fields(&self) {
        let needs = effect_needs(self.current_effect());
        self.colour1_row.set_visible(needs.contains(&"colour1"));
        self.colour2_row.set_visible(needs.contains(&"colour2"));
        self.direction_row.set_visible(needs.contains(&"direction"));
        self.speed_row.set_visible(needs.contains(&"speed"));
    }

    /// If the active effect uses no colour (rainbow-cycle, rain, rainbow-wave: speed/direction
    /// only), setting the pickers would send nothing and the keyboard would silently stay as it
    /// was. Switch to "static" first so "reset to theme" always has a visible result.
    pub fn reset_to_theme(&self) {
        if !effect_needs(self.current_effect()).contains(&"colour1") {
            if let Some(i) = aura_effect_names().iter().position(|e| *e == "static") {
                self.effect_dropdown.set_selected(i as u32);
            }
        }
        self.colour1_btn.set_rgba(&hex_to_gdk_rgba(&theme_primary_hex()));
        self.colour2_btn.set_rgba(&hex_to_gdk_rgba(&theme_secondary_hex()));
    }

    fn changed(&self) {
        if !self.suppress.get() {
            self.apply_debouncer.trigger();
        }
    }

    fn do_apply(&self) {
        let effect = self.current_effect();
        let speed = SPEEDS.get(self.speed_dropdown.selected() as usize).copied().unwrap_or("med");
        let direction = DIRECTIONS.get(self.direction_dropdown.selected() as usize).copied().unwrap_or("right");
        let cmd = build_aura_cmd(
            effect,
            &gdk_rgba_to_hex(&self.colour1_btn.rgba()),
            &gdk_rgba_to_hex(&self.colour2_btn.rgba()),
            direction,
            speed,
        );
        let toast = self.toast.clone();
        let on_changed = self.on_changed.clone();
        let done: DoneCb = Box::new(move |ok, err| {
            if !ok {
                toast(&failure_text("Effect failed", &err));
            } else if let Some(f) = on_changed {
                f();
            }
        });
        run_cmd_vec(&cmd, Some(done));
    }
}
