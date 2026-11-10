#![allow(non_snake_case)]
//! Black-box startup: run the real binary (what systemd's `ExecStart` and the launcher entry both
//! run) on a private session bus and time it until it is genuinely up.
//!
//! "Up" = it answers a call on its control interface from its main loop (and a `Peer.Ping`), i.e.
//! after startup has fully returned; the bus name appearing earlier is reported too but is not
//! "up". The child runs with a throwaway `$HOME` (an empty config: no automation) and `PATH`
//! pointing at do-nothing stand-ins for every hardware tool, so a test launch can never change
//! real profile / GPU / screen / keyboard state.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use gtk::gio::{self, prelude::*};
use rog_control::control::{CONTROL_NAME, CONTROL_PATH};

use crate::common::TempDir;

const BUDGET: Duration = Duration::from_secs(5);
const RUNS: usize = 3;

// No <servicedir>: the default session config would let GTK's portal lookups D-Bus-activate real
// xdg-desktop-portal services that outlive the test bus.
const BUS_CONF: &str = r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen><auth>EXTERNAL</auth>
<policy context="default"><allow send_destination="*" eavesdrop="true"/><allow eavesdrop="true"/><allow own="*"/></policy></busconfig>"#;

pub fn ported_tests() -> Vec<(&'static str, fn())> {
    vec![
        ("app_startup__starts_and_answers_within_budget", app_startup__starts_and_answers_within_budget as fn()),
        ("app_startup__launching_again_while_running_does_not_hang", app_startup__launching_again_while_running_does_not_hang as fn()),
    ]
}

struct PrivateBus {
    daemon: Child,
    address: String,
    _dir: TempDir,
}

impl PrivateBus {
    fn start() -> Option<Self> {
        let dir = TempDir::new("bus");
        let conf = dir.0.join("bus.conf");
        std::fs::write(&conf, BUS_CONF).ok()?;
        let mut daemon = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", conf.display()))
            .arg("--nofork")
            .arg("--print-address=1")
            .stdout(Stdio::piped())
            .spawn()
            .ok()?;
        let mut address = String::new();
        BufReader::new(daemon.stdout.take()?).read_line(&mut address).ok()?;
        Some(Self { daemon, address: address.trim().to_string(), _dir: dir })
    }

    fn connect(&self) -> gio::DBusConnection {
        gio::DBusConnection::for_address_sync(
            &self.address,
            gio::DBusConnectionFlags::AUTHENTICATION_CLIENT | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
            None,
            gio::Cancellable::NONE,
        )
        .expect("connect to the private bus")
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

/// `$HOME`, `PATH` and the bus for a child instance; kept alive for the child's lifetime.
struct ChildEnv {
    home: TempDir,
    tools: TempDir,
}

impl ChildEnv {
    fn new() -> Self {
        let tools = TempDir::new("tools");
        for tool in ["asusctl", "nvidia-smi", "nvidia-settings", "powerprofilesctl", "hyprctl", "systemctl", "notify-send"] {
            let path = tools.0.join(tool);
            let mut f = std::fs::File::create(&path).expect("create stand-in tool");
            f.write_all(b"#!/bin/sh\nexit 0\n").expect("write stand-in tool");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        Self { home: TempDir::new("home"), tools }
    }

    fn launch(&self, address: &str, stderr: std::fs::File) -> Child {
        Command::new(env!("CARGO_BIN_EXE_rog-control"))
            .env("DBUS_SESSION_BUS_ADDRESS", address)
            .env("HOME", &self.home.0)
            .env("PATH", &self.tools.0)
            .stdout(Stdio::null())
            .stderr(stderr)
            .spawn()
            .expect("launch rog-control")
    }
}

fn name_has_owner(conn: &gio::DBusConnection, name: &str) -> bool {
    conn.call_sync(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "NameHasOwner",
        Some(&(name,).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        1000,
        gio::Cancellable::NONE,
    )
    .ok()
    .and_then(|r| r.child_value(0).get::<bool>())
    .unwrap_or(false)
}

fn stderr_tail(file: &mut std::fs::File) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let mut text = String::new();
    let _ = file.seek(SeekFrom::Start(0));
    let _ = file.read_to_string(&mut text);
    text.chars().rev().take(600).collect::<Vec<_>>().into_iter().rev().collect()
}

/// Returns (time until the control name appears, time until it answers on its main loop).
fn wait_ready(conn: &gio::DBusConnection, child: &mut Child, errlog: &mut std::fs::File, t0: Instant) -> (Duration, Duration) {
    let t_name = loop {
        if let Ok(Some(status)) = child.try_wait() {
            panic!("app exited during startup ({status})\n--- app stderr ---\n{}", stderr_tail(errlog));
        }
        if name_has_owner(conn, CONTROL_NAME) {
            break t0.elapsed();
        }
        assert!(t0.elapsed() < BUDGET, "control service never appeared within {BUDGET:?}\n{}", stderr_tail(errlog));
        std::thread::sleep(Duration::from_millis(10));
    };
    let remaining = BUDGET.saturating_sub(t0.elapsed()).max(Duration::from_millis(100));
    let call = |iface: &str, method: &str, args: Option<&gtk::glib::Variant>| {
        conn.call_sync(
            Some(CONTROL_NAME),
            CONTROL_PATH,
            iface,
            method,
            args,
            None,
            gio::DBusCallFlags::NONE,
            remaining.as_millis() as i32,
            gio::Cancellable::NONE,
        )
    };
    call("org.freedesktop.DBus.Peer", "Ping", None)
        .unwrap_or_else(|e| panic!("claimed its bus name but never answered a ping: {e}\n{}", stderr_tail(errlog)));
    // Peer.Ping is answered by GDBus' own thread. A bogus OpenWindow is rejected by a handler that
    // runs on the GTK main loop, so any reply (even this error) proves the loop is running.
    match call(CONTROL_NAME, "OpenWindow", Some(&("nonsense",).to_variant())) {
        Err(e) if e.matches(gio::IOErrorEnum::TimedOut) => {
            panic!("main loop never answered (stuck in startup?): {e}\n{}", stderr_tail(errlog))
        }
        _ => {}
    }
    (t_name, t0.elapsed())
}

fn stop(child: &mut Child) {
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.kill();
    }
    let _ = child.wait();
}

fn start_once() -> Option<(Duration, Duration)> {
    let bus = PrivateBus::start()?;
    let conn = bus.connect();
    let env = ChildEnv::new();
    let mut errlog = open_log(&env);
    let t0 = Instant::now();
    let mut child = env.launch(&bus.address, errlog.try_clone().expect("dup stderr log"));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| wait_ready(&conn, &mut child, &mut errlog, t0)));
    stop(&mut child);
    match result {
        Ok(r) => Some(r),
        Err(e) => std::panic::resume_unwind(e),
    }
}

