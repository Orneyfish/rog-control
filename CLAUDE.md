# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A GTK4/libadwaita tray application for controlling ASUS ROG laptop hardware
on Linux (performance profiles, GPU MUX/dGPU switching, fan curves, RGB
keyboard lighting, battery limits, screen refresh rate), modeled on
[G-Helper](https://github.com/seerge/g-helper)'s real UI structure and
feature set. It is a single Rust binary, `rog-control` (gtk4-rs, libadwaita-rs,
GIO's GDBus, serde_json). It has no logic of its own beyond the UI — all actual
hardware control is delegated to `asusctl` (CLI + D-Bus, package
`asusctl`/`asusd`), `nvidia-smi`/`nvidia-settings`, `power-profiles-daemon`,
`hyprctl`, `systemctl`, `notify-send`, and direct sysfs reads, invoked as
subprocesses or D-Bus calls.

This is hardware- and rice-specific: it targets one exact machine (a ROG
Strix G733ZW, Intel + NVIDIA mobile GPU) running Hyprland with the
caelestia-shell rice, and several design decisions are deliberately coupled
to that environment (see Architecture below). Porting to different hardware
means re-verifying which `asus-armoury` firmware attributes are actually
available (`asusctl armoury list`), the asusd Aura object path
(`AURA_OBJECT_PATH`) and re-resolving the hwmon sensor names.

## Commands

Build, and install for the current user (binary in `~/.local/bin`, systemd user
service, launcher entry, icons; needs `cargo`, `gtk4` and `libadwaita` with their
pkg-config files, e.g. `sudo pacman -S rust gtk4 libadwaita`):
```
cargo build --release --locked      # -> target/release/rog-control
./install.sh                        # build + install + enable + restart rog-control.service
./install.sh --no-enable            # install files only, never touch systemctl
./install.sh --uninstall            # stop/disable the service, remove what it installed
```
`PREFIX`, `XDG_DATA_HOME` and `XDG_CONFIG_HOME` relocate everything (used to test
the installer without touching the real home). An existing unit / desktop entry it
replaces is first kept once as `<name>.pre-rust`.

Run the app directly (foreground, for debugging — stderr shows panics and errors
instead of the systemd journal):
```
target/release/rog-control
```

The real deployment is a systemd user service; restart it to pick up a new binary
(`./install.sh` does this for you):
```
systemctl --user restart rog-control.service
journalctl --user -u rog-control.service -n 40 --no-pager   # check for startup errors
```

Toggle the panel or fire a hotkey action against the already-running instance
(these are what Hyprland binds call — see `~/.config/hypr/custom/keybinds.lua`,
which uses the absolute path `/home/orneyfish/.local/bin/rog-control`):
```
rog-control --open                 # toggle the main panel
rog-control --cycle-profile        # next performance profile
rog-control --cycle-profile-back   # previous performance profile
```
These call `com.orneyfish.RogControl.Hotkeys` over the session bus and exit 0
without starting GTK (if no service is reachable they print `ROG Control service
not reachable: ...` to stderr and still exit 0). No flags at all is what systemd's
`ExecStart` uses at boot — it starts the resident service with no window. If an
instance is already running, a bare launch (the app-launcher entry runs exactly
this) acts as `--open` and exits; letting it start a second GApplication instead
hung forever as an invisible "remote" instance (`app_startup__launching_again_...`).

Run the test suite (a custom runner in `tests/e2e.rs`, no display-less mode: it builds
real GTK widgets, so run it inside the graphical session):
```
cargo test --locked                       # everything
cargo test --locked --test e2e gpu_mode   # tests whose name contains "gpu_mode"
cargo clippy --all-targets --locked -- -D warnings
```
Syntax/type check without producing a binary:
```
cargo check --all-targets --locked
```

## Architecture

**One crate: `src/lib.rs` (modules) + `src/main.rs` (CLI / service switch).**
`hw.rs` holds the hardware model and the shared apply functions, `sys.rs` the
process / sysfs / main-thread plumbing, `config.rs` persistence, `theme.rs` the
Material-You CSS, `panels.rs` the main window's panels, `fans.rs` / `lighting.rs`
/ `extra.rs` the three companion windows, `popup.rs` the main window,
`control.rs` + `tray.rs` the D-Bus surface, `automation.rs` the UPower watcher,
`app.rs` the resident service. There is one `RogPopup` main window (a
vertically-stacked dashboard — Performance, GPU, Screen, Keyboard, Battery,
Startup, Footer panels — auto-sized, no tabs/sidebar/scrollbar) plus three
on-demand companion `adw::Window`s (Fans + Power, Keyboard Lighting, Extra),
matching G-Helper's real window architecture: G-Helper's "tabs" are actually
separate top-level forms, not a `TabControl`. Companion windows are dropped on
close and rebuilt fresh on next open rather than hidden-and-reused; the main
window's close button hides it (and stops its polling) instead of quitting.

**Everything that shells out or makes a D-Bus call must be async.** Widget
`refresh()`/constructors may only do fast synchronous work (sysfs reads via
`read_sysfs`/`read_sysfs_int`, a capped D-Bus property read). Anything that
spawns a subprocess (`asusctl`, `nvidia-smi`, `nvidia-settings`,
`powerprofilesctl`, `hyprctl`, `systemctl`) goes through `run_cmd()` (fire on a
worker thread, deliver `(ok, err)` back on the GTK thread) or `spawn_bg()`
(run a closure off-thread, finish on the GTK thread — used by the monitor,
temperature, curve and availability probes). `run_capture()` is the single place
a process is actually started and is only ever called from worker threads. A
synchronous subprocess call on the main thread freezes window construction and
the D-Bus service (the evals hang every tool for 8 s to prove nothing does).

**Tools are invoked by bare name through `PATH`, with exactly these argument
vectors, and never by absolute path** — that is what lets the evals' fake tools
and the test guard intercept every call. Every hardware write is a command or D-Bus
call, never a direct sysfs write.

**Every interactive control that can fire its signal faster than the backend can
process it must be debounced through the shared `Debouncer`** (150 ms quiet
period), not applied directly in the signal handler. `gtk::Scale` fires
`value-changed` continuously through a drag, and each RGB control independently
triggers a full `asusctl aura effect ...` re-send. Without debouncing, either
pattern floods the backend with concurrent calls that race — the root cause of
two confirmed bugs (an RGB race where the last command to *finish*, not the last
one picked, won; and a slider-drag flood that made `asusctl armoury set` calls
hit the client-side timeout and surface as a spurious error toast).

**Hardware-read helpers return `None`/defaults rather than failing, and UI
generally shows controls regardless, only surfacing failure on write.**
`read_sysfs`/`read_sysfs_int` never fail (a `None` path — from a resolver that
couldn't find a sensor — falls through the same default path as ENOENT).
Controls backed by attributes this exact laptop's firmware reports as
unavailable (PL1/PL2, GPU dynamic boost/thermal target, NVIDIA clock offsets,
NVIDIA power limit) are still shown and interactive; a failed write reports a
toast error (with the tool's own text) instead of the row disappearing.
Availability probing (`nvidia_clock_offset_available`,
`nvidia_power_limit_settable`, etc.) exists to seed sane starting values and an
informational note, not to hide controls. Note `nvidia-smi -pl` returns exit code
0 even when it explicitly refuses the change — success must be read from stdout
text (`nvidia_smi_pl`), never the return code alone.

**Config (`config.rs`) only stores automation preferences** — remembered
performance-mode and keyboard-backlight level per power source, plus the
GPU-Optimized and Screen-Auto opt-in flags — and the last-set power limits
(which have no hardware read-back). It lives at `$HOME/.config/rog-control/config.json`
(derived from `$HOME`, so `HOME=<tmp>` isolates a run), is held as a plain JSON
value so unknown keys survive a save, is saved atomically (`config.json.tmp`, then
rename), and a missing or corrupt file gives defaults without being rewritten.
Everything else (current profile, battery limit, fan curves, RGB state) is read
live from asusctl/sysfs/asusd's D-Bus properties, never cached in config. This
mirrors G-Helper's own automation model, which also has no single global
"automation enabled" switch: performance-mode and keyboard-backlight level are
implicit and always-on (last value per power source, no opt-in UI); GPU mode and
screen refresh instead get automation via an explicit "Optimized"/"Auto" choice
*among their own normal options*.

**GPU mode is a 4-state model built from two independent hardware attributes**,
not stored directly: `gpu_mux_mode` (0=dGPU drives the display/"Ultimate",
1=hybrid — changing this needs a reboot, gated behind a confirmation dialog) and
`dgpu_disable` (soft on/off, no reboot). Eco/Standard/Optimized all run with
`gpu_mux_mode=1` and differ only in `dgpu_disable`; `apply_gpu_mode()` only
touches the MUX attribute when it's actually changing (and then does *only* that),
so switching among Eco/Standard/Optimized while already hybrid never spuriously
prompts for a reboot, and the dGPU is never disabled in the same call as a MUX
change. `gpu_mode_needs_reboot()` is exactly the condition `apply_gpu_mode()`
writes the MUX under, so a MUX write is always preceded by the panel's
confirmation; the automation manager has no dialog and therefore never changes the
MUX itself. "Optimized" also isn't just automation-flavored Eco/Standard — see
`gpu_in_heavy_use()`, which refuses to disable the dGPU out from under an active
render, and `external_display_on_dgpu()`, which refuses to disable it with a
display wired straight to the NVIDIA card.

**RGB effect state has no read API from `asusctl`'s CLI**, only writes —
`read_aura_state()` reads the *actual current* effect/colours/speed/direction
from asusd's own D-Bus property (`xyz.ljones.Aura`'s `LedModeData`, system bus,
service `xyz.ljones.Asusd`) instead of assuming whatever this app last set. The
integer `LedMode` enum has no documented mapping (`AURA_MODE_INDEX`) and was
derived empirically — it is not sequential with `AURA_EFFECTS`' order, don't
assume it is if extending it. `AURA_OBJECT_PATH` is hardware/boot specific (USB
bus numbering) and is kept as the reference implementation had it; when it does
not match, the read-back fails soft and the panels fall back to `static` / the
theme colour.

**D-Bus surface (all GIO's GDBus, no async runtime).** Session bus:
`com.orneyfish.RogControl.Hotkeys` at `/com/orneyfish/RogControl/Hotkeys` with
`Open`, `CycleProfile`, `CycleProfileBack` and `OpenWindow(s)` (`main` / `fans` /
`lighting` / `extra`; a deliberate test seam so the evals can open the companion
windows without simulated clicks — not a user feature, no CLI flag exposes it),
and a StatusNotifierItem at `/StatusNotifierItem` registered with
`org.kde.StatusNotifierWatcher` (retried every 3 s, up to 10 more times). The
Hotkeys name is distinct from the application id: GApplication already owns
`com.orneyfish.RogControl` and requesting it again just queues. System bus: UPower's
`OnBattery` (automation) and asusd's Aura property (read-only).

**External-to-this-repo state this app depends on**: the systemd user unit at
`~/.config/systemd/user/rog-control.service` and the launcher entry (both written
by `install.sh`), the Hyprland binds in `~/.config/hypr/custom/keybinds.lua`
(a one-time manual edit, deliberately not part of `install.sh`), and Hyprland
window rules in `~/.config/hypr/custom/rules.lua` that control window
placement/sizing (scoped by *title*, not just class — this app's class is shared
across the main window and all three companion windows, so a class-only rule
affects all four at once; this was the root cause of a real overlap bug).
Companion window position is fully rule-based (a static `move` expression
anchoring them to a fixed screen region) rather than computed at runtime — an
earlier dynamic approach that queried the main window's live position via
`hyprctl` and dispatched a reposition command raced against Hyprland's own
default float placement and was unreliable; don't reintroduce that pattern.

**Two kinds of GTK noise are filtered out of the log** (`theme::is_environment_noise`):
the user's `~/.config/gtk-4.0/gtk.css` is loaded into every GTK4 app and each rule GTK
rejects there prints a "Theme parser error", and a stale accessibility-bus socket makes
GTK complain ("Unable to connect to the accessibility bus") on the first window. Neither
is this app's to fix, and the evals treat any `CRITICAL` or "Theme parser" line on stderr
as a failure. Errors in the app's own CSS are still reported. Remove
`theme::quiet_environment_noise()` to see everything again.

## Testing conventions

`cargo test --locked` runs `tests/e2e.rs`, which constructs the real widget/window
classes directly in-process and drives them through their actual methods and
GObject signals, rather than simulating clicks — this is deliberate, not a
shortcut: synthetic mouse/keyboard input (`ydotool`, `hyprctl dispatch`) proved
unreliable for verifying this app in its actual desktop environment, while this
approach has caught every real regression so far. GTK widgets must all live on one
thread, so the suite has its own tiny libtest-style runner (`harness = false`).
`pump(ms)` drains pending GLib timeouts/idle callbacks (needed to let debouncers
and async probes fire during a test).

The suite is the Rust port of the original scenario list (each test carries a
`// ports: <Class>.<test_name>` comment naming the original) plus `guards:` tests for
rules the original never tested (GPU confirmation flow, command arguments, error
toasts, cycle direction, atomic config saves, ...). **Tests never change real
hardware**: `tests/e2e/common.rs` installs a fail-closed command guard — only a
short list of read-only tool invocations ever reaches a real process, every other
command is swallowed and recorded — and tests that need a particular answer replace
it with `fake_capture` / `record_run_cmd` / `hook_sysfs`. The startup tests launch
the real binary on a private `dbus-daemon` with a throwaway `$HOME` and stand-in
tools.

## Rollback to the previous implementation

The tray app used to be a single-file Python/GTK program; it is still in git
history at commit `509d5e4`. To go back (a five-minute detour):

1. `./install.sh --uninstall` (stops and disables the service, removes the binary,
   unit, launcher entry and icons; your config is left alone).
2. Restore the Hyprland binds: `cp ~/.config/hypr/custom/keybinds.lua.pre-rust
   ~/.config/hypr/custom/keybinds.lua`, then `hyprctl reload`.
3. Restore the old unit and launcher entry:
   `cp ~/.config/systemd/user/rog-control.service.pre-rust ~/.config/systemd/user/rog-control.service`,
   `cp ~/.local/share/applications/com.orneyfish.RogControl.desktop.pre-rust ~/.local/share/applications/com.orneyfish.RogControl.desktop`,
   `systemctl --user daemon-reload`.
4. Get the program back: `git checkout 509d5e4 -- main.py config.py tests/test_e2e.py tests/__init__.py`
   (use a throwaway branch), and re-copy the icons the uninstall removed:
   `cp -r icons/hicolor ~/.local/share/icons/`.
5. `systemctl --user enable --now rog-control.service`.

## Super-agents

@review.md

- Done means proven. Report a task finished only with evidence from this session (test output, screenshot, eval result), never on your own say-so.
- One feature: `/super-agents:write-eval <name>` writes `specs/<name>/{RD.md,evals.md}`; once the user approves, `/super-agents:review-loop <name>` builds it.
- Many features: `/super-agents:write-tickets` writes `specs/tickets/NNN-slug/` and `BOARD.md`; the user marks tickets ready (`/super-agents:mark-ready 001 002`) and runs `superagent` in a terminal. Do not build board tickets in an ordinary session unless asked.
- Git: one branch per feature (`feat/<name>`) or fix (`fix/<slug>`); never commit straight to main/master. Checkpoint commits on those branches are authorized; pushing, force-pushing and `reset --hard` are not.
- A bug: `/super-agents:fix-bug` (reproduce, confirm the cause, fix at the root, add a regression test).
<!-- SUPER_AGENTS_END -->
