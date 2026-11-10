//! Shared test plumbing: the fail-closed command guard, hooks, and a main-loop pump.

use std::cell::RefCell;
use std::io;
use std::rc::Rc;
use std::sync::Mutex;
use std::time::Duration;

use gtk::gio::prelude::ApplicationExt;
use gtk::{gio, glib};
use rog_control::config::{self, Cfg};
use rog_control::sys::{hook_run_cmd, push_capture_hook, CaptureGuard, CmdOut, RunCmdGuard};

static BLOCKED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Invocations that only read hardware state: these (and nothing else) may reach a real tool.
pub fn is_read_only(argv: &[String]) -> bool {
    let a: Vec<&str> = argv.iter().map(String::as_str).collect();
    match a.as_slice() {
        ["hyprctl", "monitors", "-j"] => true,
        ["nvidia-smi", q, ..] if q.starts_with("--query-gpu=") => true,
        ["nvidia-smi", "-q", "-d", "POWER"] => true,
        ["nvidia-settings", "-q", _] => true,
        ["systemctl", "--user", "is-enabled", "rog-control.service"] => true,
        ["powerprofilesctl", "get"] => true,
        ["asusctl", "fan-curve", "--mod-profile", _] => true,
        _ => false,
    }
}

/// Fail-closed: every command that is not a plain read is answered with success and recorded
/// instead of executed, so a test can never write to real hardware (NFR-1).
pub fn install_guard() -> CaptureGuard {
    push_capture_hook(|argv| {
        if is_read_only(argv) {
            None
        } else {
            BLOCKED.lock().unwrap_or_else(|e| e.into_inner()).push(argv.join(" "));
            Some(Ok(CmdOut { ok: true, ..CmdOut::default() }))
        }
    })
}

pub fn blocked_commands() -> Vec<String> {
    BLOCKED.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

thread_local! {
    static SKIPPED: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Marks the running test as skipped because a fixture it needs is absent on this machine. Use it
/// as `return skip("why")`: the runner then reports the test as skipped, never as passed, so a
/// missing fixture cannot masquerade as a green test.
pub fn skip(reason: &str) {
    SKIPPED.with(|s| *s.borrow_mut() = Some(reason.to_string()));
}

/// The skip reason of the test that just returned, if it skipped itself.
pub fn take_skip() -> Option<String> {
    SKIPPED.with(|s| s.borrow_mut().take())
}

/// Drain pending GLib main-loop work (debouncers, async probes) for `ms` milliseconds.
pub fn pump(ms: u64) {
    let main_loop = glib::MainLoop::new(None, false);
    let quit = main_loop.clone();
    glib::timeout_add_local_once(Duration::from_millis(ms), move || quit.quit());
    main_loop.run();
}

/// The application id the construction test used to register as is. An application id is a name on
/// the session bus shared with every other test run and with the evals: if another process owns it,
/// `register()` still returns Ok but as a *remote* instance, `startup` never runs and
/// `window.set_application` crashes the whole runner (SEGV, no summary). Never register it bare.
pub const CONSTRUCTION_APP_BASE: &str = "com.test.ConstructionCheck";

/// `base` made unique to this process, for any bus name a test claims on a bus other runs share.
pub fn unique_app_id(base: &str) -> String {
    format!("{base}.p{}", std::process::id())
}

/// Runner mode for a helper process (see `startup::app_id__...`): own `id` as a registered
/// application, tell nobody, and serve until killed. Never returns.
pub fn hold_application_id(id: &str) -> ! {
    let app = gio::Application::new(Some(id), gio::ApplicationFlags::empty());
    app.register(gio::Cancellable::NONE).expect("register the held application");
    glib::MainLoop::new(None, false).run();
    std::process::exit(0)
}

pub type Calls = Rc<RefCell<Vec<Vec<String>>>>;

/// `mock.patch(run_cmd)`: records every command handed to `run_cmd` instead of running it.
pub fn record_run_cmd() -> (Calls, RunCmdGuard) {
    let calls: Calls = Rc::new(RefCell::new(Vec::new()));
    let sink = calls.clone();
    let guard = hook_run_cmd(move |argv| sink.borrow_mut().push(argv.to_vec()));
    (calls, guard)
}

/// Answers commands at the lowest level: `reply` returns `Some(output)` to answer, `None` to
/// fall through (to the guard). Unlike [`record_run_cmd`] this runs the real `run_cmd` machinery,
/// so completion callbacks fire.
pub fn fake_capture(reply: impl Fn(&[String]) -> Option<CmdOut> + Send + Sync + 'static) -> CaptureGuard {
    push_capture_hook(move |argv| reply(argv).map(Ok::<CmdOut, io::Error>))
}

pub fn ok_out(stdout: &str) -> CmdOut {
    CmdOut { ok: true, stdout: stdout.to_string(), stderr: String::new() }
}

pub fn failed_out(stderr: &str) -> CmdOut {
    CmdOut { ok: false, stdout: String::new(), stderr: stderr.to_string() }
}

pub fn make_cfg() -> Cfg {
    config::new_cfg(config::merge_defaults(serde_json::json!({}), &config::defaults()))
}

pub type Toasts = Rc<RefCell<Vec<String>>>;
pub type ToastFn = Rc<dyn Fn(&str)>;

pub fn toast_sink() -> (Toasts, ToastFn) {
    let toasts = Rc::new(RefCell::new(Vec::new()));
    let sink = toasts.clone();
    (toasts, Rc::new(move |t: &str| sink.borrow_mut().push(t.to_string())))
}

pub fn no_toast() -> Rc<dyn Fn(&str)> {
    Rc::new(|_: &str| {})
}

pub fn no_save() -> Rc<dyn Fn()> {
    Rc::new(|| {})
}

pub fn on_ac() -> Rc<dyn Fn() -> bool> {
    Rc::new(|| false)
}

pub fn cmds_with(calls: &Calls, needle: &str) -> Vec<Vec<String>> {
    calls.borrow().iter().filter(|c| c.iter().any(|a| a == needle)).cloned().collect()
}

/// A tempdir removed on drop.
pub struct TempDir(pub std::path::PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!("rog-control-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
