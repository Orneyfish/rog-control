//! The "Fans and Power" companion window: per-profile fan curves (draggable temperature -> speed
//! charts), CPU power limits and NVIDIA GPU settings. A standalone window, as in G-Helper, not a
//! tab of the main window.

use std::cell::RefCell;
use std::f64::consts::PI;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use serde_json::json;

use crate::config::{self, Cfg, SaveCb};
use crate::hw::*;
use crate::sys::{capitalize, py_round, read_sysfs, read_sysfs_int, run_capture, run_cmd, spawn_bg, tool_available, DoneCb};
use crate::theme::{hex_to_rgba, load_material_colors};
use crate::widgets::*;

pub const DEFAULT_CURVE: [(i32, i32); 8] = [(40, 20), (50, 30), (55, 40), (60, 50), (65, 60), (70, 75), (80, 90), (95, 100)];

pub const TEMP_MIN: i32 = 20;
pub const TEMP_MAX: i32 = 100;
pub const SPEED_MIN: i32 = 0;
pub const SPEED_MAX: i32 = 100;
const PAD_L: f64 = 34.0;
const PAD_R: f64 = 12.0;
const PAD_T: f64 = 12.0;
const PAD_B: f64 = 22.0;
const HIT_RADIUS: f64 = 16.0;

type Rgba = (f64, f64, f64, f64);
type PointSelectedCb = Rc<dyn Fn(usize, (i32, i32))>;
type SliderBinding<'a> = (&'a LabeledSlider, usize, fn(&FansPowerWindow));

struct GraphState {
    points: Vec<(i32, i32)>,
    enabled: bool,
    dragging: Option<usize>,
    drag_origin: (f64, f64),
}

/// A draggable temperature -> speed % curve, styled after G-Helper's fan chart (8 points, 20-110 C
/// there, 0-100 %). asusctl's fan-curve model is percent-based, not RPM, so this stays on a
/// percent axis. Releasing the mouse after dragging a point auto-applies, as G-Helper does.
pub struct FanCurveGraph {
    pub area: gtk::DrawingArea,
    state: Rc<RefCell<GraphState>>,
    on_point_selected: Option<PointSelectedCb>,
}

impl FanCurveGraph {
    pub fn new(on_point_selected: Option<PointSelectedCb>, on_drag_end: Option<Rc<dyn Fn()>>) -> Rc<Self> {
        let state = Rc::new(RefCell::new(GraphState {
            points: DEFAULT_CURVE.to_vec(),
            enabled: true,
            dragging: None,
            drag_origin: (0.0, 0.0),
        }));
        let colours = load_material_colors();
        let colour = |name: &str, default: &str, alpha: f64| hex_to_rgba(colours.get(name).map_or(default, String::as_str), alpha);
        let line = colour("primary", "#f7b6b9", 1.0);
        let point = colour("onSurface", "#f8e0e0", 1.0);
        let grid = colour("onSurfaceVariant", "#bba6a6", 0.18);
        let inert = colour("onSurfaceVariant", "#bba6a6", 0.4);

        let area = gtk::DrawingArea::new();
        area.set_content_width(380);
        area.set_content_height(180);
        area.set_hexpand(true);
        area.set_vexpand(false);
        let draw_state = state.clone();
        area.set_draw_func(move |_, cr, w, h| draw(&draw_state.borrow(), cr, f64::from(w), f64::from(h), line, point, grid, inert));

        let this = Rc::new(Self { area, state, on_point_selected });
        let drag = gtk::GestureDrag::new();
        let weak = Rc::downgrade(&this);
        drag.connect_drag_begin(move |_, x, y| {
            if let Some(s) = weak.upgrade() {
                let (w, h) = s.geometry();
                let mut st = s.state.borrow_mut();
                st.dragging = nearest_point(&st.points, x, y, w, h);
                st.drag_origin = (x, y);
                drop(st);
                s.area.queue_draw();
            }
        });
        let weak = Rc::downgrade(&this);
        drag.connect_drag_update(move |_, ox, oy| {
            if let Some(s) = weak.upgrade() {
                let (w, h) = s.geometry();
                let (i, (sx, sy)) = {
                    let st = s.state.borrow();
                    match st.dragging {
                        Some(i) => (i, st.drag_origin),
                        None => return,
                    }
                };
                let (temp, speed) = from_px(sx + ox, sy + oy, w, h);
                s.drag_to(i, temp, speed);
            }
        });
        let weak = Rc::downgrade(&this);
        drag.connect_drag_end(move |_, _, _| {
            if let Some(s) = weak.upgrade() {
                let was_dragging = s.state.borrow_mut().dragging.take().is_some();
                s.area.queue_draw();
                if let (true, Some(cb)) = (was_dragging, &on_drag_end) {
                    cb();
                }
            }
        });
        this.area.add_controller(drag);
        this
    }

    pub fn points(&self) -> Vec<(i32, i32)> {
        self.state.borrow().points.clone()
    }

    pub fn set_points(&self, points: &[(i32, i32)]) {
        self.state.borrow_mut().points = points.to_vec();
        self.area.queue_draw();
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.state.borrow_mut().enabled = enabled;
        self.area.queue_draw();
    }

