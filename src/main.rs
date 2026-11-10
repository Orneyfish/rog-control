//! `rog-control` with no flags is the resident service (what systemd's `ExecStart` runs): tray
//! icon and control service, no window. `--open`, `--cycle-profile` and `--cycle-profile-back`
//! call the running instance over the session bus and exit (the Hyprland hotkeys use them).

use std::process::ExitCode;

use rog_control::control;

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // The launcher entry runs a bare `rog-control`, the same command as systemd. With an instance
    // already up, a second GApplication would register as a "remote" instance and hang forever
    // with no window, so a bare launch then means --open.
    if args.is_empty() && control::instance_running() {
        args.push("--open".to_string());
    }
    if !args.is_empty() {
        if let Some(method) = control::method_for_args(&args) {
            control::call_service(method);
        }
        return ExitCode::SUCCESS;
    }
    rog_control::app::run()
}