fn open_log(env: &ChildEnv) -> std::fs::File {
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(env.home.0.join("stderr.log"))
        .expect("create stderr log")
}

fn skip_without_dbus_daemon() -> bool {
    let missing = Command::new("dbus-daemon").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_err();
    if missing {
        crate::common::skip("dbus-daemon not found; these tests need a private session bus");
    }
    missing
}

// ports: TestAppStartup.test_starts_and_answers_within_budget
fn app_startup__starts_and_answers_within_budget() {
    if skip_without_dbus_daemon() {
        return;
    }
    let runs: Vec<_> = (0..RUNS).map(|_| start_once().expect("private bus")).collect();
    let fmt = |xs: Vec<f64>| {
        let mut s = xs.clone();
        s.sort_by(f64::total_cmp);
        format!("min {:.2}s / median {:.2}s / max {:.2}s", s[0], s[s.len() / 2], s[s.len() - 1])
    };
    println!(
        "\n[startup] bus name up : {}\n[startup] fully ready : {}  (budget {BUDGET:?}, {RUNS} runs)",
        fmt(runs.iter().map(|r| r.0.as_secs_f64()).collect()),
        fmt(runs.iter().map(|r| r.1.as_secs_f64()).collect())
    );
    for (_, ready) in &runs {
        assert!(*ready < BUDGET, "ready after {ready:?}");
    }
}

// ports: TestAppStartup.test_launching_again_while_running_does_not_hang
fn app_startup__launching_again_while_running_does_not_hang() {
    // The launcher entry runs a bare `rog-control`, so clicking it while the service is up is a
    // second plain launch. It must hand off to the running instance and exit, not linger as an
    // invisible GApplication "remote" instance (found in the wild: eight of them, one per click).
    if skip_without_dbus_daemon() {
        return;
    }
    let bus = PrivateBus::start().expect("private bus");
    let conn = bus.connect();
    let env = ChildEnv::new();
    let mut errlog = open_log(&env);
    let mut first = env.launch(&bus.address, errlog.try_clone().expect("dup log"));
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        wait_ready(&conn, &mut first, &mut errlog, Instant::now());
        let mut second = env.launch(&bus.address, errlog.try_clone().expect("dup log"));
        let deadline = Instant::now() + BUDGET;
        let status = loop {
            if let Some(status) = second.try_wait().expect("poll second launch") {
                break status;
            }
            if Instant::now() >= deadline {
                stop(&mut second);
                panic!("second plain launch still running after {BUDGET:?}: it hangs instead of handing off");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(status.success(), "second launch exited with {status}");
        assert!(first.try_wait().expect("poll first").is_none(), "the running instance must survive a second launch");
    }));
    stop(&mut first);
    if let Err(e) = outcome {
        std::panic::resume_unwind(e);
    }
}