    fn geometry(&self) -> (f64, f64) {
        let w = if self.area.width() > 0 { self.area.width() } else { self.area.content_width() };
        let h = if self.area.height() > 0 { self.area.height() } else { self.area.content_height() };
        (f64::from(w), f64::from(h))
    }

    /// Moves point `i` towards (`temp`, `speed`). Temperature is clamped between its neighbours,
    /// and so is speed: asusctl rejects a curve whose speed is not non-decreasing with
    /// temperature ("Invalid percentage, previous value X is higher than next value Y"), so
    /// dragging a point past a neighbour's speed must never produce one.
    pub fn drag_to(&self, i: usize, temp: f64, speed: f64) {
        let moved = {
            let mut st = self.state.borrow_mut();
            let n = st.points.len();
            if i >= n {
                return;
            }
            let lo = if i > 0 { st.points[i - 1].0 + 1 } else { TEMP_MIN };
            let hi = if i + 1 < n { st.points[i + 1].0 - 1 } else { TEMP_MAX };
            let temp = lo.max(hi.min(py_round(temp) as i32));
            let speed_lo = if i > 0 { st.points[i - 1].1 } else { SPEED_MIN };
            let speed_hi = if i + 1 < n { st.points[i + 1].1 } else { SPEED_MAX };
            let speed = speed_lo.max(speed_hi.min(py_round(speed) as i32));
            st.points[i] = (temp, speed);
            st.points[i]
        };
        self.area.queue_draw();
        if let Some(cb) = &self.on_point_selected {
            cb(i, moved);
        }
    }
}

fn to_px(temp: f64, speed: f64, w: f64, h: f64) -> (f64, f64) {
    let x = PAD_L + (temp - f64::from(TEMP_MIN)) / f64::from(TEMP_MAX - TEMP_MIN) * (w - PAD_L - PAD_R);
    let y = h - PAD_B - (speed - f64::from(SPEED_MIN)) / f64::from(SPEED_MAX - SPEED_MIN) * (h - PAD_T - PAD_B);
    (x, y)
}

fn from_px(x: f64, y: f64, w: f64, h: f64) -> (f64, f64) {
    let temp = f64::from(TEMP_MIN) + (x - PAD_L) / (w - PAD_L - PAD_R) * f64::from(TEMP_MAX - TEMP_MIN);
    let speed = f64::from(SPEED_MIN) + (h - PAD_B - y) / (h - PAD_T - PAD_B) * f64::from(SPEED_MAX - SPEED_MIN);
    (temp, speed)
}

fn nearest_point(points: &[(i32, i32)], x: f64, y: f64, w: f64, h: f64) -> Option<usize> {
    let mut best: Option<usize> = None;
    let mut best_d = HIT_RADIUS;
    for (i, (t, s)) in points.iter().enumerate() {
        let (px, py) = to_px(f64::from(*t), f64::from(*s), w, h);
        let d = (px - x).hypot(py - y);
        if d < best_d {
            best = Some(i);
            best_d = d;
        }
    }
    best
}

#[allow(clippy::too_many_arguments)]
fn draw(st: &GraphState, cr: &gtk::cairo::Context, w: f64, h: f64, line: Rgba, point: Rgba, grid: Rgba, inert: Rgba) {
    cr.select_font_face("sans-serif", gtk::cairo::FontSlant::Normal, gtk::cairo::FontWeight::Normal);
    cr.set_font_size(9.0);
    cr.set_source_rgba(grid.0, grid.1, grid.2, grid.3);
    cr.set_line_width(1.0);
    for i in 0..5 {
        let y = PAD_T + f64::from(i) * (h - PAD_T - PAD_B) / 4.0;
        cr.move_to(PAD_L, y);
        cr.line_to(w - PAD_R, y);
        let speed = f64::from(SPEED_MAX) - f64::from(i) * f64::from(SPEED_MAX - SPEED_MIN) / 4.0;
        cr.move_to(2.0, y + 3.0);
        let _ = cr.show_text(&format!("{}", speed as i64));
    }
    for i in 0..5 {
        let x = PAD_L + f64::from(i) * (w - PAD_L - PAD_R) / 4.0;
        cr.move_to(x, PAD_T);
        cr.line_to(x, h - PAD_B);
        let temp = f64::from(TEMP_MIN) + f64::from(i) * f64::from(TEMP_MAX - TEMP_MIN) / 4.0;
        cr.move_to(x - 6.0, h - 6.0);
        let _ = cr.show_text(&format!("{}", temp as i64));
    }
    let _ = cr.stroke();

    let line_rgba = if st.enabled { line } else { inert };
    let point_rgba = if st.enabled { point } else { inert };
    cr.set_source_rgba(line_rgba.0, line_rgba.1, line_rgba.2, line_rgba.3);
    cr.set_line_width(2.5);
    for (i, (t, s)) in st.points.iter().enumerate() {
        let (x, y) = to_px(f64::from(*t), f64::from(*s), w, h);
        if i == 0 {
            cr.move_to(x, y);
        } else {
            cr.line_to(x, y);
        }
    }
    let _ = cr.stroke();

    for (i, (t, s)) in st.points.iter().enumerate() {
        let (x, y) = to_px(f64::from(*t), f64::from(*s), w, h);
        let radius = if st.dragging == Some(i) { 6.0 } else { 4.0 };
        cr.arc(x, y, radius, 0.0, 2.0 * PI);
        cr.set_source_rgba(point_rgba.0, point_rgba.1, point_rgba.2, point_rgba.3);
        let _ = cr.fill_preserve();
        cr.set_source_rgba(line_rgba.0, line_rgba.1, line_rgba.2, line_rgba.3);
        cr.set_line_width(2.0);
        let _ = cr.stroke();
    }
}

