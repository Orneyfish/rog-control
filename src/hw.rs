//! Hardware model: constants, sysfs-backed state, and the "apply" functions shared by direct UI
//! actions and the automation manager, so "what happens when you pick Balanced" is defined once.
//!
//! All control is delegated to `asusctl`, `nvidia-smi`, `nvidia-settings`, `powerprofilesctl`,
//! `hyprctl`, `systemctl` and `notify-send` (invoked by bare name through `PATH`) plus sysfs
//! reads and asusd's D-Bus properties. Nothing here writes sysfs.

use std::sync::OnceLock;
use std::time::Duration;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use serde_json::Value;

use crate::config::{self, Cfg};
use crate::sys::{
    capitalize, list_dir, py_round, read_sysfs, read_sysfs_int, run_capture, run_cmd, spawn_bg, tool_available, DoneCb,
};

pub const APP_ID: &str = "com.orneyfish.RogControl";
pub const TRAY_ICON_NAME: &str = "rog-control-symbolic";

pub const PROFILES: [&str; 3] = ["Quiet", "Balanced", "Performance"];

pub fn profile_label(name: &str) -> &str {
    match name {
        "Quiet" => "Silent",
        "Balanced" => "Balanced",
        "Performance" => "Turbo",
        other => other,
    }
}

pub fn profile_icon(name: &str) -> &'static str {
    match name {
        "Quiet" => "weather-clear-symbolic",
        "Balanced" => "weather-few-clouds-symbolic",
        _ => "weather-storm-symbolic",
    }
}

pub fn profile_hint(name: &str) -> &'static str {
    match name {
        "Quiet" => "Lower power draw, quieter fans",
        "Balanced" => "Default balance of power and noise",
        _ => "Maximum performance",
    }
}

/// G-Helper pairs each BIOS mode with a power plan; power-profiles-daemon is the Linux surface.
pub const PROFILE_POWER_PLAN: [(&str, &str); 3] =
    [("Quiet", "power-saver"), ("Balanced", "balanced"), ("Performance", "performance")];

pub fn profile_power_plan(name: &str) -> Option<&'static str> {
    PROFILE_POWER_PLAN.iter().find(|(p, _)| *p == name).map(|(_, plan)| *plan)
}

pub const PLATFORM_PROFILE_PATH: &str = "/sys/firmware/acpi/platform_profile";
pub const BATTERY_LIMIT_PATH: &str = "/sys/class/power_supply/BAT0/charge_control_end_threshold";
pub const BATTERY_CAPACITY_PATH: &str = "/sys/class/power_supply/BAT0/capacity";
pub const BATTERY_STATUS_PATH: &str = "/sys/class/power_supply/BAT0/status";
pub const BATTERY_POWER_NOW_PATH: &str = "/sys/class/power_supply/BAT0/power_now";
pub const BATTERY_ENERGY_FULL_PATH: &str = "/sys/class/power_supply/BAT0/energy_full";
pub const BATTERY_ENERGY_FULL_DESIGN_PATH: &str = "/sys/class/power_supply/BAT0/energy_full_design";
pub const KBD_BRIGHTNESS_PATH: &str = "/sys/class/leds/asus::kbd_backlight/brightness";
pub const GPU_MUX_PATH: &str = "/sys/class/firmware-attributes/asus-armoury/attributes/gpu_mux_mode/current_value";
pub const DGPU_DISABLE_PATH: &str = "/sys/class/firmware-attributes/asus-armoury/attributes/dgpu_disable/current_value";
pub const PENDING_REBOOT_PATH: &str = "/sys/class/firmware-attributes/asus-armoury/attributes/pending_reboot";
pub const PANEL_OVERDRIVE_PATH: &str =
    "/sys/class/firmware-attributes/asus-armoury/attributes/panel_overdrive/current_value";
pub const BOOT_SOUND_PATH: &str = "/sys/class/firmware-attributes/asus-armoury/attributes/boot_sound/current_value";
pub const CHARGE_MODE_PATH: &str = "/sys/class/firmware-attributes/asus-armoury/attributes/charge_mode/current_value";
pub const PPT_PL1_PATH: &str = "/sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl1_spl/current_value";
pub const PPT_PL2_PATH: &str = "/sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl2_sppt/current_value";
pub const NV_BOOST_PATH: &str = "/sys/class/firmware-attributes/asus-armoury/attributes/nv_dynamic_boost/current_value";
pub const NV_TEMP_TARGET_PATH: &str =
    "/sys/class/firmware-attributes/asus-armoury/attributes/nv_temp_target/current_value";

