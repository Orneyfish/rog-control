//! Small reusable widgets and helpers shared by the main window and the companion windows.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;

/// Shows a toast (the owning window's overlay) with the given text.
pub type ToastCb = Rc<dyn Fn(&str)>;
/// `(heading, body, ok label, on confirm)`: ask the user before doing something destructive.
pub type ConfirmFn = Rc<dyn Fn(&str, &str, &str, Box<dyn FnOnce()>)>;

/// "Command failed: <first 80 characters of the error>".
pub fn failure_text(prefix: &str, err: &str) -> String {
    format!("{prefix}: {}", crate::sys::truncate_chars(err, 80))
}

pub fn section_label(text: &str) -> gtk::Label {
    let lbl = gtk::Label::new(Some(text));
    lbl.set_halign(gtk::Align::Start);
    lbl.add_css_class("rog-section");
    lbl
}

/// Icon + bold title on the left and a live-status label on the right: G-Helper puts the
/// sensor/state read-out inline in the title row, not in a separate stats panel.
pub struct SectionHeader {
    pub root: gtk::Box,
    pub title: gtk::Label,
    pub stat: gtk::Label,
}

pub fn section_header(icon_name: &str, title: &str) -> SectionHeader {
    let root = gtk::Box::builder().spacing(8).build();
    let icon = gtk::Image::from_icon_name(icon_name);
    let title = gtk::Label::new(Some(title));
    title.add_css_class("rog-title");
    let stat = gtk::Label::new(Some(""));
    stat.add_css_class("rog-subtitle");
    stat.set_halign(gtk::Align::End);
    stat.set_hexpand(true);
    root.append(&icon);
    root.append(&title);
    root.append(&stat);
    SectionHeader { root, title, stat }
}

pub fn hint_label() -> gtk::Label {
    let lbl = gtk::Label::new(Some(""));
    lbl.set_halign(gtk::Align::Start);
    lbl.add_css_class("rog-hint");
    lbl
}

pub fn add_hover_hint(widget: &impl IsA<gtk::Widget>, hint: &gtk::Label, text: &'static str) {
    let motion = gtk::EventControllerMotion::new();
    let (enter_hint, leave_hint) = (hint.clone(), hint.clone());
    motion.connect_enter(move |_, _, _| enter_hint.set_label(text));
    motion.connect_leave(move |_| leave_hint.set_label(""));
    widget.add_controller(motion);
}

fn mode_button_content(icon_name: &str, label: &str) -> (gtk::Box, gtk::Label) {
    let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).halign(gtk::Align::Center).build();
    let image = gtk::Image::from_icon_name(icon_name);
    image.set_pixel_size(20);
    let text = gtk::Label::new(Some(label));
    content.append(&image);
    content.append(&text);
    (content, text)
}

/// One of a radio-style group of big icon-over-text buttons. Returns the button and its text label
/// (so a label can change later, e.g. "240Hz + OD").
pub fn mode_toggle(
    icon_name: &str,
    label: &str,
    accent_class: Option<&str>,
    group_with: Option<&gtk::ToggleButton>,
) -> (gtk::ToggleButton, gtk::Label) {
    let btn = gtk::ToggleButton::new();
    btn.add_css_class("rog-mode-btn");
    if let Some(class) = accent_class {
        btn.add_css_class(class);
    }
    if let Some(group) = group_with {
        btn.set_group(Some(group));
    }
    let (content, text) = mode_button_content(icon_name, label);
    btn.set_child(Some(&content));
    btn.set_hexpand(true);
    (btn, text)
}

/// The same look, as a plain (non-toggle) action button.
pub fn mode_action_button(icon_name: &str, label: &str, accent_class: &str) -> gtk::Button {
    let btn = gtk::Button::new();
    btn.add_css_class("rog-mode-btn");
    btn.add_css_class(accent_class);
    btn.add_css_class("rog-secondary");
    let (content, _) = mode_button_content(icon_name, label);
    btn.set_child(Some(&content));
    btn.set_hexpand(true);
    btn
}

pub struct SwitchRow {
    pub root: gtk::Box,
    pub switch: gtk::Switch,
}