pub struct LabeledSlider {
    pub root: gtk::Box,
    pub scale: gtk::Scale,
    pub value_label: gtk::Label,
}

pub fn labeled_slider(text: &str, lo: f64, hi: f64, unit: &'static str) -> LabeledSlider {
    let root = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).build();
    let label_row = gtk::Box::builder().spacing(8).build();
    let label = gtk::Label::builder().label(text).halign(gtk::Align::Start).hexpand(true).build();
    let value_label = gtk::Label::new(Some(&format!("{}{unit}", lo as i64)));
    value_label.add_css_class("rog-subtitle");
    label_row.append(&label);
    label_row.append(&value_label);
    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, lo, hi, 1.0);
    scale.set_draw_value(false);
    let shown = value_label.clone();
    scale.connect_value_changed(move |s| shown.set_label(&format!("{}{unit}", s.value() as i64)));
    root.append(&label_row);
    root.append(&scale);
    LabeledSlider { root, scale, value_label }
}

// ---------------------------------------------------------------------------
// One fan's curve editor
// ---------------------------------------------------------------------------

/// First `n` characters of `s` starting at byte `from`.
fn chunk_from(s: &str, from: usize, n: usize) -> String {
    s[from..].chars().take(n).collect()
}

/// The integer list inside `key: ( ... )`.
fn parse_int_list(chunk: &str, key: &str) -> Option<Vec<i32>> {
    let rest = chunk[chunk.find(key)? + key.len()..].trim_start().strip_prefix('(')?;
    let inner = &rest[..rest.find(')')?];
    inner.split(',').filter(|x| !x.trim().is_empty()).map(|x| x.trim().parse().ok()).collect()
}

/// Parses one fan out of `asusctl fan-curve --mod-profile <Name>` output:
/// `( fan: CPU, pwm: (64, ...), temp: (47, ...), enabled: false, ),`. Falls back to the default
/// curve, disabled, when the fan is missing or malformed.
pub fn parse_curve(text: &str, fan: &str) -> (Vec<(i32, i32)>, bool) {
    let marker = format!("fan: {}", fan.to_uppercase());
    if let Some(idx) = text.find(&marker) {
        let chunk = chunk_from(text, idx, 400);
        let enabled = chunk.find("enabled:").and_then(|i| {
            let rest = chunk[i + "enabled:".len()..].trim_start();
            if rest.starts_with("true") {
                Some(true)
            } else if rest.starts_with("false") {
                Some(false)
            } else {
                None
            }
        });
        if let (Some(pwms), Some(temps), Some(enabled)) = (parse_int_list(&chunk, "pwm:"), parse_int_list(&chunk, "temp:"), enabled) {
            if !pwms.is_empty() && !temps.is_empty() {
                let points = temps.iter().zip(&pwms).map(|(t, p)| (*t, py_round(f64::from(*p) / 255.0 * 100.0) as i32)).collect();
                return (points, enabled);
            }
        }
    }
    (DEFAULT_CURVE.to_vec(), false)
}

/// One fan's curve with its enable switch and Factory Defaults button; same shape for CPU and GPU.
pub struct FanCurveEditor {
    pub root: gtk::Box,
    pub graph: Rc<FanCurveGraph>,
    pub point_label: gtk::Label,
    pub enable_switch: gtk::Switch,
    fan: &'static str,
    toast: ToastCb,
    get_profile: Rc<dyn Fn() -> String>,
    suppress: Rc<Suppress>,
}

impl FanCurveEditor {
    pub fn new(fan: &'static str, toast: ToastCb, get_profile: Rc<dyn Fn() -> String>) -> Rc<Self> {
        let root = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).build();
        root.append(&section_label(&format!("{} Fan Profile, %/°C", fan.to_uppercase())));

        let point_label = gtk::Label::builder().label("Drag a point to edit it").halign(gtk::Align::Start).build();
        point_label.add_css_class("rog-subtitle");

        // The graph needs the editor to apply on drag end, the editor owns the graph: break the
        // knot with a weak handle that is filled in once the editor exists.
        let editor_cell: Rc<RefCell<Option<std::rc::Weak<FanCurveEditor>>>> = Rc::new(RefCell::new(None));
        let cell = editor_cell.clone();
        let label = point_label.clone();
        let on_selected: Rc<dyn Fn(usize, (i32, i32))> =
            Rc::new(move |i, (temp, speed)| label.set_label(&format!("Point {}: {temp}°C → {speed}%", i + 1)));
        let on_end: Rc<dyn Fn()> = Rc::new(move || {
            if let Some(e) = cell.borrow().as_ref().and_then(std::rc::Weak::upgrade) {
                e.auto_apply();
            }
        });
        let graph = FanCurveGraph::new(Some(on_selected), Some(on_end));
        root.append(&graph.area);
        root.append(&point_label);