/// hwmon sensors, resolved by driver name: `hwmonN` numbers are not stable across boots, and a
/// hard-coded index once pointed the CPU temperature at a RAM module's sensor.
#[derive(Debug, Clone, Default)]
pub struct Hwmon {
    pub cpu_temp: Option<String>,
    pub cpu_fan: Option<String>,
    pub gpu_fan: Option<String>,
}

fn resolve_hwmon_paths() -> Hwmon {
    let base = "/sys/class/hwmon";
    let (mut coretemp, mut asus): (Option<String>, Option<String>) = (None, None);
    let mut entries = list_dir(base).unwrap_or_default();
    entries.sort();
    for entry in entries {
        let dir = format!("{base}/{entry}");
        let Some(name) = read_sysfs(Some(&format!("{dir}/name"))) else { continue };
        if name == "coretemp" && coretemp.is_none() {
            coretemp = Some(dir);
        } else if name == "asus" && asus.is_none() {
            asus = Some(dir);
        }
    }
    Hwmon {
        cpu_temp: coretemp.map(|d| format!("{d}/temp1_input")),
        cpu_fan: asus.clone().map(|d| format!("{d}/fan1_input")),
        gpu_fan: asus.map(|d| format!("{d}/fan2_input")),
    }
}

pub fn hwmon() -> &'static Hwmon {
    static HWMON: OnceLock<Hwmon> = OnceLock::new();
    HWMON.get_or_init(resolve_hwmon_paths)
}

pub const BRIGHTNESS_LEVELS: [&str; 4] = ["off", "low", "med", "high"];
pub const BRIGHTNESS_LABELS: [&str; 4] = ["Off", "Low", "Medium", "High"];

pub const GPU_MODES: [&str; 4] = ["Eco", "Standard", "Ultimate", "Optimized"];

pub fn gpu_hint(mode: &str) -> &'static str {
    match mode {
        "Eco" => "Disables dGPU to save battery",
        "Standard" => "Enables dGPU for standard use",
        "Ultimate" => "Routes laptop screen to dGPU, maximizing FPS",
        _ => "Switch to Eco on battery and to Standard when plugged in",
    }
}

pub fn gpu_icon(mode: &str) -> &'static str {
    match mode {
        "Eco" => "power-profile-power-saver-symbolic",
        "Standard" => "power-profile-balanced-symbolic",
        "Ultimate" => "input-gaming-symbolic",
        "Optimized" => "power-profile-performance-symbolic",
        _ => "video-display-symbolic",
    }
}

pub fn screen_hint(key: &str) -> &'static str {
    match key {
        "auto" => "Sets 60Hz to save battery, and back when plugged",
        "60" => "Fixed at 60Hz",
        _ => "Fixed at the display's maximum refresh rate, with overdrive",
    }
}

pub const CHARGE_MODES: [&str; 3] = ["Full Capacity", "Balanced", "Max Lifespan"];

/// Effect name -> the extra fields it takes, per `asusctl aura effect <name> --help`.
pub const AURA_EFFECTS: [(&str, &[&str]); 12] = [
    ("static", &["colour1"]),
    ("breathe", &["colour1", "colour2", "speed"]),
    ("pulse", &["colour1"]),
    ("rainbow-cycle", &["speed"]),
    ("rainbow-wave", &["direction", "speed"]),
    ("stars", &["colour1", "colour2", "speed"]),
    ("rain", &["speed"]),
    ("highlight", &["colour1", "speed"]),
    ("laser", &["colour1", "speed"]),
    ("ripple", &["colour1", "speed"]),
    ("comet", &["colour1"]),
    ("flash", &["colour1"]),
];
pub const DIRECTIONS: [&str; 4] = ["up", "down", "left", "right"];
pub const SPEEDS: [&str; 3] = ["low", "med", "high"];

