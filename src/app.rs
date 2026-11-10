//! The resident service: a GTK application with no window at boot, a tray icon, the control
//! service for hotkeys, and the automation manager.

use std::cell::{Cell, RefCell};
use std::process::ExitCode;
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use crate::automation::AutomationManager;
use crate::config::{self, Cfg, SaveCb};
use crate::control::{self, ControlHandlers};
use crate::hw::{cycle_profile, APP_ID};
use crate::popup::RogPopup;
use crate::theme;
use crate::tray::{self, TrayHandlers};

struct AppState {
    app: adw::Application,
    cfg: Cfg,
    popup: RefCell<Option<Rc<RogPopup>>>,
    automation: RefCell<Option<Rc<AutomationManager>>>,
    failed: Cell<bool>,
}

impl AppState {
    fn save_cb(self: &Rc<Self>) -> SaveCb {
        let cfg = self.cfg.clone();
        Rc::new(move || {
            if let Err(e) = config::save(&cfg.borrow()) {
                eprintln!("rog-control: could not save config: {e}");
            }
        })
    }

    fn on_battery(self: &Rc<Self>) -> Rc<dyn Fn() -> bool> {
        let weak = Rc::downgrade(self);
        Rc::new(move || weak.upgrade().and_then(|s| s.automation.borrow().as_ref().and_then(|a| a.on_battery())).unwrap_or(false))
    }

    /// The main window, built on first use.
    fn popup(self: &Rc<Self>) -> Rc<RogPopup> {
        if let Some(p) = self.popup.borrow().clone() {
            return p;
        }
        let weak = Rc::downgrade(self);
        let on_quit: Rc<dyn Fn()> = Rc::new(move || {
            if let Some(s) = weak.upgrade() {
                s.quit();
            }
        });
        let popup = RogPopup::new(Some(&self.app), self.cfg.clone(), self.save_cb(), self.on_battery(), on_quit);
        *self.popup.borrow_mut() = Some(popup.clone());
        popup
    }

    fn toggle_popup(self: &Rc<Self>) {
        self.popup().toggle();
    }

    /// Right-click on the tray icon quits: the usual convention for a single-action tray, and the
    /// only way to end this app short of `systemctl --user stop`, since it has no taskbar window.
    fn quit(self: &Rc<Self>) {
        if let Some(p) = self.popup.borrow().as_ref() {
            p.hide();
        }
        self.app.quit();
    }

    fn refresh_popup_if_visible(self: &Rc<Self>) {
        let popup = self.popup.borrow().clone();
        if let Some(p) = popup.filter(|p| p.is_visible()) {
            p.refresh_all();
        }
    }

    fn cycle(self: &Rc<Self>, step: i32) {
        let weak = Rc::downgrade(self);
        cycle_profile(
            step,
            Some(Box::new(move |_, _| {
                if let Some(s) = weak.upgrade() {
                    s.refresh_popup_if_visible();
                }
            })),
        );
    }

    fn open_window(self: &Rc<Self>, name: &str) -> Result<(), String> {
        match name {
            "main" => self.popup().show(),
            "fans" => self.popup().open_fans(),
            "lighting" => self.popup().open_lighting(),
            "extra" => self.popup().open_extra(),
            other => return Err(format!("unknown window '{}'", crate::sys::truncate_chars(other, 40))),
        }
        Ok(())
    }

    fn startup(self: &Rc<Self>) {
        theme::register_icons();
        theme::install_css();

        let connection = match gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("rog-control: cannot reach the session bus: {e}");
                self.failed.set(true);
                self.app.quit();
                return;
            }
        };

        let cb = |f: fn(&Rc<AppState>)| {
            let weak = Rc::downgrade(self);
            move || {
                if let Some(s) = weak.upgrade() {
                    f(&s);
                }
            }
        };
        let (activate, secondary, open_cb, cycle_cb, cycle_back_cb) = (
            cb(Self::toggle_popup),
            cb(Self::quit),
            cb(Self::toggle_popup),
            cb(|s| s.cycle(1)),
            cb(|s| s.cycle(-1)),
        );
        let weak = Rc::downgrade(self);
        let scroll = move |_delta: i32| {
            // Any scroll cycles to the next profile, as G-Helper / omarchy-asus's tray does.
            if let Some(s) = weak.upgrade() {
                s.cycle(1);
            }
        };
        let weak = Rc::downgrade(self);
        let open_window = move |name: &str| match weak.upgrade() {
            Some(s) => s.open_window(name),
            None => Err("shutting down".to_string()),
        };

        let icon_path = theme::icons_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        if let Err(e) = tray::register_item(
            &connection,
            icon_path,
            TrayHandlers { activate: Box::new(activate), secondary_activate: Box::new(secondary), scroll: Box::new(scroll) },
        ) {
            eprintln!("rog-control: tray item not exported: {e}");
        }
        tray::register_with_watcher(connection.clone(), 10);

        let handlers = ControlHandlers {
            open: Box::new(open_cb),
            cycle: Box::new(cycle_cb),
            cycle_back: Box::new(cycle_back_cb),
            open_window: Box::new(open_window),
        };
        if let Err(e) = control::register_control(&connection, handlers) {
            eprintln!("rog-control: control service not exported: {e}");
            self.failed.set(true);
            self.app.quit();
            return;
        }
        let (app, weak) = (self.app.clone(), Rc::downgrade(self));
        // Ownership lasts as long as the connection, i.e. the process; there is nothing to hold.
        let _owner = control::own_control_name(&connection, move || {
            eprintln!("rog-control: {} is already owned by another instance", control::CONTROL_NAME);
            if let Some(s) = weak.upgrade() {
                s.failed.set(true);
            }
            app.quit();
        });

        let weak = Rc::downgrade(self);
        let refresh_ui: Rc<dyn Fn()> = Rc::new(move || {
            if let Some(s) = weak.upgrade() {
                s.refresh_popup_if_visible();
            }
        });
        *self.automation.borrow_mut() = Some(AutomationManager::start(self.cfg.clone(), refresh_ui));
    }
}

/// Runs the resident service until it is quit.
pub fn run() -> ExitCode {
    theme::quiet_user_theme_warnings();
    let app = adw::Application::builder().application_id(APP_ID).build();
    let state = Rc::new(AppState {
        app: app.clone(),
        cfg: config::new_cfg(config::load()),
        popup: RefCell::new(None),
        automation: RefCell::new(None),
        failed: Cell::new(false),
    });
    // No window at boot: hold() keeps the process alive with nothing shown.
    let _hold = app.hold();
    let s = state.clone();
    app.connect_startup(move |_| s.startup());
    // Opening the panel always goes through the control service, never through activation.
    app.connect_activate(|_| {});
    let code = app.run_with_args::<&str>(&[]);
    if state.failed.get() || code.value() != 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
