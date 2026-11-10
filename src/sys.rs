//! Process, sysfs and main-thread plumbing shared by everything that talks to hardware.
//!
//! Every external command goes through exactly one function, [`run_capture`], and every
//! sysfs read through [`read_sysfs`]. That is what lets the test-suite (and the evals' fake
//! tools) intercept the whole app: tools are resolved by bare name through `PATH`, and the
//! hooks below let a test replace what a command or a sysfs file "returns".

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use gtk::glib;

/// Result of a finished command. `ok` is "exit status 0".
#[derive(Clone, Debug, Default)]
pub struct CmdOut {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Completion callback of [`run_cmd`]: `(ok, error text)`. Always runs on the GTK main thread.
pub type DoneCb = Box<dyn FnOnce(bool, String) + 'static>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

type CaptureHook = Arc<dyn Fn(&[String]) -> Option<io::Result<CmdOut>> + Send + Sync>;
static CAPTURE_HOOKS: Mutex<Vec<(u64, CaptureHook)>> = Mutex::new(Vec::new());
static HOOK_ID: Mutex<u64> = Mutex::new(0);

fn next_hook_id() -> u64 {
    let mut id = lock(&HOOK_ID);
    *id += 1;
    *id
}

/// Removes its hook when dropped.
pub struct CaptureGuard(u64);

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        lock(&CAPTURE_HOOKS).retain(|(id, _)| *id != self.0);
    }
}

/// Test seam: `hook` sees every command before it is executed. Returning `Some(result)` answers
/// the command without running it; `None` lets the next older hook (and finally the real
/// process) handle it. Hooks are global because commands run on worker threads.
pub fn push_capture_hook(hook: impl Fn(&[String]) -> Option<io::Result<CmdOut>> + Send + Sync + 'static) -> CaptureGuard {
    let id = next_hook_id();
    lock(&CAPTURE_HOOKS).push((id, Arc::new(hook)));
    CaptureGuard(id)
}

fn hooked(argv: &[String]) -> Option<io::Result<CmdOut>> {
    let hooks: Vec<CaptureHook> = lock(&CAPTURE_HOOKS).iter().rev().map(|(_, h)| h.clone()).collect();
    hooks.iter().find_map(|h| h(argv))
}

/// Runs `argv` (resolved through `PATH`), capturing stdout and stderr. Blocks: call it off the
/// GTK main thread. Kills the child and fails with `TimedOut` after `timeout`.
pub fn run_capture(argv: &[&str], timeout: Duration) -> io::Result<CmdOut> {
    let owned: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
    if let Some(result) = hooked(&owned) {
        return result;
    }
    exec(&owned, timeout)
}

