# Rust rewrite of ROG Control, with a shell installer

**Status:** ready
**Risk:** high

## Summary
ROG Control (the GTK4/libadwaita tray app for the ASUS ROG Strix G733ZW) is rewritten from Python into a single Rust binary, `rog-control`, that looks and behaves exactly like today's app, and a `install.sh` puts it on a machine (binary, systemd user service, launcher entry, icons) in one command. Your Hyprland keybinds are repointed at the installed binary. Nothing in the UI, hardware behaviour or saved settings changes for the user.

**Reference implementation:** `main.py` + `config.py` + `tests/test_e2e.py` at git commit `509d5e4`. Where this document is silent, that code is the spec; any observable difference from it is a defect unless listed under Decisions.

**Why high risk:** the app writes to firmware and hardware (GPU MUX / dGPU power, fan curves, power limits, battery limit, a system reboot). A slip in the port can overheat the laptop, leave the display without a GPU until a reboot, or reboot without asking. The installer replaces a running service, and the keybind edit touches the file that decides whether the ROG key still works.

## User stories
- As the laptop's owner, I want the app to look and behave exactly as it does now, so that the rewrite costs me nothing to adopt.
- As the owner, I want to run one shell command to build and install it (and one to remove it), so that deploying, updating, or moving it to another Arch/CachyOS machine is not a manual job.
- As the owner, I want my ROG key and Ctrl+Shift+F5/F12 hotkeys to keep working after the switch, so that I never have to edit Hyprland config by hand.
- As the owner, I want the safety rules the Python app has learned (no MUX change without confirmation, no dGPU-off with a display on it, monotonic fan curves) to survive the port, so that the rewrite cannot hurt the hardware.
- As the owner, I want to be able to go back to the Python version if something is wrong, so that a bad rewrite is a five-minute detour.

## Functional requirements

### Binary, CLI and lifecycle
- FR-1: `cargo build --release --locked` at the repository root produces one executable, `target/release/rog-control`, with `Cargo.lock` committed. The binary links no Python (`ldd` shows no libpython) and runs with no `python3` on `PATH`.
- FR-2: Launched with no flags and no running instance, it starts the service: it registers the tray item and the control service and stays resident with **no window shown**. It is up (answers a D-Bus `Peer.Ping` on `com.orneyfish.RogControl.Hotkeys`) within 5 seconds of launch on this machine, measured over 3 runs.
- FR-3: The flags `--open`, `--cycle-profile`, `--cycle-profile-back` call the running instance over the session bus and exit 0 without starting a GTK application: service name `com.orneyfish.RogControl.Hotkeys`, object `/com/orneyfish/RogControl/Hotkeys`, interface `com.orneyfish.RogControl.Hotkeys`, methods `Open`, `CycleProfile`, `CycleProfileBack` (no arguments). `--open` toggles the main window (shows it if hidden, hides it if visible). `--cycle-profile` / `--cycle-profile-back` move to the next / previous performance profile in the order Quiet, Balanced, Performance (wrapping), starting from the profile currently in `/sys/firmware/acpi/platform_profile`.
- FR-4: If the service is not reachable, a flag invocation prints `ROG Control service not reachable: <reason>` to stderr and exits 0 within 5 seconds (same as today).
- FR-5: A launch with no flags while an instance already owns `com.orneyfish.RogControl` behaves as `--open` and exits 0 within 5 seconds; it never lingers as a second process, and the running instance survives. Eight such launches in a row leave exactly one `rog-control` process.
- FR-6: The D-Bus control interface gains one additive method, `OpenWindow(s name)`, where `name` is one of `main`, `fans`, `lighting`, `extra`; it presents that window (like its in-app button) and is the only way the evals open the companion windows without simulated clicks. No CLI flag exposes it.
- FR-7: Unknown `name` or an `OpenWindow` call that cannot build the window returns a D-Bus error and the service keeps running.

### Tray
- FR-8: The service registers a StatusNotifierItem at object path `/StatusNotifierItem` with `org.kde.StatusNotifierWatcher` at `/StatusNotifierWatcher` (`RegisterStatusNotifierItem(<unique-name>/StatusNotifierItem)`), retrying every 3 seconds up to 10 more times if the watcher is not there yet. Properties: Category `Hardware`, Id `rog-control`, Title `ROG Control`, Status `Active`, IconName `rog-control-symbolic`, IconThemePath = the directory holding the installed `hicolor` icon tree, ItemIsMenu false, ToolTip text as in `main.py`.
- FR-9: `Activate` toggles the main window; `SecondaryActivate` (right-click) hides the window and quits the app; `Scroll` cycles to the next performance profile.

