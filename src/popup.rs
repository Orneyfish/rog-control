//! The main window: a single vertically stacked dashboard (Performance, GPU, Screen, Keyboard,
//! Battery, Startup, Footer), auto-sized to its content, as in G-Helper's SettingsForm: no tabs,
//! no sidebar, no scrollbar.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::glib;

use crate::config::{Cfg, SaveCb};
use crate::extra::ExtraWindow;
use crate::fans::FansPowerWindow;
use crate::hw::device_name;
use crate::lighting::LightingWindow;
use crate::panels::*;
use crate::widgets::ToastCb;

pub struct RogPopup {
    pub window: adw::Window,
    toast_overlay: adw::ToastOverlay,
    pub performance: Rc<PerformancePanel>,
    pub gpu: Rc<GpuPanel>,
    pub screen: Rc<ScreenPanel>,
    pub keyboard: Rc<KeyboardPanel>,
    pub battery: Rc<BatteryPanel>,
    pub startup: Rc<StartupPanel>,
    pub footer: FooterPanel,
    pub fans_window: RefCell<Option<Rc<FansPowerWindow>>>,
    pub lighting_window: RefCell<Option<Rc<LightingWindow>>>,
    pub extra_window: RefCell<Option<Rc<ExtraWindow>>>,
    app: Option<adw::Application>,
    cfg: Cfg,
    save: SaveCb,
    refresh_source: RefCell<Option<glib::SourceId>>,
}

impl RogPopup {
    pub fn new(
        app: Option<&adw::Application>,
        cfg: Cfg,
        save: SaveCb,
        on_battery: Rc<dyn Fn() -> bool>,
        on_quit: Rc<dyn Fn()>,
    ) -> Rc<Self> {
        let window = adw::Window::new();
        if let Some(app) = app {
            window.set_application(Some(app));
        }
        window.set_title(Some("ROG Control"));
        window.set_resizable(false);

        let toast_overlay = adw::ToastOverlay::new();
        window.set_content(Some(&toast_overlay));
        let toolbar_view = adw::ToolbarView::new();
        toast_overlay.set_child(Some(&toolbar_view));
        toolbar_view.add_top_bar(&adw::HeaderBar::builder().title_widget(&adw::WindowTitle::new("ROG Control", &device_name())).build());

        let outer = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).build();
        outer.set_margin_top(14);
        outer.set_margin_bottom(14);
        outer.set_margin_start(16);
        outer.set_margin_end(16);

        let this = Rc::new_cyclic(|weak: &Weak<RogPopup>| {
            let toast: ToastCb = {
                let weak = weak.clone();
                Rc::new(move |text: &str| {
                    if let Some(p) = weak.upgrade() {
                        p.show_toast(text);
                    }
                })
            };
            let open_fans = {
                let weak = weak.clone();
                Rc::new(move || {
                    if let Some(p) = weak.upgrade() {
                        p.open_fans();
                    }
                }) as Rc<dyn Fn()>
            };
            let open_lighting = {
                let weak = weak.clone();
                Rc::new(move || {
                    if let Some(p) = weak.upgrade() {
                        p.open_lighting();
                    }
                }) as Rc<dyn Fn()>
            };
            let open_extra = {
                let weak = weak.clone();
                Rc::new(move || {
                    if let Some(p) = weak.upgrade() {
                        p.open_extra();
                    }
                }) as Rc<dyn Fn()>
            };
            let get_root: Rc<dyn Fn() -> Option<gtk::Widget>> = {
                let weak = weak.clone();
                Rc::new(move || weak.upgrade().map(|p| p.window.clone().upcast::<gtk::Widget>()))
            };

            let performance = PerformancePanel::new(toast.clone(), cfg.clone(), save.clone(), on_battery.clone(), open_fans);
            let gpu = GpuPanel::new(toast.clone(), cfg.clone(), save.clone(), on_battery.clone(), get_root);
            let screen = ScreenPanel::new(toast.clone(), cfg.clone(), save.clone());
            let keyboard = KeyboardPanel::new(toast.clone(), cfg.clone(), save.clone(), on_battery, open_lighting);
            let battery = BatteryPanel::new(toast.clone());
            let startup = StartupPanel::new(toast);
            let footer = FooterPanel::new(open_extra, on_quit);

            for panel in [&performance.root, &gpu.root, &screen.root, &keyboard.root, &battery.root, &startup.root] {
                outer.append(panel);
                outer.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            }
            outer.append(&footer.root);
            toolbar_view.set_content(Some(&outer));

            Self {
                window,
                toast_overlay,
                performance,
                gpu,
                screen,
                keyboard,
                battery,
                startup,
                footer,
                fans_window: RefCell::new(None),
                lighting_window: RefCell::new(None),
                extra_window: RefCell::new(None),
                app: app.cloned(),
                cfg,
                save,
                refresh_source: RefCell::new(None),
            }
        });