        let suppress = Rc::new(Suppress::new(false));
        let enable_cell = editor_cell.clone();
        let enable_suppress = suppress.clone();
        let enable_row = switch_row("Apply Custom Fan Curve", move |_, state| {
            if !enable_suppress.get() {
                if let Some(e) = enable_cell.borrow().as_ref().and_then(std::rc::Weak::upgrade) {
                    e.on_enable_toggled(state);
                }
            }
            glib::Propagation::Proceed
        });
        root.append(&enable_row.root);

        let btn_row = gtk::Box::builder().spacing(6).homogeneous(true).hexpand(true).build();
        let reset_btn = gtk::Button::builder().label("Factory Defaults").hexpand(true).build();
        btn_row.append(&reset_btn);
        root.append(&btn_row);

        let this = Rc::new(Self { root, graph, point_label, enable_switch: enable_row.switch, fan, toast, get_profile, suppress });
        *editor_cell.borrow_mut() = Some(Rc::downgrade(&this));
        let weak = Rc::downgrade(&this);
        reset_btn.connect_clicked(move |_| {
            if let Some(e) = weak.upgrade() {
                e.reset();
            }
        });
        this.load_curve();
        this
    }

    /// Reads this fan's curve for the selected profile in the background. `--get-enabled` only
    /// ever reports the active profile whatever you ask, so the per-profile query is
    /// `--mod-profile <Name>` with nothing else (it "shows data if no options provided").
    pub fn load_curve(self: &Rc<Self>) {
        let profile = (self.get_profile)();
        let fan = self.fan;
        let weak = Rc::downgrade(self);
        spawn_bg(
            move || {
                let out = run_capture(&["asusctl", "fan-curve", "--mod-profile", &profile], Duration::from_secs(10))
                    .map(|o| o.stdout)
                    .unwrap_or_default();
                parse_curve(&out, fan)
            },
            move |(points, enabled)| {
                if let Some(e) = weak.upgrade() {
                    e.populate(&points, enabled);
                }
            },
        );
    }

    fn populate(&self, points: &[(i32, i32)], enabled: bool) {
        self.suppress.during(|| {
            self.graph.set_points(points);
            self.graph.set_enabled(enabled);
            self.point_label.set_label("Drag a point to edit it");
            self.enable_switch.set_active(enabled);
        });
    }

    /// Mirrors G-Helper: releasing a dragged point commits the curve immediately.
    pub fn auto_apply(&self) {
        let data = self.graph.points().iter().map(|(t, s)| format!("{t}c:{s}%")).collect::<Vec<_>>().join(",");
        let toast = self.toast.clone();
        run_cmd(
            &["asusctl", "fan-curve", "--mod-profile", &(self.get_profile)(), "--fan", self.fan, "--data", &data],
            Some(Box::new(move |ok, err| {
                if !ok {
                    toast(&failure_text("Fan curve failed", &err));
                }
            })),
        );
    }

    fn on_enable_toggled(&self, state: bool) {
        self.graph.set_enabled(state);
        let toast = self.toast.clone();
        run_cmd(
            &[
                "asusctl",
                "fan-curve",
                "--mod-profile",
                &(self.get_profile)(),
                "--enable-fan-curve",
                if state { "true" } else { "false" },
                "--fan",
                self.fan,
            ],
            Some(Box::new(move |ok, err| {
                if !ok {
                    toast(&failure_text("Enable failed", &err));
                }
            })),
        );
    }

    /// `asusctl fan-curve --default` only ever resets the *active* profile: `--mod-profile` is
    /// accepted alongside it but silently ignored for this operation. Refuse rather than reset
    /// the wrong profile when the one being edited is not the active one.
    pub fn reset(self: &Rc<Self>) {
        let profile = (self.get_profile)();
        let active = capitalize(&read_sysfs(Some(PLATFORM_PROFILE_PATH)).unwrap_or_default());
        if profile != active {
            (self.toast)(&format!("Can't reset {profile}'s curve: only the active profile ({active}) can be reset to default"));
            return;
        }
        let weak = Rc::downgrade(self);
        run_cmd(
            &["asusctl", "fan-curve", "--mod-profile", &profile, "--fan", self.fan, "--default"],
            Some(Box::new(move |ok, err| {
                if let Some(e) = weak.upgrade() {
                    if ok {
                        e.load_curve();
                    } else {
                        (e.toast)(&failure_text("Reset failed", &err));
                    }
                }
            })),
        );
    }
}

// ---------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------

struct SubView {
    root: gtk::Box,
    power_page: gtk::Box,
    fan_page: gtk::Box,
    fan_btn: gtk::ToggleButton,
}