### Windows and look
- FR-10: GTK application id and window class are `com.orneyfish.RogControl`. Four windows exist with these exact titles: the main window `ROG Control` (not resizable, auto-sized to content, no scrollbar), and the on-demand `Fans and Power`, `Keyboard Lighting` and `Extra` (the existing Hyprland window rules in `~/.config/hypr/custom/rules.lua` match on class and title and must keep placing them exactly as now).
- FR-11: The main window stacks Performance, GPU, Screen, Keyboard, Battery, Startup panels, each followed by a separator, then the Footer, with the same labels, icons, order, live status labels and spacing as `main.py`. The header bar shows the title and the device name (DMI product name before the first underscore, or `This laptop`).
- FR-12: Appearance: the Material-You CSS built from `~/.local/state/caelestia/scheme.json` (`build_material_css`) is ported with every colour, radius, glow and selector, and the bundled icons are registered. A CSS rule GTK already rejects as invalid (the `Unknown name of pseudo-class` warning at `gtk.css:312`) may be dropped, because it has no visual effect today; the Rust app prints no GTK CSS parser warnings.
- FR-13: The window close button hides the main window (it does not quit) and stops its polling; `Fans and Power`, `Keyboard Lighting` and `Extra` are destroyed on close and rebuilt fresh on next open. Only one instance of each companion window exists at a time.
- FR-14: While the main window is visible, Performance, GPU and Battery refresh every 2 seconds; while it is hidden, no polling and no hardware-tool call happens.
- FR-15: Window geometry matches the reference: main 494x913, Fans and Power 560x760, Keyboard Lighting 380x480, Extra 453x320 (each within 4px, on this machine and theme).

### Hardware behaviour (all identical to `main.py`)
- FR-16: Performance panel: Silent/Balanced/Turbo map to `Quiet`/`Balanced`/`Performance`; selecting one runs `asusctl profile set <Name>` and then, if `powerprofilesctl` exists, `powerprofilesctl set <plan>` with the same plan pairing as `PROFILE_POWER_PLAN`; "Fans + Power" opens the companion window; CPU temperature and fan RPM come from hwmon sensors resolved by driver name (`coretemp`, `asus`), never by fixed `hwmonN` numbers.
- FR-17: GPU panel keeps the 4-state model (Eco, Standard, Ultimate, Optimized) built from `gpu_mux_mode` and `dgpu_disable`: it writes the MUX attribute only when it is actually changing, never writes `dgpu_disable` in the same call as a MUX change, asks for confirmation before any MUX change (and only then offers `systemctl reboot`), refuses to disable the dGPU while an external display is connected through the NVIDIA card, and in Optimized refuses to disable the dGPU while it is in heavy use (`nvidia-smi --query-gpu=utilization.gpu`). A refusal reports an error toast and changes nothing.
- FR-18: Screen panel (Auto / 60Hz / 240Hz + OD) applies presets with `hyprctl eval <lua>` (never `hyprctl keyword`), on the internal panel (the `eDP*` monitor, whichever order `hyprctl monitors -j` lists it), preserving that monitor's position and output name; `asusctl armoury set panel_overdrive` for the overdrive part.
- FR-19: Keyboard panel and Lighting window: backlight level via `asusctl leds set <level>`; effects via `asusctl aura effect <effect> ...`; the current effect, colours, speed and direction are read from asusd (`xyz.ljones.Asusd`, `xyz.ljones.Aura`, property `LedModeData`, system bus), not assumed; the effect-to-index mapping tables are ported verbatim and remain exact inverses; "Reset colours to desktop theme" uses the theme colour and switches to `static` when the effect carries no colour.
- FR-20: Battery panel: slider range equals the range `asusctl` supports (as `main.py`), `asusctl battery limit <n>`, the "Full" one-shot button runs `asusctl battery oneshot`, live capacity/status/power figures read from `/sys/class/power_supply/BAT0/*`.
- FR-21: Fans and Power window: CPU/GPU fan selection, Power / Fan Curve views, power-mode dropdown (`powerprofilesctl get` / `set`), PL1, PL2, dynamic boost, temp target (`asusctl armoury set ppt_pl1_spl|ppt_pl2_sppt|nv_dynamic_boost|nv_temp_target <n>`), NVIDIA core/memory clock offsets (`nvidia-settings -a ...`) and power limit (`nvidia-smi -pl <watts>`). Building this window runs the same availability probes as Python (`nvidia-settings -q [gpu:0]/GPUGraphicsClockOffset[3]`, `nvidia-smi -q -d POWER`, and `nvidia-smi -pl <current limit>` to test whether the limit is settable). The fan-curve editor parses `asusctl fan-curve --mod-profile <Profile>` output per fan, writes `--fan <cpu|gpu> --data <points>`, enables with `--enable-fan-curve`, factory-defaults with `--default`, only ever produces curves whose fan speed is non-decreasing with temperature, and refuses Factory Defaults when the edited profile is not the active one.
- FR-22: `nvidia-smi -pl` success is decided from its stdout text, never its exit code (`Changing power management limit is not supported ...` is a failure even at exit 0).
- FR-23: Controls whose firmware attribute reads "unavailable" are still shown and interactive; a failed write shows a toast with the tool's error text, never a crash and never a vanished control.
- FR-24: Extra window and Footer: Boot (POST) sound (`asusctl armoury set boot_sound 0|1`), charging mode (`asusctl armoury set charge_mode <0|1|2>`), the Firmware and Quit buttons behave as today. Startup panel: its switch reflects `systemctl --user is-enabled rog-control.service` and toggles it with `systemctl --user enable|disable rog-control.service`.
- FR-25: External commands are invoked by bare name through `PATH` (`asusctl`, `nvidia-smi`, `nvidia-settings`, `powerprofilesctl`, `hyprctl`, `systemctl`, `notify-send`) with exactly the argument vectors the Python app uses, so the evals' fake tools can intercept every one; no command is invoked via a hard-coded absolute path. Every hardware write is a command or D-Bus call, never a direct sysfs write (as today).
- FR-26: No hardware-tool call blocks the GTK main thread: while every tool hangs for 8 seconds, the service still answers D-Bus within 1 second and the main window appears within 2 seconds of `--open`.
- FR-27: Sliders and RGB controls are debounced with the same 150 ms quiet period as today: a rapid sequence of changes sends one command carrying the final value(s); a multi-field RGB edit sends one `asusctl aura effect` with the final effect, colours, speed and direction.