        let weak = Rc::downgrade(&this);
        this.window.connect_close_request(move |w| {
            w.set_visible(false);
            if let Some(p) = weak.upgrade() {
                p.stop_polling();
            }
            glib::Propagation::Stop
        });
        this
    }

    pub fn show_toast(&self, text: &str) {
        self.toast_overlay.add_toast(adw::Toast::new(text));
    }

    fn toast_cb(self: &Rc<Self>) -> ToastCb {
        let weak = Rc::downgrade(self);
        Rc::new(move |text: &str| {
            if let Some(p) = weak.upgrade() {
                p.show_toast(text);
            }
        })
    }

    /// Companion windows are destroyed on close and rebuilt fresh on the next open; only one of
    /// each exists at a time.
    pub fn open_fans(self: &Rc<Self>) {
        let existing = self.fans_window.borrow().clone();
        let win = match existing {
            Some(w) => {
                w.refresh();
                w
            }
            None => {
                let w = FansPowerWindow::new(self.app.as_ref(), self.toast_cb(), self.cfg.clone(), self.save.clone());
                let weak = Rc::downgrade(self);
                w.window.connect_close_request(move |_| {
                    if let Some(p) = weak.upgrade() {
                        p.fans_window.borrow_mut().take();
                    }
                    glib::Propagation::Proceed
                });
                *self.fans_window.borrow_mut() = Some(w.clone());
                w
            }
        };
        win.window.present();
    }

    pub fn open_lighting(self: &Rc<Self>) {
        let existing = self.lighting_window.borrow().clone();
        let win = match existing {
            Some(w) => {
                w.refresh();
                w
            }
            None => {
                let weak = Rc::downgrade(self);
                let on_changed: Rc<dyn Fn()> = Rc::new(move || {
                    if let Some(p) = weak.upgrade() {
                        p.keyboard.refresh();
                    }
                });
                let w = LightingWindow::new(self.app.as_ref(), self.toast_cb(), Some(on_changed));
                let weak = Rc::downgrade(self);
                w.window.connect_close_request(move |_| {
                    if let Some(p) = weak.upgrade() {
                        p.lighting_window.borrow_mut().take();
                    }
                    glib::Propagation::Proceed
                });
                *self.lighting_window.borrow_mut() = Some(w.clone());
                w
            }
        };
        win.window.present();
    }

    pub fn open_extra(self: &Rc<Self>) {
        let existing = self.extra_window.borrow().clone();
        let win = match existing {
            Some(w) => {
                w.refresh();
                w
            }
            None => {
                let w = ExtraWindow::new(self.app.as_ref(), self.toast_cb());
                let weak = Rc::downgrade(self);
                w.window.connect_close_request(move |_| {
                    if let Some(p) = weak.upgrade() {
                        p.extra_window.borrow_mut().take();
                    }
                    glib::Propagation::Proceed
                });
                *self.extra_window.borrow_mut() = Some(w.clone());
                w
            }
        };
        win.window.present();
    }

    pub fn is_visible(&self) -> bool {
        self.window.get_visible()
    }

    pub fn refresh_all(self: &Rc<Self>) {
        self.performance.refresh();
        self.gpu.refresh();
        self.screen.refresh();
        self.keyboard.refresh();
        self.battery.refresh();
        self.startup.refresh();
        if let Some(w) = self.fans_window.borrow().clone() {
            w.refresh();
        }
        if let Some(w) = self.lighting_window.borrow().clone() {
            w.refresh();
        }
        if let Some(w) = self.extra_window.borrow().clone() {
            w.refresh();
        }
    }

    pub fn hide(&self) {
        self.window.set_visible(false);
        self.stop_polling();
    }

    /// Shows the window (refreshing everything first) and starts polling; a no-op re-present when
    /// it is already up.
    pub fn show(self: &Rc<Self>) {
        if !self.is_visible() {
            self.refresh_all();
            self.window.present();
            self.start_polling();
        } else {
            self.window.present();
        }
    }

    pub fn toggle(self: &Rc<Self>) {
        if self.is_visible() {
            self.hide();
        } else {
            self.show();
        }
    }

    /// While visible, Performance, GPU and Battery refresh every 2 s; hidden, nothing polls.
    fn start_polling(self: &Rc<Self>) {
        if self.refresh_source.borrow().is_some() {
            return;
        }
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_seconds_local(2, move || {
            let Some(p) = weak.upgrade() else { return glib::ControlFlow::Break };
            if !p.is_visible() {
                p.refresh_source.borrow_mut().take();
                return glib::ControlFlow::Break;
            }
            p.performance.refresh();
            p.gpu.refresh();
            p.battery.refresh();
            glib::ControlFlow::Continue
        });
        *self.refresh_source.borrow_mut() = Some(id);
    }

    fn stop_polling(&self) {
        if let Some(id) = self.refresh_source.borrow_mut().take() {
            id.remove();
        }
    }

    pub fn is_polling(&self) -> bool {
        self.refresh_source.borrow().is_some()
    }
}