/// A Power / Fan Curve segmented toggle over a two-page stack, so the two read as distinct
/// sections instead of one continuously scrolling page.
fn sub_view_page(prefix: &'static str) -> SubView {
    let root = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).build();
    let nav = gtk::Box::builder().spacing(6).homogeneous(true).hexpand(true).build();
    let power_btn = gtk::ToggleButton::builder().label("Power").active(true).hexpand(true).build();
    let fan_btn = gtk::ToggleButton::builder().label("Fan Curve").hexpand(true).build();
    fan_btn.set_group(Some(&power_btn));
    nav.append(&power_btn);
    nav.append(&fan_btn);
    root.append(&nav);

    let sub_stack = gtk::Stack::new();
    let power_page = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).build();
    let fan_page = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).build();
    sub_stack.add_named(&power_page, Some(&format!("{prefix}-power")));
    sub_stack.add_named(&fan_page, Some(&format!("{prefix}-fan")));
    let stack = sub_stack.clone();
    power_btn.connect_toggled(move |b| {
        if b.is_active() {
            stack.set_visible_child_name(&format!("{prefix}-power"));
        }
    });
    let stack = sub_stack.clone();
    fan_btn.connect_toggled(move |b| {
        if b.is_active() {
            stack.set_visible_child_name(&format!("{prefix}-fan"));
        }
    });
    root.append(&sub_stack);
    SubView { root, power_page, fan_page, fan_btn }
}

const POWER_PLANS: [&str; 3] = ["power-saver", "balanced", "performance"];

pub struct FansPowerWindow {
    pub window: adw::Window,
    pub mode_row: gtk::Box,
    pub mode_dropdown: gtk::DropDown,
    pub cpu_nav: gtk::ToggleButton,
    pub gpu_nav: gtk::ToggleButton,
    pub cpu_fan_btn: gtk::ToggleButton,
    pub gpu_fan_btn: gtk::ToggleButton,
    pub cpu_fan_page: gtk::Box,
    pub gpu_fan_page: gtk::Box,
    pub power_mode_row: gtk::Box,
    pub power_mode_dropdown: gtk::DropDown,
    pub pl1: LabeledSlider,
    pub pl2: LabeledSlider,
    pub apply_limits_switch: gtk::Switch,
    pub core_clock: LabeledSlider,
    pub mem_clock: LabeledSlider,
    pub gpu_power: LabeledSlider,
    pub boost: LabeledSlider,
    pub temp_target: LabeledSlider,
    pub gpu_unsupported_notice: gtk::Label,
    pub cpu_fan: Rc<FanCurveEditor>,
    pub gpu_fan: Rc<FanCurveEditor>,
    suppress: Rc<Suppress>,
    toast: ToastCb,
    cfg: Cfg,
    save: SaveCb,
    nvidia: RefCell<NvidiaProbe>,
    debouncers: Vec<Debouncer>,
}

#[derive(Default, Clone)]
struct NvidiaProbe {
    done: bool,
    clock_ok: bool,
    power_bounds: Option<(f64, f64, f64)>,
    power_ok: bool,
}

/// Indices into `FansPowerWindow::debouncers`.
const D_PL1: usize = 0;
const D_PL2: usize = 1;
const D_CORE: usize = 2;
const D_MEM: usize = 3;
const D_GPU_POWER: usize = 4;
const D_BOOST: usize = 5;
const D_TEMP: usize = 6;

impl FansPowerWindow {
    pub fn new(app: Option<&adw::Application>, toast: ToastCb, cfg: Cfg, save: SaveCb) -> Rc<Self> {
        let window = adw::Window::new();
        if let Some(app) = app {
            window.set_application(Some(app));
        }
        // Bigger than the old 440x620, which clipped slider value labels at the right edge.
        window.set_default_size(560, 760);
        window.set_title(Some("Fans and Power"));

        let toolbar_view = adw::ToolbarView::new();
        let header = adw::HeaderBar::builder()
            .title_widget(&adw::WindowTitle::new("Fans and Power", &device_name()))
            .build();
        toolbar_view.add_top_bar(&header);
        window.set_content(Some(&toolbar_view));

        let outer = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).build();
        outer.set_margin_top(12);
        outer.set_margin_bottom(14);
        outer.set_margin_start(14);
        outer.set_margin_end(14);

        // The fan curve is genuinely per profile (asusctl stores one per Silent/Balanced/Turbo);
        // the power limits are not. So this selector exists only right above the chart it
        // affects: one shared widget, re-parented into whichever fan page is showing.
        let mode_row = gtk::Box::builder().spacing(8).build();
        mode_row.append(&gtk::Label::builder().label("Fan curve for profile:").halign(gtk::Align::Start).build());
        let profile_names: Vec<&str> = PROFILES.iter().map(|p| profile_label(p)).collect();
        let mode_dropdown = gtk::DropDown::from_strings(&profile_names);
        mode_dropdown.set_hexpand(true);
        mode_row.append(&mode_dropdown);
        mode_row.set_visible(false);

        let nav_row = gtk::Box::builder().spacing(6).homogeneous(true).hexpand(true).build();
        let cpu_nav = gtk::ToggleButton::builder().label("CPU").active(true).hexpand(true).build();
        let gpu_nav = gtk::ToggleButton::builder().label("GPU").hexpand(true).build();
        gpu_nav.set_group(Some(&cpu_nav));
        nav_row.append(&cpu_nav);
        nav_row.append(&gpu_nav);
        outer.append(&nav_row);

        let stack = gtk::Stack::new();
        let s = stack.clone();
        cpu_nav.connect_toggled(move |b| {
            if b.is_active() {
                s.set_visible_child_name("cpu");
            }
        });
        let s = stack.clone();
        gpu_nav.connect_toggled(move |b| {
            if b.is_active() {
                s.set_visible_child_name("gpu");
            }
        });

        // CPU page: Power and Fan Curve are separate sub-views.
        let cpu = sub_view_page("cpu");