pub fn aura_effect_names() -> Vec<&'static str> {
    AURA_EFFECTS.iter().map(|(n, _)| *n).collect()
}

pub fn effect_needs(effect: &str) -> &'static [&'static str] {
    AURA_EFFECTS.iter().find(|(n, _)| *n == effect).map(|(_, needs)| *needs).unwrap_or(&[])
}

/// asusctl has no "get current effect": the only read-back is asusd's `LedModeData`, whose mode
/// is a plain integer. This mapping was derived empirically (pulse is NOT sequential), so keep
/// the two directions exact inverses when extending it.
pub const AURA_MODE_INDEX: [(&str, u32); 12] = [
    ("static", 0),
    ("breathe", 1),
    ("rainbow-cycle", 2),
    ("rainbow-wave", 3),
    ("stars", 4),
    ("rain", 5),
    ("highlight", 6),
    ("laser", 7),
    ("ripple", 8),
    ("pulse", 10),
    ("comet", 11),
    ("flash", 12),
];

pub fn aura_index_mode(idx: u32) -> Option<&'static str> {
    AURA_MODE_INDEX.iter().find(|(_, i)| *i == idx).map(|(n, _)| *n)
}

/// Hardware-specific object suffix (USB vendor/product/bus/port); kept verbatim from the
/// reference implementation.
pub const AURA_OBJECT_PATH: &str = "/xyz/ljones/aura/19b6_3_3";

/// G-Helper's fixed per-concept accent colours (teal = battery friendly, blue = standard, red =
/// max performance, orange = custom action), a deliberate exception to the Material palette.
pub const ACCENT_ECO: &str = "#06B48A";
pub const ACCENT_STANDARD: &str = "#3AAEEF";
pub const ACCENT_TURBO: &str = "#FF2020";
pub const ACCENT_CUSTOM: &str = "#FF8000";

pub fn profile_accent_class(name: &str) -> &'static str {
    match name {
        "Quiet" => "rog-accent-eco",
        "Balanced" => "rog-accent-standard",
        _ => "rog-accent-turbo",
    }
}

pub fn gpu_accent_class(mode: &str) -> &'static str {
    match mode {
        "Eco" | "Optimized" => "rog-accent-eco",
        "Standard" => "rog-accent-standard",
        _ => "rog-accent-turbo",
    }
}

pub fn device_name() -> String {
    // DMI often duplicates the SKU after an underscore, e.g. "ROG Strix G733ZW_G733ZW".
    match read_sysfs(Some("/sys/class/dmi/id/product_name")) {
        Some(name) if !name.is_empty() => name.split('_').next().unwrap_or("").trim().to_string(),
        _ => "This laptop".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Notifications, profiles
// ---------------------------------------------------------------------------

/// Desktop-notification feedback for hotkey/automation-triggered actions, which fire without the
/// panel necessarily being open.
pub fn notify_osd(title: &str, body: &str) {
    if !tool_available("notify-send") {
        return;
    }
    let mut cmd = vec!["notify-send", "-a", "ROG Control", "-t", "2000", title];
    if !body.is_empty() {
        cmd.push(body);
    }
    run_cmd(&cmd, None);
}

pub fn apply_profile(name: &str, on_done: Option<DoneCb>) {
    run_cmd(&["asusctl", "profile", "set", name], on_done);
    if tool_available("powerprofilesctl") {
        if let Some(plan) = profile_power_plan(name) {
            run_cmd(&["powerprofilesctl", "set", plan], None);
        }
    }
}

/// The profile after `current` when stepping by `step`, wrapping; an unknown `current` counts as
/// the first profile.
pub fn cycle_target(current: &str, step: i32) -> &'static str {
    let idx = PROFILES.iter().position(|p| *p == current).unwrap_or(0) as i32;
    PROFILES[(idx + step).rem_euclid(PROFILES.len() as i32) as usize]
}

pub fn cycle_profile(step: i32, on_done: Option<DoneCb>) {
    let current = capitalize(&read_sysfs(Some(PLATFORM_PROFILE_PATH)).unwrap_or_else(|| "balanced".to_string()));
    let target = cycle_target(&current, step);
    let done: DoneCb = Box::new(move |ok, err| {
        if ok {
            notify_osd("Performance mode", profile_label(target));
        }
        if let Some(d) = on_done {
            d(ok, err);
        }
    });
    apply_profile(target, Some(done));
}

// ---------------------------------------------------------------------------
// GPU mode
// ---------------------------------------------------------------------------

/// Eco/Standard/Optimized all run with `gpu_mux_mode=1` and differ only in `dgpu_disable`;
/// only Ultimate needs the MUX at 0.
pub fn current_gpu_mode(cfg: &Cfg) -> &'static str {
    if read_sysfs_int(Some(GPU_MUX_PATH)).unwrap_or(0) == 0 {
        return "Ultimate";
    }
    if config::get_bool(cfg, "automation", "gpu_optimized") {
        return "Optimized";
    }
    if read_sysfs_int(Some(DGPU_DISABLE_PATH)).unwrap_or(0) == 1 {
        "Eco"
    } else {
        "Standard"
    }
}

