# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A GTK4/libadwaita tray application for controlling ASUS ROG laptop hardware
on Linux (performance profiles, GPU MUX/dGPU switching, fan curves, RGB
keyboard lighting, battery limits, screen refresh rate), modeled on
[G-Helper](https://github.com/seerge/g-helper)'s real UI structure and
feature set. It has no GUI toolkit of its own logic beyond GTK — all actual
hardware control is delegated to `asusctl` (CLI + D-Bus, package
`asusctl`/`asusd`), `nvidia-smi`/`nvidia-settings`, `power-profiles-daemon`,
and direct sysfs reads, invoked as subprocesses or D-Bus calls.

This is hardware- and rice-specific: it targets one exact machine (a ROG
Strix G733ZW, Intel + NVIDIA mobile GPU) running Hyprland with the
caelestia-shell rice, and several design decisions are deliberately coupled
to that environment (see Architecture below). Porting to different hardware
means re-verifying which `asus-armoury` firmware attributes are actually
available (`asusctl armoury list`) and re-resolving the hwmon sensor names.

## Commands

Run the app directly (foreground, for debugging — prints tracebacks to
stdout/stderr instead of the systemd journal):
```
/usr/bin/python3 main.py
```

The real deployment is a systemd user service; restart it to pick up code changes:
```
systemctl --user restart rog-control.service
journalctl --user -u rog-control.service -n 40 --no-pager   # check for startup errors
```

Toggle the panel or fire a hotkey action against the already-running instance
(these are what Hyprland binds call — see `~/.config/hypr/custom/keybinds.lua`):
```
python3 main.py --open                 # toggle the main panel
python3 main.py --cycle-profile        # next performance profile
python3 main.py --cycle-profile-back   # previous performance profile
```
No flags at all is what systemd's `ExecStart` uses at boot — deliberately a
no-op, so the process starts hidden in the tray rather than popping a window.

Run the test suite (stdlib `unittest`, no pytest dependency):
```
python3 -m unittest discover -s tests -v      # full suite
python3 -m unittest tests.test_e2e.TestGpuModeLogic -v          # one class
python3 -m unittest tests.test_e2e.TestGpuModeLogic.test_gpu_mode_needs_reboot_only_for_mux_change -v  # one test
```
Syntax/compile check without running anything:
```
python3 -m py_compile main.py config.py
```

## Architecture

**Single-file UI (`main.py`), single-file persistence (`config.py`).** There
is one `RogPopup` main window (a vertically-stacked dashboard — Performance,
GPU, Screen, Keyboard, Battery, Startup, Footer panels — auto-sized, no
tabs/sidebar/scrollbar) plus three on-demand companion `Adw.Window`s (Fans +
Power, Keyboard Lighting, Extra), matching G-Helper's real window
architecture: G-Helper's "tabs" are actually separate top-level forms, not a
`TabControl`. Companion windows are `None`'d out on close and rebuilt fresh
on next open rather than hidden-and-reused.

**Everything that shells out or makes a D-Bus call must be async.** Widget
`refresh()`/`__init__` methods may only do fast synchronous work (sysfs
reads via `read_sysfs`/`read_sysfs_int`, quick D-Bus property `Get()`
calls). Anything that spawns a subprocess (`asusctl`, `nvidia-smi`,
`nvidia-settings`, `powerprofilesctl`) goes through `run_cmd()` (fire on a
background thread, deliver the result back via `GLib.idle_add`) or a
dedicated async probe (see `FansPowerWindow._probe_nvidia_async`,
`_refresh_power_mode_async`). A synchronous subprocess call inside
`__init__` previously froze window construction for several seconds — this
is a real, previously-hit failure mode, not a hypothetical one.

**Every interactive control that can fire its signal faster than the
backend can process it must be debounced through the shared `Debouncer`
class**, not applied directly in the signal handler. `Gtk.Scale` fires
`value-changed` continuously through a drag (not once at release), and each
RGB control (colour pickers, effect/speed/direction dropdowns) independently
triggers a full `asusctl aura effect ...` re-send. Without debouncing,
either pattern floods the backend with concurrent calls that race — this
has been the root cause of two separate confirmed bugs (an RGB race where
the last command to *finish*, not the last one picked, won; and a
slider-drag flood that made `asusctl armoury set` calls queue long enough
to hit the client-side timeout and surface as a spurious error toast).

**Hardware-read helpers return `None`/defaults rather than raising, and UI
generally shows controls regardless, only surfacing failure on write.**
`read_sysfs`/`read_sysfs_int` never raise (a `None` path — from a resolver
that couldn't find a sensor — falls through the same default path as ENOENT).
Controls backed by attributes this exact laptop's firmware reports as
unavailable (PL1/PL2, GPU dynamic boost/thermal target, NVIDIA clock
offsets, NVIDIA power limit) are still shown and interactive; a failed
write reports a toast error instead of the row disappearing. Availability
probing (`nvidia_clock_offset_available`, `nvidia_power_limit_settable`,
etc.) exists to seed sane starting values and an informational note, not to
hide controls. Note `nvidia-smi -pl` returns exit code 0 even when it
explicitly refuses the change — success must be read from stdout text
(`_nvidia_smi_pl`), never the return code alone.

**Config (`config.py`) only stores automation preferences** — remembered
performance-mode and keyboard-backlight level per power source, plus the
GPU-Optimized and Screen-Auto opt-in flags. Everything else (current
profile, battery limit, fan curves, RGB state) is read live from
asusctl/sysfs/asusd's D-Bus properties, never cached in config. This
mirrors G-Helper's own automation model, which also has no single global
"automation enabled" switch: performance-mode and keyboard-backlight level
are implicit and always-on (last value per power source, no opt-in UI);
GPU mode and screen refresh instead get automation via an explicit
"Optimized"/"Auto" choice *among their own normal options*.

**GPU mode is a 4-state model built from two independent hardware
attributes**, not stored directly: `gpu_mux_mode` (0=dGPU drives the
display/"Ultimate", 1=hybrid — changing this needs a reboot, gated behind a
confirmation dialog) and `dgpu_disable` (soft on/off, no reboot). Eco/
Standard/Optimized all run with `gpu_mux_mode=1` and differ only in
`dgpu_disable`; `apply_gpu_mode()` deliberately only touches the MUX
attribute when it's actually changing, so switching among Eco/Standard/
Optimized while already hybrid never spuriously prompts for a reboot.
"Optimized" also isn't just automation-flavored Eco/Standard — see
`gpu_in_heavy_use()`, which refuses to disable the dGPU out from under an
active render.

**RGB effect state has no read API from `asusctl`'s CLI**, only writes —
`read_aura_state()` reads the *actual current* effect/colours/speed/
direction from asusd's own D-Bus property (`xyz.ljones.Aura`'s
`LedModeData`, system bus, service `xyz.ljones.Asusd`) instead of assuming
whatever this app last set. The integer `LedMode` enum has no documented
mapping (`AURA_MODE_INDEX`/`AURA_INDEX_MODE`) and was derived empirically —
it is not sequential with `AURA_EFFECTS`' iteration order, don't assume it
is if extending it.

**External-to-this-repo state this app depends on**: the systemd user unit
at `~/.config/systemd/user/rog-control.service`, and Hyprland window rules
in `~/.config/hypr/custom/rules.lua` that control window placement/sizing
(scoped by *title*, not just class — this app's class is shared across the
main window and all three companion windows, so a class-only rule affects
all four at once; this was the root cause of a real overlap bug). Companion
window position is fully rule-based (a static `move` expression anchoring
them to a fixed screen region) rather than computed at runtime — an earlier
dynamic approach that queried the main window's live position via `hyprctl`
and dispatched a reposition command raced against Hyprland's own default
float placement and was unreliable; don't reintroduce that pattern.

## Testing conventions

`tests/test_e2e.py` constructs the real widget/window classes directly
in-process and drives them through their actual methods and GObject
signals, rather than simulating clicks — this is deliberate, not a
shortcut: synthetic mouse/keyboard input (`ydotool`, `hyprctl dispatch`)
proved unreliable for verifying this app in its actual desktop environment,
while this approach has caught every real regression so far. `pump(ms)` is
the helper for draining pending `GLib` timeouts/idle callbacks (needed to
let debouncers and async probes actually fire during a test). Tests that
would change real hardware state (GPU MUX switch, an actual reboot) are
never exercised for real — either the underlying command is mocked via
`unittest.mock.patch.object(main, "run_cmd", ...)`, or only the pure
decision logic is checked against a mocked `read_sysfs_int`.