fn exec(argv: &[String], timeout: Duration) -> io::Result<CmdOut> {
    let (prog, args) = argv
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty command"))?;
    let mut child = Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + timeout;
    let (tx, rx) = mpsc::channel::<(bool, Vec<u8>)>();
    // Detached readers: a tool that leaves a grandchild holding the pipe must not wedge us.
    for (is_stdout, pipe) in [
        (true, child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>)),
        (false, child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>)),
    ] {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            let _ = tx.send((is_stdout, buf));
        });
    }
    drop(tx);
    let timed_out = || {
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!("Command '{}' timed out after {} seconds", argv.join(" "), timeout.as_secs()),
        )
    };
    let status = loop {
        match child.try_wait()? {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(timed_out());
            }
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    };
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    for _ in 0..2 {
        let wait = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(wait) {
            Ok((true, b)) => stdout = b,
            Ok((false, b)) => stderr = b,
            Err(_) => return Err(timed_out()),
        }
    }
    Ok(CmdOut {
        ok: status.success(),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

type RunCmdHook = Rc<dyn Fn(&[String])>;

thread_local! {
    static RUN_CMD_HOOK: RefCell<Option<RunCmdHook>> = const { RefCell::new(None) };
}

/// Restores the previous `run_cmd` hook when dropped.
pub struct RunCmdGuard(Option<RunCmdHook>);

impl Drop for RunCmdGuard {
    fn drop(&mut self) {
        RUN_CMD_HOOK.with(|h| *h.borrow_mut() = self.0.take());
    }
}

/// Test seam (main thread only): while the guard lives, [`run_cmd`] hands every command to
/// `hook` instead of running it. Like the Python suite's `mock.patch(run_cmd)`, `on_done` is
/// never called.
pub fn hook_run_cmd(hook: impl Fn(&[String]) + 'static) -> RunCmdGuard {
    RunCmdGuard(RUN_CMD_HOOK.with(|h| h.borrow_mut().replace(Rc::new(hook))))
}

/// Fire-and-forget a command off the GTK main thread; `on_done(ok, err)` reports back on it.
pub fn run_cmd(cmd: &[&str], on_done: Option<DoneCb>) {
    let owned: Vec<String> = cmd.iter().map(|s| s.to_string()).collect();
    if let Some(hook) = RUN_CMD_HOOK.with(|h| h.borrow().clone()) {
        hook(&owned);
        return;
    }
    let work = move || {
        let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
        match run_capture(&refs, Duration::from_secs(10)) {
            Ok(out) if out.ok => (true, String::new()),
            Ok(out) => (false, out.stderr.trim().to_string()),
            Err(e) => (false, e.to_string()),
        }
    };
    match on_done {
        Some(done) => spawn_bg(work, move |(ok, err)| done(ok, err)),
        None => {
            std::thread::spawn(move || {
                work();
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Background work that reports back on the main thread
// ---------------------------------------------------------------------------

type Pending = Box<dyn FnOnce(Box<dyn Any + Send>)>;

thread_local! {
    static PENDING: RefCell<HashMap<u64, Pending>> = RefCell::new(HashMap::new());
    static NEXT_PENDING: Cell<u64> = const { Cell::new(0) };
}

/// Runs `work` on a worker thread and then `done(result)` on the GTK main thread. `done` may
/// capture widgets (it never leaves the main thread); only `work` and its result must be
/// `Send`. Must itself be called from the main thread.
pub fn spawn_bg<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static, done: impl FnOnce(T) + 'static) {
    let id = NEXT_PENDING.with(|n| {
        n.set(n.get() + 1);
        n.get()
    });
    PENDING.with(|p| {
        p.borrow_mut().insert(
            id,
            Box::new(move |boxed: Box<dyn Any + Send>| {
                if let Ok(value) = boxed.downcast::<T>() {
                    done(*value);
                }
            }),
        )
    });
    std::thread::spawn(move || {
        let result: Box<dyn Any + Send> = Box::new(work());
        glib::idle_add_once(move || {
            if let Some(cb) = PENDING.with(|p| p.borrow_mut().remove(&id)) {
                cb(result);
            }
        });
    });
}

// ---------------------------------------------------------------------------
// sysfs
// ---------------------------------------------------------------------------

type SysfsHook = Rc<dyn Fn(&str) -> Option<String>>;
type ListDirHook = Rc<dyn Fn(&str) -> Option<Vec<String>>>;

thread_local! {
    static SYSFS_HOOK: RefCell<Option<SysfsHook>> = const { RefCell::new(None) };
    static LISTDIR_HOOK: RefCell<Option<ListDirHook>> = const { RefCell::new(None) };
}

/// Restores the previous sysfs hooks when dropped.
pub struct SysfsGuard(Option<SysfsHook>, Option<ListDirHook>);

impl Drop for SysfsGuard {
    fn drop(&mut self) {
        SYSFS_HOOK.with(|h| *h.borrow_mut() = self.0.take());
        LISTDIR_HOOK.with(|h| *h.borrow_mut() = self.1.take());
    }
}

/// Test seam (main thread only): replaces what [`read_sysfs`] and [`list_dir`] see.
pub fn hook_sysfs(
    read: impl Fn(&str) -> Option<String> + 'static,
    list: impl Fn(&str) -> Option<Vec<String>> + 'static,
) -> SysfsGuard {
    SysfsGuard(
        SYSFS_HOOK.with(|h| h.borrow_mut().replace(Rc::new(read))),
        LISTDIR_HOOK.with(|h| h.borrow_mut().replace(Rc::new(list))),
    )
}

/// Reads a sysfs file, trimmed. Never fails: a missing file, an unreadable one and a `None` path
/// (a resolver that found no sensor) all give `None`.
pub fn read_sysfs(path: Option<&str>) -> Option<String> {
    let path = path.filter(|p| !p.is_empty())?;
    if let Some(hook) = SYSFS_HOOK.with(|h| h.borrow().clone()) {
        return hook(path);
    }
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

/// [`read_sysfs`] parsed as an integer; non-numeric text such as "unavailable" gives `None`.
pub fn read_sysfs_int(path: Option<&str>) -> Option<i64> {
    read_sysfs(path)?.parse().ok()
}

/// Directory entry names, or `None` when the directory cannot be read.
pub fn list_dir(path: &str) -> Option<Vec<String>> {
    if let Some(hook) = LISTDIR_HOOK.with(|h| h.borrow().clone()) {
        return hook(path);
    }
    let rd = std::fs::read_dir(path).ok()?;
    Some(rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
}

// ---------------------------------------------------------------------------
// Tool lookup
// ---------------------------------------------------------------------------

static TOOL_OVERRIDES: Mutex<Vec<(String, bool)>> = Mutex::new(Vec::new());

/// Test seam: pretend `tool` is (not) installed.
pub fn override_tool(tool: &str, present: bool) {
    let mut v = lock(&TOOL_OVERRIDES);
    v.retain(|(t, _)| t != tool);
    v.push((tool.to_string(), present));
}

/// Forget every [`override_tool`].
pub fn clear_tool_overrides() {
    lock(&TOOL_OVERRIDES).clear();
}

/// `shutil.which`: is `tool` an executable file somewhere on `PATH`? The tool is still invoked
/// by bare name; this only decides whether to bother (e.g. no `powerprofilesctl`, no pairing).
pub fn tool_available(tool: &str) -> bool {
    if let Some((_, present)) = lock(&TOOL_OVERRIDES).iter().find(|(t, _)| t == tool) {
        return *present;
    }
    use std::os::unix::fs::PermissionsExt;
    let Some(path) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&path).any(|dir| {
        std::fs::metadata(dir.join(tool)).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
    })
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// `$HOME` (Python's `~`): config and scheme paths hang off it so `HOME=<tmp>` isolates a run.
pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

pub fn home_join(rel: &str) -> PathBuf {
    home_dir().join(rel)
}

/// Python's `str.capitalize()`: first character upper-case, the rest lower-case.
pub fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
        None => String::new(),
    }
}

/// Python's `round()` (ties to even), which differs from `f64::round` on exact halves.
pub fn py_round(x: f64) -> i64 {
    x.round_ties_even() as i64
}

/// First `n` characters (Python's `s[:n]`), safe on multi-byte text.
pub fn truncate_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

pub fn path_exists(p: &Path) -> bool {
    p.exists()
}