fn target_mux(mode: &str) -> i64 {
    if mode == "Ultimate" {
        0
    } else {
        1
    }
}

/// True when applying `mode` would write `gpu_mux_mode`, which only takes effect after a reboot
/// and so must be confirmed first. An unreadable MUX counts as "would change it": when in doubt,
/// ask. This is exactly the condition [`apply_gpu_mode`] writes the MUX under.
pub fn gpu_mode_needs_reboot(mode: &str) -> bool {
    read_sysfs_int(Some(GPU_MUX_PATH)) != Some(target_mux(mode))
}

/// Safety-critical. A MUX change only takes effect after a reboot, and the confirmation dialog
/// that says so lives in the GPU panel's click handler. So when a MUX change is needed this does
/// ONLY that and stops: `dgpu_disable` is touched only once the real MUX already matches, or the
/// dGPU could be disabled while it is still the only thing driving the display.
pub fn apply_gpu_mode(mode: &str, on_battery: bool, on_done: Option<DoneCb>) {
    let target = target_mux(mode);
    if read_sysfs_int(Some(GPU_MUX_PATH)) != Some(target) {
        run_cmd(&["asusctl", "armoury", "set", "gpu_mux_mode", &target.to_string()], on_done);
        return;
    }
    if mode == "Ultimate" {
        if let Some(d) = on_done {
            d(true, String::new());
        }
        return;
    }
    let disable = mode == "Eco" || (mode == "Optimized" && on_battery);
    if disable && external_display_on_dgpu() {
        // An external monitor wired straight to the dGPU (this laptop's HDMI port) has no iGPU
        // fallback: disabling the dGPU would blank it whatever the MUX says.
        if let Some(d) = on_done {
            d(
                false,
                "An external display is connected directly to the dGPU -- disabling it would disconnect that screen"
                    .to_string(),
            );
        }
        return;
    }
    run_cmd(&["asusctl", "armoury", "set", "dgpu_disable", if disable { "1" } else { "0" }], on_done);
}

/// Utilisation above 20 % means a render is probably running. Blocks: worker threads only.
pub fn gpu_in_heavy_use_blocking() -> bool {
    run_capture(&["nvidia-smi", "--query-gpu=utilization.gpu", "--format=csv,noheader,nounits"], Duration::from_secs(5))
        .ok()
        .and_then(|out| out.stdout.trim().parse::<i64>().ok())
        .is_some_and(|util| util > 20)
}

pub fn gpu_in_heavy_use(done: impl FnOnce(bool) + 'static) {
    spawn_bg(gpu_in_heavy_use_blocking, done);
}