### Automation and settings
- FR-28: Automation is ported: UPower `OnBattery` is watched on the system bus; on every AC/battery transition and once silently at startup, the app applies the remembered performance profile and keyboard level for that power source, GPU "Optimized" (with the heavy-use guard) and Screen "Auto" when opted in, with desktop notifications via `notify-send -a "ROG Control" -t 2000` on transitions only.
- FR-29: Config lives at `$HOME/.config/rog-control/config.json` with the identical schema (`automation.{gpu_optimized, screen_auto, profile_battery, profile_ac, kbd_battery, kbd_ac}`, `power_limits.{pl1, pl2, apply_limits, boost, temp_target, core_clock, mem_clock, gpu_power}`): a file written by the Python app loads unchanged; missing keys take defaults; unknown keys are preserved on save; a missing or corrupt file yields defaults without crashing and without being rewritten at startup; saves are atomic (write `config.json.tmp`, then rename).
- FR-30: Power-limit values and the Apply Power Limits switch persist across closing and reopening the Fans and Power window, as today.

### Deployment
- FR-31: `install.sh` (run from a checkout, as the user, no sudo) does, in this order: check prerequisites (`cargo`, `pkg-config` with `gtk4` and `libadwaita-1`) and, if any is missing, exit non-zero before touching anything, naming what is missing and printing the Arch command (`sudo pacman -S rust gtk4 libadwaita`); build with `cargo build --release --locked`; copy the binary to `$PREFIX/bin/rog-control` (`PREFIX` defaults to `$HOME/.local`); install the icon tree under `${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor`; write `${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/rog-control.service` and `${XDG_DATA_HOME:-$HOME/.local/share}/applications/com.orneyfish.RogControl.desktop`; then, unless `--no-enable` is given, run `systemctl --user daemon-reload`, `systemctl --user enable rog-control.service`, `systemctl --user restart rog-control.service`, in that order.
- FR-32: The generated unit equals the current unit (`harness/orig-rog-control.service`) except `ExecStart`, which is the installed binary's absolute path; the generated desktop entry equals the current one (`harness/orig-com.orneyfish.RogControl.desktop`) except `Exec`, likewise. Neither refers to `python3`, `main.py`, or the checkout.
- FR-33: The installed app works with the checkout deleted or moved: the binary finds its icons from the installed location, not from the repo.
- FR-34: `install.sh` is idempotent (second run leaves identical files and one running service) and atomic with respect to failure: if the build fails, nothing is installed, no installed file changes, and `systemctl` is never asked to stop, restart or disable anything. An existing unit or desktop entry that it replaces is first copied, once, to `<name>.pre-rust` (an existing `.pre-rust` is never overwritten).
- FR-35: `install.sh --uninstall` disables and stops the service (`systemctl --user disable --now rog-control.service`, then `daemon-reload`), removes the binary, unit, desktop entry and icons it installed, leaves `$HOME/.config/rog-control/config.json` and every unrelated file untouched, and exits 0 when run again with nothing installed.
- FR-36: The four Hyprland binds in `~/.config/hypr/custom/keybinds.lua` (`XF86Launch1` and `CTRL + SHIFT + F12` to `--open`, `CTRL + SHIFT + F5` to `--cycle-profile`, `CTRL + SHIFT + ALT + F5` to `--cycle-profile-back`) execute `/home/orneyfish/.local/bin/rog-control` with the same flag instead of `/usr/bin/python3 /home/orneyfish/Projects/rog-control/main.py`. Nothing else in the file changes. The original is first copied to `keybinds.lua.pre-rust` beside it. The file still parses, and `hyprctl configerrors` reports nothing after a reload.
- FR-37: The final step of the build runs `install.sh` for real on this machine so the keybinds point at an installed, running binary, and leaves `rog-control.service` active and running the Rust binary with no Python process for it.