/// A label on the left and a switch on the right; `handler` runs on `state-set` and decides
/// whether GTK should still apply the state itself (`Proceed`, the usual).
pub fn switch_row(text: &str, handler: impl Fn(&gtk::Switch, bool) -> glib::Propagation + 'static) -> SwitchRow {
    let root = gtk::Box::builder().spacing(8).build();
    let label = gtk::Label::new(Some(text));
    label.set_hexpand(true);
    label.set_halign(gtk::Align::Start);
    let switch = gtk::Switch::new();
    switch.set_valign(gtk::Align::Center);
    switch.connect_state_set(move |sw, state| handler(sw, state));
    root.append(&label);
    root.append(&switch);
    SwitchRow { root, switch }
}

/// Modal Cancel / `ok_label` dialog; `on_confirm` runs only on the OK response.
pub fn confirm_dialog(
    parent: Option<&gtk::Widget>,
    heading: &str,
    body: &str,
    ok_label: &str,
    on_confirm: Box<dyn FnOnce()>,
    destructive: bool,
) {
    let dialog = adw::AlertDialog::new(Some(heading), Some(body));
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("ok", ok_label);
    if destructive {
        dialog.set_response_appearance("ok", adw::ResponseAppearance::Destructive);
    }
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let on_confirm = RefCell::new(Some(on_confirm));
    dialog.connect_response(None, move |_, response| {
        if response == "ok" {
            if let Some(f) = on_confirm.borrow_mut().take() {
                f();
            }
        }
    });
    dialog.present(parent);
}

/// Coalesces rapid repeated triggers into one call, fired after `delay_ms` of quiet.
///
/// Fixes a real race: each RGB control and each slider tick used to fire its own concurrent
/// command, and whichever finished last (not whichever was picked last) won, or a burst queued
/// long enough to hit the client-side timeout. Debounced, only the last change in a burst ever
/// becomes a command, reading fully settled widget state.
pub struct Debouncer {
    delay: Duration,
    callback: RefCell<Option<Rc<dyn Fn()>>>,
    source: Rc<RefCell<Option<glib::SourceId>>>,
}

impl Debouncer {
    /// The quiet period every slider and RGB control in this app uses.
    pub const QUIET_MS: u64 = 150;

    /// A debouncer whose callback is attached later with [`Debouncer::bind`] (it usually needs
    /// the object that owns the debouncer).
    pub fn new(delay_ms: u64) -> Self {
        Self { delay: Duration::from_millis(delay_ms), callback: RefCell::new(None), source: Rc::new(RefCell::new(None)) }
    }

    pub fn with_callback(delay_ms: u64, callback: impl Fn() + 'static) -> Self {
        let d = Self::new(delay_ms);
        d.bind(callback);
        d
    }

    pub fn bind(&self, callback: impl Fn() + 'static) {
        *self.callback.borrow_mut() = Some(Rc::new(callback));
    }

    pub fn trigger(&self) {
        if let Some(id) = self.source.borrow_mut().take() {
            id.remove();
        }
        let callback = self.callback.borrow().clone();
        let source = self.source.clone();
        let id = glib::timeout_add_local(self.delay, move || {
            *source.borrow_mut() = None;
            if let Some(cb) = &callback {
                cb();
            }
            glib::ControlFlow::Break
        });
        *self.source.borrow_mut() = Some(id);
    }
}

impl Drop for Debouncer {
    fn drop(&mut self) {
        if let Some(id) = self.source.borrow_mut().take() {
            id.remove();
        }
    }
}

/// A `Cell<bool>` that is true while a widget is being synced from hardware, so the resulting
/// change signals are not mistaken for user input.
pub struct Suppress(Cell<bool>);

impl Suppress {
    pub fn new(initial: bool) -> Self {
        Self(Cell::new(initial))
    }

    pub fn get(&self) -> bool {
        self.0.get()
    }

    pub fn set(&self, v: bool) {
        self.0.set(v);
    }

    /// Runs `f` with the flag raised, restoring it afterwards.
    pub fn during<R>(&self, f: impl FnOnce() -> R) -> R {
        let before = self.0.replace(true);
        let r = f();
        self.0.set(before);
        r
    }
}