        // G-Helper's "Windows Power Mode" combo, backed by power-profiles-daemon: the same pairing
        // apply_profile() sets automatically, exposed on its own since the two need not move in
        // lockstep.
        let power_mode_row = gtk::Box::builder().spacing(8).build();
        power_mode_row.append(&gtk::Label::builder().label("Power Mode").hexpand(true).halign(gtk::Align::Start).build());
        let power_mode_dropdown = gtk::DropDown::from_strings(&["Power Saver", "Balanced", "Performance"]);
        power_mode_dropdown.set_hexpand(true);
        power_mode_row.append(&power_mode_dropdown);
        cpu.power_page.append(&power_mode_row);

        // Always shown and interactive even though this firmware reports them unavailable: a
        // write against unsupported firmware just reports a clear toast instead.
        let pl1 = labeled_slider("PL1 (CPU sustained)", 10.0, 90.0, "W");
        let pl2 = labeled_slider("PL2 (CPU long boost)", 10.0, 150.0, "W");
        let suppress = Rc::new(Suppress::new(false));
        let apply_suppress = suppress.clone();
        let apply_cfg = cfg.clone();
        let apply_save = save.clone();
        let apply_limits_row = switch_row("Apply Power Limits", move |_, state| {
            if !apply_suppress.get() {
                config::set(&apply_cfg, "power_limits", "apply_limits", json!(state));
                apply_save();
            }
            glib::Propagation::Proceed
        });
        let power_limits_box = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).build();
        power_limits_box.append(&section_label("Power Limits"));
        power_limits_box.append(&pl1.root);
        power_limits_box.append(&pl2.root);
        power_limits_box.append(&apply_limits_row.root);
        cpu.power_page.append(&power_limits_box);

        let get_profile: Rc<dyn Fn() -> String> = {
            let dropdown = mode_dropdown.clone();
            Rc::new(move || PROFILES.get(dropdown.selected() as usize).copied().unwrap_or("Quiet").to_string())
        };
        let cpu_fan = FanCurveEditor::new("cpu", toast.clone(), get_profile.clone());
        cpu.fan_page.append(&cpu_fan.root);
        stack.add_named(&cpu.root, Some("cpu"));

        // GPU page: same Power / Fan Curve split.
        let gpu = sub_view_page("gpu");
        // Always shown, never hidden behind an availability probe: Core/Memory Clock Offset and
        // GPU Power are unsupported on THIS GPU/session (no Xorg + Coolbits, vBIOS power lock),
        // but that is this laptop's limitation to hit when the user tries, not the app's decision.
        let core_clock = labeled_slider("Core Clock Offset", -200.0, 300.0, " MHz");
        let mem_clock = labeled_slider("Memory Clock Offset (VRAM)", -500.0, 1500.0, " MHz");
        let gpu_power = labeled_slider("GPU Power (TGP)", 1.0, 150.0, "W");
        let boost = labeled_slider("Dynamic Boost", 0.0, 25.0, "W");
        let temp_target = labeled_slider("Thermal Target", 60.0, 100.0, "°C");
        let gpu_unsupported_notice = gtk::Label::new(Some(
            "Core/Memory Clock Offset and GPU Power need Xorg + Coolbits and/or\n\
             firmware support this laptop's GPU doesn't have -- shown anyway in\n\
             case that changes; a failed change reports why below.",
        ));
        gpu_unsupported_notice.add_css_class("rog-empty");
        gpu_unsupported_notice.set_halign(gtk::Align::Start);
        let gpu_limits_box = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).build();
        gpu_limits_box.append(&section_label("GPU Settings"));
        gpu_limits_box.append(&core_clock.root);
        gpu_limits_box.append(&mem_clock.root);
        gpu_limits_box.append(&gpu_power.root);
        gpu_limits_box.append(&boost.root);
        gpu_limits_box.append(&temp_target.root);
        gpu_limits_box.append(&gpu_unsupported_notice);
        gpu.power_page.append(&gpu_limits_box);

        let gpu_fan = FanCurveEditor::new("gpu", toast.clone(), get_profile);
        gpu.fan_page.append(&gpu_fan.root);
        stack.add_named(&gpu.root, Some("gpu"));

        let scroller = gtk::ScrolledWindow::new();
        scroller.set_child(Some(&stack));
        scroller.set_vexpand(true);
        outer.append(&scroller);
        toolbar_view.set_content(Some(&outer));

        let this = Rc::new(Self {
            window,
            mode_row,
            mode_dropdown,
            cpu_nav,
            gpu_nav,
            cpu_fan_btn: cpu.fan_btn,
            gpu_fan_btn: gpu.fan_btn,
            cpu_fan_page: cpu.fan_page,
            gpu_fan_page: gpu.fan_page,
            power_mode_row,
            power_mode_dropdown,
            pl1,
            pl2,
            apply_limits_switch: apply_limits_row.switch,
            core_clock,
            mem_clock,
            gpu_power,
            boost,
            temp_target,
            gpu_unsupported_notice,
            cpu_fan,
            gpu_fan,
            suppress,
            toast,
            cfg,
            save,
            nvidia: RefCell::new(NvidiaProbe::default()),
            // Gtk.Scale fires value-changed continuously through a drag, and a burst of
            // concurrent asusctl/nvidia calls was confirmed to queue long enough to hit the
            // command timeout and surface as a spurious error toast.
            debouncers: (0..7).map(|_| Debouncer::new(Debouncer::QUIET_MS)).collect(),
        });
        this.wire();
        this.refresh_power_limits();
        this.update_mode_selector_visibility();
        this
    }

    fn wire(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        let update = move |_: &gtk::ToggleButton| {
            if let Some(s) = weak.upgrade() {
                s.update_mode_selector_visibility();
            }
        };
        self.cpu_nav.connect_toggled(update.clone());
        self.gpu_nav.connect_toggled(update.clone());
        self.cpu_fan_btn.connect_toggled(update.clone());
        self.gpu_fan_btn.connect_toggled(update);

        let weak = Rc::downgrade(self);
        self.mode_dropdown.connect_selected_notify(move |_| {
            if let Some(s) = weak.upgrade() {
                s.cpu_fan.load_curve();
                s.gpu_fan.load_curve();
            }
        });
        let weak = Rc::downgrade(self);
        self.power_mode_dropdown.connect_selected_notify(move |d| {
            if let Some(s) = weak.upgrade() {
                s.on_power_mode_changed(d);
            }
        });

        // Slider -> debouncer -> apply.
        let sliders: [SliderBinding; 7] = [
            (&self.pl1, D_PL1, Self::apply_pl1),
            (&self.pl2, D_PL2, Self::apply_pl2),
            (&self.core_clock, D_CORE, Self::apply_core_clock),
            (&self.mem_clock, D_MEM, Self::apply_mem_clock),
            (&self.gpu_power, D_GPU_POWER, Self::apply_gpu_power),
            (&self.boost, D_BOOST, Self::apply_boost),
            (&self.temp_target, D_TEMP, Self::apply_temp),
        ];
        for (slider, idx, apply) in sliders {
            let weak = Rc::downgrade(self);
            self.debouncers[idx].bind(move || {
                if let Some(s) = weak.upgrade() {
                    apply(&s);
                }
            });
            let weak = Rc::downgrade(self);
            slider.scale.connect_value_changed(move |_| {
                if let Some(s) = weak.upgrade() {
                    if !s.suppress.get() {
                        s.debouncers[idx].trigger();
                    }
                }
            });
        }
    }

    /// Keeps the shared profile selector parented inside whichever fan page (CPU or GPU) is
    /// showing, and hidden while looking at a Power page. Both fan pages want the very same
    /// widget (one selection drives both curves), so it is re-parented rather than duplicated.
    pub fn update_mode_selector_visibility(&self) {
        let active_fan_page = if self.cpu_nav.is_active() && self.cpu_fan_btn.is_active() {
            Some(&self.cpu_fan_page)
        } else if self.gpu_nav.is_active() && self.gpu_fan_btn.is_active() {
            Some(&self.gpu_fan_page)
        } else {
            None
        };
        let Some(page) = active_fan_page else {
            self.mode_row.set_visible(false);
            return;
        };
        let current_parent = self.mode_row.parent();
        if current_parent.as_ref() != Some(page.upcast_ref::<gtk::Widget>()) {
            if let Some(parent) = current_parent.and_then(|p| p.downcast::<gtk::Box>().ok()) {
                parent.remove(&self.mode_row);
            }
            page.prepend(&self.mode_row);
        }
        self.mode_row.set_visible(true);
    }

    pub fn refresh(self: &Rc<Self>) {
        self.refresh_power_limits();
        self.cpu_fan.load_curve();
        self.gpu_fan.load_curve();
    }

    /// Prefers the real hardware reading; when there is none (firmware reports the attribute
    /// unavailable) falls back to what the user last set, since this window is rebuilt fresh on
    /// every open and would otherwise always reset.
    fn sync_slider(&self, slider: &LabeledSlider, debouncer: usize, sysfs_path: &str, cfg_key: &str) {
        // A value picked a moment ago is still waiting out its debounce: syncing would replace it
        // and the pending apply would send the old value.
        if self.debouncers[debouncer].is_pending() {
            return;
        }
        if let Some(v) = read_sysfs_int(Some(sysfs_path)) {
            slider.scale.set_value(v as f64);
        } else if let Some(v) = config::get_f64(&self.cfg, "power_limits", cfg_key) {
            slider.scale.set_value(v);
        }
    }

    /// Only fast sysfs reads happen here; everything that shells out runs in the background.
    pub fn refresh_power_limits(self: &Rc<Self>) {
        self.suppress.during(|| {
            self.sync_slider(&self.pl1, D_PL1, PPT_PL1_PATH, "pl1");
            self.sync_slider(&self.pl2, D_PL2, PPT_PL2_PATH, "pl2");
            self.sync_slider(&self.boost, D_BOOST, NV_BOOST_PATH, "boost");
            self.sync_slider(&self.temp_target, D_TEMP, NV_TEMP_TARGET_PATH, "temp_target");
            self.apply_limits_switch.set_active(config::get_bool(&self.cfg, "power_limits", "apply_limits"));
        });
        self.refresh_power_mode_async();
        if self.nvidia.borrow().done {
            self.apply_nvidia_state();
        } else {
            self.probe_nvidia_async();
        }
    }

    fn refresh_power_mode_async(self: &Rc<Self>) {
        if !tool_available("powerprofilesctl") {
            self.power_mode_row.set_visible(false);
            return;
        }
        let weak = Rc::downgrade(self);
        spawn_bg(
            || {
                run_capture(&["powerprofilesctl", "get"], Duration::from_secs(5))
                    .map(|o| o.stdout.trim().to_string())
                    .unwrap_or_default()
            },
            move |current| {
                if let Some(s) = weak.upgrade() {
                    s.suppress.during(|| {
                        s.power_mode_row.set_visible(true);
                        if let Some(i) = POWER_PLANS.iter().position(|p| *p == current) {
                            s.power_mode_dropdown.set_selected(i as u32);
                        }
                    });
                }
            },
        );
    }

    /// The probes have a real (if brief) subprocess cost and, for the power limit, a genuine
    /// no-op write: run once in the background and cache, not on every refresh.
    fn probe_nvidia_async(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        spawn_bg(
            || {
                let clock_ok = nvidia_clock_offset_available();
                let bounds = nvidia_power_limit_bounds();
                let power_ok = bounds.is_some_and(|b| nvidia_power_limit_settable(b.0));
                NvidiaProbe { done: true, clock_ok, power_bounds: bounds, power_ok }
            },
            move |probe| {
                if let Some(s) = weak.upgrade() {
                    *s.nvidia.borrow_mut() = probe;
                    s.apply_nvidia_state();
                }
            },
        );
    }

    /// Always visible and interactive: the probe only decides the starting value and whether the
    /// "may not work here" note shows, never whether a slider exists.
    fn apply_nvidia_state(&self) {
        let probe = self.nvidia.borrow().clone();
        self.suppress.during(|| {
            // nvidia-settings cannot even read these back on this session, so fall back to what
            // the user last set instead of a hard-coded 0.
            // The probe lands asynchronously, so the user may already be dragging: a slider whose
            // change is still waiting out its debounce keeps the value they picked.
            let idle = |idx: usize| !self.debouncers[idx].is_pending();
            let saved = |key: &str| config::get_f64(&self.cfg, "power_limits", key).filter(|v| *v != 0.0).unwrap_or(0.0);
            if idle(D_CORE) {
                self.core_clock.scale.set_value(saved("core_clock"));
            }
            if idle(D_MEM) {
                self.mem_clock.scale.set_value(saved("mem_clock"));
            }
            if let Some((_, lo, hi)) = probe.power_bounds {
                self.gpu_power.scale.set_range(lo, hi);
            }
            if idle(D_GPU_POWER) {
                if let Some(v) = probe.power_bounds.map(|b| b.0).or_else(|| config::get_f64(&self.cfg, "power_limits", "gpu_power")) {
                    self.gpu_power.scale.set_value(v);
                }
            }
            self.gpu_unsupported_notice.set_visible(!(probe.clock_ok || probe.power_ok));
        });
    }

    fn on_power_mode_changed(&self, dropdown: &gtk::DropDown) {
        if self.suppress.get() || !tool_available("powerprofilesctl") {
            return;
        }
        if let Some(plan) = POWER_PLANS.get(dropdown.selected() as usize) {
            run_cmd(&["powerprofilesctl", "set", plan], Some(self.report()));
        }
    }

    fn report(&self) -> DoneCb {
        let toast = self.toast.clone();
        Box::new(move |ok, err| {
            if !ok {
                toast(&failure_text("Command failed", &err));
            }
        })
    }

    fn remember(&self, key: &str, value: serde_json::Value) {
        config::set(&self.cfg, "power_limits", key, value);
        (self.save)();
    }

    fn apply_pl1(&self) {
        let value = self.pl1.scale.value() as i64;
        self.remember("pl1", json!(value));
        run_cmd(&["asusctl", "armoury", "set", "ppt_pl1_spl", &value.to_string()], Some(self.report()));
    }

    fn apply_pl2(&self) {
        let value = self.pl2.scale.value() as i64;
        self.remember("pl2", json!(value));
        run_cmd(&["asusctl", "armoury", "set", "ppt_pl2_sppt", &value.to_string()], Some(self.report()));
    }

    fn apply_core_clock(&self) {
        let value = self.core_clock.scale.value() as i64;
        self.remember("core_clock", json!(value));
        apply_core_clock_offset(value, Some(self.report()));
    }

    fn apply_mem_clock(&self) {
        let value = self.mem_clock.scale.value() as i64;
        self.remember("mem_clock", json!(value));
        apply_mem_clock_offset(value, Some(self.report()));
    }

    fn apply_gpu_power(&self) {
        let value = self.gpu_power.scale.value();
        self.remember("gpu_power", json!(value));
        apply_gpu_power_limit(value, Some(self.report()));
    }

    fn apply_boost(&self) {
        let value = self.boost.scale.value() as i64;
        self.remember("boost", json!(value));
        run_cmd(&["asusctl", "armoury", "set", "nv_dynamic_boost", &value.to_string()], Some(self.report()));
    }

    fn apply_temp(&self) {
        let value = self.temp_target.scale.value() as i64;
        self.remember("temp_target", json!(value));
        run_cmd(&["asusctl", "armoury", "set", "nv_temp_target", &value.to_string()], Some(self.report()));
    }
}