### Tests and records
- FR-38: `cargo test` runs a Rust port of every scenario in `tests/test_e2e.py` at `509d5e4` (67 tests): each Rust test carries a `// ports: <Class>.<test_name>` comment naming the Python test it ports, so the set of names equals the Python set exactly. Two are adapted rather than transliterated: `TestNoDynamicDockingLeftover.test_open_dock_left_function_removed` becomes "the source contains no `movewindowpixel`", and the `TestHyprlandRulesSyntax` tests keep reading `~/.config/hypr/custom/rules.lua`. Tests that would change real hardware are mocked as before. `results.md` carries the Python-to-Rust mapping table.
- FR-39: `CLAUDE.md` is updated so its commands (run, restart, test, compile check) and architecture notes describe the Rust app, and it documents the rollback recipe: `./install.sh --uninstall`, restore `keybinds.lua.pre-rust`, restore the `.pre-rust` unit and desktop entry, check out `509d5e4` for `main.py`.
- FR-40: After parity is proven, `main.py`, `config.py`, `tests/test_e2e.py` and `tests/__init__.py` are removed from the tree (they remain in git history at `509d5e4`), `target/` is git-ignored, and `git ls-files '*.py'` lists nothing outside `specs/` (the spec harness there is test tooling, not shipped code).

## Non-functional requirements
- NFR-1: Safety first: no code path may change real hardware state during any automated test or eval; hardware writes in tests go through a mocked command runner or the fake tools.
- NFR-2: Responsiveness and idle cost: service ready within 5 s; idle (window hidden) CPU under 2 % averaged over 10 s; no unhandled panic on any path (stderr never contains `panicked at`).
- NFR-3: Robustness: with none of the hardware tools installed (empty `PATH`), the service still starts, `--open` still shows the window, and nothing panics.
- NFR-4: Dependencies stay light: GTK4 + libadwaita bindings, serde_json for config, and D-Bus through GIO/zbus; no async runtime and no second GUI toolkit; no crate added for a few lines.
- NFR-5: Logging: errors go to stderr (the systemd journal); nothing is printed in normal operation.
- NFR-6: Nothing the user can see or read is lost: every label, button text and icon visible in the reference captures (`mockup.html`) appears in the Rust windows.