/// True if a non-internal display is connected directly to the NVIDIA dGPU, bypassing the MUX
/// entirely (confirmed on this hardware: HDMI shows up as a connector under the dGPU's own DRM
/// card, vendor 0x10de).
pub fn external_display_on_dgpu() -> bool {
    let base = "/sys/class/drm";
    let Some(entries) = list_dir(base) else { return false };
    for entry in entries {
        if !entry.starts_with("card") {
            continue;
        }
        let Some((card, connector)) = entry.split_once('-') else { continue };
        if read_sysfs(Some(&format!("{base}/{card}/device/vendor"))).as_deref() != Some("0x10de") {
            continue; // NVIDIA's PCI vendor id
        }
        if connector.starts_with("eDP") {
            continue; // the internal panel routed through the dGPU is expected, not "external"
        }
        if read_sysfs(Some(&format!("{base}/{entry}/status"))).as_deref() == Some("connected") {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// NVIDIA overclocking / power limit (nvidia-settings and nvidia-smi)
// ---------------------------------------------------------------------------

/// This GPU's top P-state, per `nvidia-settings -q GPUPerfModes`.
pub const NV_PERF_LEVEL: i32 = 3;

fn nvidia_settings_query_ok(attr: &str) -> bool {
    match run_capture(&["nvidia-settings", "-q", attr], Duration::from_secs(5)) {
        Ok(out) => !out.stdout.contains("ERROR") && !out.stderr.contains("ERROR"),
        Err(_) => false,
    }
}

pub fn nvidia_clock_offset_available() -> bool {
    tool_available("nvidia-settings")
        && nvidia_settings_query_ok(&format!("[gpu:0]/GPUGraphicsClockOffset[{NV_PERF_LEVEL}]"))
}

pub fn apply_core_clock_offset(mhz: i64, on_done: Option<DoneCb>) {
    run_cmd(&["nvidia-settings", "-a", &format!("[gpu:0]/GPUGraphicsClockOffset[{NV_PERF_LEVEL}]={mhz}")], on_done);
}

pub fn apply_mem_clock_offset(mhz: i64, on_done: Option<DoneCb>) {
    run_cmd(&["nvidia-settings", "-a", &format!("[gpu:0]/GPUMemoryTransferRateOffset[{NV_PERF_LEVEL}]={mhz}")], on_done);
}

/// `<label>\s*:\s*<number>\s*W` anywhere in `text` (first match), as watts.
fn find_watts(text: &str, label: &str) -> Option<f64> {
    let mut from = 0;
    while let Some(pos) = text[from..].find(label) {
        let rest = text[from + pos + label.len()..].trim_start();
        if let Some(after_colon) = rest.strip_prefix(':') {
            let rest = after_colon.trim_start();
            let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(rest.len());
            if end > 0 && rest[end..].trim_start().starts_with('W') {
                if let Ok(v) = rest[..end].parse() {
                    return Some(v);
                }
            }
        }
        from += pos + label.len();
    }
    None
}

/// `(current, min, max)` watts out of `nvidia-smi -q -d POWER` output.
pub fn parse_power_limit_bounds(out: &str) -> Option<(f64, f64, f64)> {
    Some((
        find_watts(out, "Current Power Limit")?,
        find_watts(out, "Min Power Limit")?,
        find_watts(out, "Max Power Limit")?,
    ))
}

/// Blocks: worker threads only.
pub fn nvidia_power_limit_bounds() -> Option<(f64, f64, f64)> {
    let out = run_capture(&["nvidia-smi", "-q", "-d", "POWER"], Duration::from_secs(5)).ok()?;
    parse_power_limit_bounds(&out.stdout)
}

/// `nvidia-smi -pl` exits 0 even when it explicitly refuses the change ("Changing power
/// management limit is not supported in current scope"), so success is read from its output
/// text, never the exit code alone. Blocks: worker threads only.
pub fn nvidia_smi_pl(watts: f64, timeout: Duration) -> (bool, String) {
    match run_capture(&["nvidia-smi", "-pl", &py_round(watts).to_string()], timeout) {
        Ok(out) => {
            let ok = out.ok && !out.stdout.to_lowercase().contains("not supported");
            let err = if ok {
                String::new()
            } else if !out.stdout.trim().is_empty() {
                out.stdout.trim().to_string()
            } else {
                out.stderr.trim().to_string()
            };
            (ok, err)
        }
        Err(e) => (false, e.to_string()),
    }
}

/// One-off no-op probe (sets the limit back to its current value): confirms whether
/// `nvidia-smi -pl` is honoured rather than merely accepted. Blocks: worker threads only.
pub fn nvidia_power_limit_settable(watts: f64) -> bool {
    nvidia_smi_pl(watts, Duration::from_secs(5)).0
}

pub fn apply_gpu_power_limit(watts: f64, on_done: Option<DoneCb>) {
    spawn_bg(move || nvidia_smi_pl(watts, Duration::from_secs(10)), move |(ok, err)| {
        if let Some(d) = on_done {
            d(ok, err);
        }
    });
}

// ---------------------------------------------------------------------------
// Keyboard RGB
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuraState {
    pub effect: String,
    pub colour1: String,
    pub colour2: String,
    pub speed: String,
    pub direction: String,
}

fn rgb_hex(v: &glib::Variant) -> Option<String> {
    if !v.is_container() {
        return None;
    }
    let mut s = String::new();
    for i in 0..v.n_children() {
        s.push_str(&format!("{:02x}", v.child_value(i).get::<u8>()?));
    }
    Some(s)
}

/// Parses asusd's `LedModeData` property, signature `(uu(yyy)(yyy)ss)`: mode, zone, colour1,
/// colour2, speed, direction.
pub fn parse_led_mode_data(v: &glib::Variant) -> Option<AuraState> {
    if !v.is_container() || v.n_children() < 6 {
        return None;
    }
    let mode = v.child_value(0).get::<u32>()?;
    Some(AuraState {
        effect: aura_index_mode(mode).unwrap_or("static").to_string(),
        colour1: rgb_hex(&v.child_value(2))?,
        colour2: rgb_hex(&v.child_value(3))?,
        speed: v.child_value(4).str()?.to_lowercase(),
        direction: v.child_value(5).str()?.to_lowercase(),
    })
}

/// Current keyboard RGB mode/colours/speed/direction, read straight from asusd's own D-Bus
/// property rather than assumed from whatever this app last set, so the panel reflects reality
/// even after a physical Fn-combo or an asusctl call made elsewhere. A quick property read with a
/// 1 s cap, as the architecture allows for D-Bus `Get()` calls.
pub fn read_aura_state() -> Option<AuraState> {
    let bus = gio::bus_get_sync(gio::BusType::System, gio::Cancellable::NONE).ok()?;
    let reply = bus
        .call_sync(
            Some("xyz.ljones.Asusd"),
            AURA_OBJECT_PATH,
            "org.freedesktop.DBus.Properties",
            "Get",
            Some(&("xyz.ljones.Aura", "LedModeData").to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            1000,
            gio::Cancellable::NONE,
        )
        .ok()?;
    let inner = reply.child_value(0).as_variant()?;
    parse_led_mode_data(&inner)
}

/// The `asusctl aura effect ...` command for the given field values; only the fields the effect
/// actually takes are included.
pub fn build_aura_cmd(effect: &str, colour1: &str, colour2: &str, direction: &str, speed: &str) -> Vec<String> {
    let needs = effect_needs(effect);
    let mut cmd: Vec<String> = ["asusctl", "aura", "effect", effect].iter().map(|s| s.to_string()).collect();
    if needs.contains(&"colour1") {
        let short = ["static", "pulse", "highlight", "laser", "ripple", "comet", "flash"].contains(&effect);
        cmd.push(if short { "-c" } else { "--colour" }.to_string());
        cmd.push(colour1.to_string());
    }
    if needs.contains(&"colour2") {
        cmd.push("--colour2".to_string());
        cmd.push(colour2.to_string());
    }
    if needs.contains(&"direction") {
        cmd.push("--direction".to_string());
        cmd.push(direction.to_string());
    }
    if needs.contains(&"speed") {
        cmd.push("--speed".to_string());
        cmd.push(speed.to_string());
    }
    cmd
}

pub fn run_cmd_vec(cmd: &[String], on_done: Option<DoneCb>) {
    let refs: Vec<&str> = cmd.iter().map(String::as_str).collect();
    run_cmd(&refs, on_done);
}

pub fn apply_kbd_level(level: &str, on_done: Option<DoneCb>) {
    run_cmd(&["asusctl", "leds", "set", level], on_done);
}

// ---------------------------------------------------------------------------
// Screen
// ---------------------------------------------------------------------------

/// The laptop's own panel: refresh rate and panel overdrive are internal-panel-only features, and
/// `hyprctl monitors -j` may list an external monitor first. Falls back to the first monitor only
/// when no `eDP*` output exists. Blocks: worker threads only.
pub fn current_monitor_blocking() -> Option<Value> {
    let out = run_capture(&["hyprctl", "monitors", "-j"], Duration::from_secs(5)).ok()?;
    let monitors: Value = serde_json::from_str(&out.stdout).ok()?;
    let list = monitors.as_array()?;
    let internal = list.iter().find(|m| m["name"].as_str().is_some_and(|n| n.starts_with("eDP")));
    internal.or_else(|| list.first()).cloned()
}

pub fn current_monitor(done: impl FnOnce(Option<Value>) + 'static) {
    spawn_bg(current_monitor_blocking, done);
}

/// Highest refresh rate among the monitor's modes ("2560x1440@240.00Hz"), 60 when unknown.
pub fn max_refresh_rate(monitor: Option<&Value>) -> i64 {
    let modes = monitor.and_then(|m| m["availableModes"].as_array()).cloned().unwrap_or_default();
    let rates: Option<Vec<i64>> = modes
        .iter()
        .map(|m| {
            let hz = m.as_str()?.split('@').nth(1)?.trim_end_matches(['H', 'z']);
            Some(py_round(hz.parse::<f64>().ok()?))
        })
        .collect();
    rates.and_then(|r| r.into_iter().max()).unwrap_or(60)
}

/// `preset`: "auto-battery" (60Hz, overdrive off), "auto-ac" (max Hz, overdrive on), "60", or
/// "max" (max Hz + overdrive, G-Helper's single combined "<Hz> + OD" button).
///
/// The mode change goes through `hyprctl eval "hl.monitor({...})"`, never `hyprctl keyword
/// monitor`: this Hyprland has a Lua config, where `keyword` exits 0 and does nothing. Position
/// and scale are passed through from the live read-back so the user's layout is never touched.
pub fn apply_screen_preset(preset: &str, on_done: Option<DoneCb>) {
    let low = preset == "60" || preset == "auto-battery";
    let overdrive = if low { "0" } else { "1" };
    current_monitor(move |monitor| {
        let Some(monitor) = monitor else {
            if let Some(d) = on_done {
                d(false, "could not read monitor info".to_string());
            }
            return;
        };
        let hz = if low { 60 } else { max_refresh_rate(Some(&monitor)) };
        let name = monitor["name"].as_str().unwrap_or("").replace('"', "");
        let lua = format!(
            "hl.monitor({{output = \"{name}\", mode = \"{}x{}@{hz}\", position = \"{}x{}\", scale = {}}})",
            monitor["width"], monitor["height"], monitor["x"], monitor["y"], monitor["scale"]
        );
        if read_sysfs_int(Some(PANEL_OVERDRIVE_PATH)).is_none() {
            run_cmd(&["hyprctl", "eval", &lua], on_done);
        } else {
            let after: DoneCb = Box::new(move |ok, err| {
                if ok {
                    run_cmd(&["asusctl", "armoury", "set", "panel_overdrive", overdrive], on_done);
                } else if let Some(d) = on_done {
                    d(ok, err);
                }
            });
            run_cmd(&["hyprctl", "eval", &lua], Some(after));
        }
    });
}

// ---------------------------------------------------------------------------
// Battery
// ---------------------------------------------------------------------------

pub fn battery_charging_text() -> String {
    let status = read_sysfs(Some(BATTERY_STATUS_PATH)).unwrap_or_default();
    let power_now = read_sysfs_int(Some(BATTERY_POWER_NOW_PATH));
    if status == "Full" {
        return "Full".to_string();
    }
    if let Some(p) = power_now.filter(|p| *p != 0) {
        let verb = if status == "Charging" { "Charging" } else { "Discharging" };
        return format!("{verb}: {:.1}W", p as f64 / 1_000_000.0);
    }
    if status.is_empty() {
        "--".to_string()
    } else {
        status
    }
}

pub fn battery_health_text() -> Option<String> {
    let full = read_sysfs_int(Some(BATTERY_ENERGY_FULL_PATH)).filter(|v| *v != 0)?;
    let design = read_sysfs_int(Some(BATTERY_ENERGY_FULL_DESIGN_PATH)).filter(|v| *v != 0)?;
    Some(format!("Battery Health: {:.1}%", full as f64 / design as f64 * 100.0))
}