## Touches
- New: `Cargo.toml`, `Cargo.lock`, `src/**`, `install.sh`, Rust tests (`tests/` or in-crate), `.gitignore` (`target/`).
- Changed: `CLAUDE.md`; `/home/orneyfish/.config/hypr/custom/keybinds.lua` (four exec paths only); `~/.config/systemd/user/rog-control.service` and `~/.local/share/applications/com.orneyfish.RogControl.desktop` (rewritten by the real install, originals kept as `.pre-rust`); the real service switches from Python to Rust.
- Removed (last, after parity): `main.py`, `config.py`, `tests/test_e2e.py`, `tests/__init__.py`.

## Must not touch
- `~/.config/hypr/custom/rules.lua` (window rules) and every line of `keybinds.lua` other than the four exec paths.
- `$HOME/.config/rog-control/config.json` contents (the user's real saved settings) during building and evals; evals use a copy under a temp `HOME`.
- The app id, window titles, systemd unit name `rog-control.service`, icon names, the D-Bus names and paths above, and the config schema.
- Real hardware state during builds and evals (profile, GPU mode, fan curves, power limits, keyboard, battery limit, refresh rate, boot state). No test or eval may run a real `systemctl reboot`.
- `specs/rust-rewrite/{RD.md,evals.md,mockup.html,ref/,harness/}` while building.

## Decisions
Made by the spec author while writing; the user can overrule any of them.
- D1 (user): same look and behaviour, rewritten in Rust; the installer is a shell script. D2 (user): update the Hyprland keybinds as part of this work.
- D3: Rust GTK bindings (`gtk4`, `libadwaita` from gtk4-rs) on the installed GTK 4.24 / libadwaita 1.10; the UI is built in code, as today, not from `.ui` files.
- D4: Hardware control stays "shell out to `asusctl`, `nvidia-smi`, `nvidia-settings`, `powerprofilesctl`, `hyprctl`, `systemctl`, `notify-send` and read sysfs / asusd D-Bus", exactly as Python does. No Rust asusctl crates, no direct sysfs writes.
- D5: Config and scheme paths are derived from `$HOME`, like Python's `~` expansion, not from `XDG_CONFIG_HOME`, so existing settings are found and `HOME=<tmp>` isolates a test run.
- D6: Single D-Bus mechanism choice (GIO's GDBus vs `zbus`) is left to the builder; the wire contract in FR-3, FR-6, FR-8 is fixed.
- D7: `OpenWindow` (FR-6) is a deliberate test seam, since synthetic clicks proved unreliable here; it is not a user feature and is not documented as a flag.
- D8: Evals open the real GTK windows on the live Hyprland session but with fake hardware tools on `PATH`; evals that need the real service name stop the real `rog-control.service` first and restart it at the end.
- D9: `install.sh` builds from source on the target (needs cargo and GTK dev packages); there is no prebuilt-binary download, no PKGBUILD and no sudo. A `PREFIX` and the XDG variables relocate everything so it can be tested without touching the real home.
- D10: Keybinds use the absolute installed path (`/home/orneyfish/.local/bin/rog-control`) because Hyprland's exec environment may not have `~/.local/bin` on `PATH`. The keybind edit is a one-time manual step, not part of `install.sh`, so installing elsewhere never edits someone else's Hyprland config.
- D11: Python is deleted only after parity is proven (FR-40), on the same branch, so a rollback is one `git checkout` away; the pre-rust backups (FR-34, FR-36) make rollback possible without git for the system files.
- D12: Branch is `feat/rust-rewrite`; checkpoint commits there are fine; nothing is pushed.
- D13: Quiet-period, window sizes and every user-visible string are taken from `main.py` rather than re-specified here.
- D14: The reference screenshots are from this machine, this theme and this hardware state. Evals compare a fresh capture with `imgsame` in `harness/lib.sh` (at most 1.5 % of pixels differing by more than 3 %, and a shrunk-image RMSE of at most 0.02; the Python app re-shot against itself scores 0.0013 and 0.0101) plus 4 px of size slack. I chose these thresholds by measuring: a blank dark window scores about 5 % on a naive pixel count, so a looser pixel-count rule would have passed an empty window.

## Out of scope
- New features, new hardware support, a different look, light/dark switching beyond what the Python app does.
- Other distributions or architectures, a PKGBUILD/AUR package, prebuilt release binaries, auto-update, a flatpak.
- Replacing `asusctl` with direct sysfs/D-Bus control, or porting G-Helper features the Python app does not have.
- Editing the user's Hyprland window rules or the caelestia-shell config.
- Changing the saved config schema.

UI: yes
