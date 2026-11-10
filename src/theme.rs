//! Material You skin: colours from caelestia's `scheme.json`, the CSS built from them, the bundled
//! icon search path, and the colour helpers the RGB controls use.

use std::collections::HashMap;
use std::path::PathBuf;

use gtk::{gdk, glib};
use serde_json::Value;

use crate::hw::{ACCENT_CUSTOM, ACCENT_ECO, ACCENT_STANDARD, ACCENT_TURBO};
use crate::sys::{home_join, py_round};

pub type Colours = HashMap<String, String>;

/// Full Material 3 role set, matching caelestia's scheme.json keys 1:1; used only when that file
/// is missing (e.g. running off this machine).
const FALLBACK: [(&str, &str); 28] = [
    ("surface", "#1a1112"),
    ("surfaceDim", "#1a1112"),
    ("surfaceBright", "#413735"),
    ("surfaceContainerLowest", "#140c0d"),
    ("surfaceContainerLow", "#231819"),
    ("surfaceContainer", "#271c1d"),
    ("surfaceContainerHigh", "#322627"),
    ("surfaceContainerHighest", "#3d3132"),
    ("onSurface", "#f8e0e0"),
    ("onSurfaceVariant", "#bba6a6"),
    ("primary", "#f7b6b9"),
    ("onPrimary", "#603337"),
    ("primaryContainer", "#7a4a4d"),
    ("onPrimaryContainer", "#ffdadb"),
    ("secondary", "#e6bdbe"),
    ("onSecondary", "#55393a"),
    ("secondaryContainer", "#6d4f50"),
    ("onSecondaryContainer", "#ffdadb"),
    ("tertiary", "#e6bdbe"),
    ("onTertiary", "#55393a"),
    ("tertiaryContainer", "#6d4f50"),
    ("onTertiaryContainer", "#ffdadb"),
    ("outline", "#847172"),
    ("outlineVariant", "#524344"),
    ("error", "#f97758"),
    ("onError", "#450900"),
    ("errorContainer", "#852c0a"),
    ("onErrorContainer", "#ffdad2"),
];

fn fallback(name: &str) -> Option<&'static str> {
    FALLBACK.iter().find(|(k, _)| *k == name).map(|(_, v)| *v)
}

pub fn scheme_json_path() -> PathBuf {
    home_join(".local/state/caelestia/scheme.json")
}

/// The live palette, or the built-in fallback when the scheme file is missing or incomplete.
pub fn load_material_colors() -> Colours {
    let live = std::fs::read_to_string(scheme_json_path())
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|data| data.get("colours").and_then(Value::as_object).cloned())
        .filter(|c| c.get("surface").is_some_and(truthy) && c.get("primary").is_some_and(truthy));
    match live {
        Some(colours) => colours
            .into_iter()
            .map(|(k, v)| {
                let s = v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string());
                (k, if s.starts_with('#') { s } else { format!("#{s}") })
            })
            .collect(),
        None => FALLBACK.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null | Value::Bool(false) => false,
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_f64() != Some(0.0),
        _ => true,
    }
}

fn colour_or(name: &str, default: &str) -> String {
    load_material_colors().get(name).cloned().unwrap_or_else(|| default.to_string()).trim_start_matches('#').to_string()
}

pub fn theme_primary_hex() -> String {
    colour_or("primary", "#f7b6b9")
}

pub fn theme_secondary_hex() -> String {
    colour_or("secondary", "#e6bdbe")
}

/// `#rrggbb` -> (r, g, b, alpha) in 0..1; unparsable input gives mid-grey.
pub fn hex_to_rgba(hexval: &str, alpha: f64) -> (f64, f64, f64, f64) {
    let h = hexval.trim_start_matches('#');
    let channel = |i: usize| h.get(i..i + 2).and_then(|s| u8::from_str_radix(s, 16).ok()).map_or(0.5, |v| f64::from(v) / 255.0);
    (channel(0), channel(2), channel(4), alpha)
}

pub fn hex_to_gdk_rgba(hexval: &str) -> gdk::RGBA {
    gdk::RGBA::parse(format!("#{}", hexval.trim_start_matches('#'))).unwrap_or_else(|_| gdk::RGBA::new(0.5, 0.5, 0.5, 1.0))
}

pub fn gdk_rgba_to_hex(rgba: &gdk::RGBA) -> String {
    let c = |v: f32| py_round(f64::from(v) * 255.0);
    format!("{:02x}{:02x}{:02x}", c(rgba.red()), c(rgba.green()), c(rgba.blue()))
}

/// Material 3 colours/shapes in G-Helper's layout language: no boxed section backgrounds, just
/// surface/on-surface tokens, tonal button fills and the M3 shape scale on every control.
pub fn build_material_css(c: &Colours) -> String {
    let col = |name: &str| c.get(name).cloned().or_else(|| fallback(name).map(str::to_string)).unwrap_or_else(|| "#888888".to_string());
    let rgba_css = |hexval: &str, a: &str| {
        let (r, g, b, _) = hex_to_rgba(hexval, 1.0);
        format!("rgba({}, {}, {}, {})", (r * 255.0) as i64, (g * 255.0) as i64, (b * 255.0) as i64, a)
    };
    let (eco, standard, turbo) = (ACCENT_ECO, ACCENT_STANDARD, ACCENT_TURBO);
    let (surface, on_surface, on_surface_variant) = (col("surface"), col("onSurface"), col("onSurfaceVariant"));
    let (outline_variant, outline, primary, on_primary) = (col("outlineVariant"), col("outline"), col("primary"), col("onPrimary"));
    let (highest, high, container) = (col("surfaceContainerHighest"), col("surfaceContainerHigh"), col("surfaceContainer"));
    let (primary_container, on_primary_container) = (col("primaryContainer"), col("onPrimaryContainer"));
    let (error, on_error) = (col("error"), col("onError"));
    let (eco18, eco55) = (rgba_css(eco, "0.18"), rgba_css(eco, "0.55"));
    let (std18, std55) = (rgba_css(standard, "0.18"), rgba_css(standard, "0.55"));
    let (turbo18, turbo55) = (rgba_css(turbo, "0.18"), rgba_css(turbo, "0.55"));
    let custom = ACCENT_CUSTOM;
    format!(
        r#"
    window {{
        background-color: {surface};
        color: {on_surface};
    }}
    headerbar {{
        background-color: {surface};
        color: {on_surface};
        box-shadow: none;
        border-bottom: 1px solid {outline_variant};
        min-height: 40px;
    }}
    headerbar windowtitle title {{
        font-size: 95%;
        font-weight: 600;
    }}

    label.rog-title {{
        font-weight: 700;
        font-size: 100%;
    }}
    label.rog-subtitle {{
        color: {on_surface_variant};
        font-size: 85%;
    }}
    label.rog-hint {{
        color: {on_surface_variant};
        font-size: 80%;
        min-height: 1.4em;
    }}
    label.rog-empty {{
        color: {on_surface_variant};
        font-size: 90%;
    }}
    label.rog-section {{
        color: {on_surface_variant};
        font-weight: 700;
        font-size: 78%;
        letter-spacing: 0.5px;
    }}

    scale trough {{
        background-color: {highest};
        border: 1px solid {outline_variant};
        border-radius: 999px;
        min-height: 8px;
    }}
    scale trough highlight {{
        background-color: {primary};
        border-radius: 999px;
    }}
    scale slider {{
        background-color: {primary};
        border-radius: 999px;
        min-width: 18px;
        min-height: 18px;
        margin: -6px;
    }}

    switch {{
        background-color: {highest};
        border: 1px solid {outline};
        border-radius: 999px;
    }}
    switch:checked {{
        background-color: {primary};
        border-color: {primary};
    }}
    switch slider {{
        background-color: {container};
        border-radius: 999px;
    }}
    switch:checked slider {{
        background-color: {on_primary};
    }}

    button {{
        border-radius: 20px;
    }}
    button.suggested-action {{
        background-color: {primary};
        color: {on_primary};
    }}
    button.destructive-action {{
        background-color: {error};
        color: {on_error};
    }}
    button.rog-swatch {{
        border-radius: 999px;
        min-width: 24px;
        min-height: 24px;
        padding: 0;
        border: 1px solid {outline};
    }}
    button.rog-pill {{
        border-radius: 999px;
        padding: 2px 12px;
        font-size: 85%;
        background-color: {highest};
        color: {on_surface_variant};
    }}

    dropdown button {{
        background-color: {high};
        color: {on_surface};
        border: 1px solid {outline_variant};
        border-radius: 12px;
    }}

    /* Mode buttons: M3 tonal surface when unselected, fixed G-Helper accent glow
       (via one of the rog-accent-* classes) when selected -- see mode_button(). */
    button.rog-mode-btn {{
        background-color: {high};
        color: {on_surface_variant};
        border: 1.5px solid transparent;
        border-radius: 14px;
        box-shadow: none;
        padding: 10px 4px;
    }}
    button.rog-mode-btn:hover {{
        background-color: {highest};
    }}
    button.rog-mode-btn.rog-secondary {{
        color: {on_surface_variant};
    }}
    button.rog-mode-btn:checked.rog-accent-eco {{
        background-color: {eco18};
        color: {on_surface};
        border-color: {eco};
        box-shadow: 0 0 10px {eco55};
    }}
    button.rog-mode-btn:checked.rog-accent-standard {{
        background-color: {std18};
        color: {on_surface};
        border-color: {standard};
        box-shadow: 0 0 10px {std55};
    }}
    button.rog-mode-btn:checked.rog-accent-turbo {{
        background-color: {turbo18};
        color: {on_surface};
        border-color: {turbo};
        box-shadow: 0 0 10px {turbo55};
    }}
    button.rog-mode-btn:checked {{
        background-color: {primary_container};
        color: {on_primary_container};
    }}
    button.rog-mode-btn.rog-accent-custom {{
        color: {custom};
    }}

    togglebutton {{
        background-color: {high};
        color: {on_surface_variant};
        border: 1px solid {outline_variant};
        border-radius: 12px;
        box-shadow: none;
    }}
    togglebutton:checked {{
        background-color: {primary_container};
        color: {on_primary_container};
        border-color: transparent;
    }}

    spinbutton {{
        background-color: {high};
        border: 1px solid {outline_variant};
        border-radius: 12px;
    }}

    separator {{
        background-color: {outline_variant};
        min-height: 1px;
        margin: 6px 0;
    }}
    "#
    )
}

/// Where the bundled `hicolor` icon tree lives: next to an installed binary
/// (`<prefix>/bin/../share/icons`), in a source checkout (`target/release/../../icons`), or under
/// the XDG data dir. The first that actually holds the tray icon wins.
pub fn icons_dir() -> Option<PathBuf> {
    let probe = "hicolor/scalable/status/rog-control-symbolic.svg";
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(bin_dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(PathBuf::from)) {
        candidates.push(bin_dir.join("../share/icons"));
        candidates.push(bin_dir.join("../../icons"));
    }
    candidates.push(home_join(".local/share/icons"));
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        candidates.push(PathBuf::from(xdg).join("icons"));
    }
    candidates
        .into_iter()
        .find(|dir| dir.join(probe).is_file())
        .and_then(|dir| dir.canonicalize().ok())
}

/// Registers the bundled icons with the default display's icon theme. Needs GTK initialised.
pub fn register_icons() {
    if let (Some(display), Some(dir)) = (gdk::Display::default(), icons_dir()) {
        gtk::IconTheme::for_display(&display).add_search_path(dir);
    }
}

/// Builds and installs the application CSS on the default display.
pub fn install_css() {
    let Some(display) = gdk::Display::default() else { return };
    let provider = gtk::CssProvider::new();
    provider.connect_parsing_error(|_, section, err| eprintln!("rog-control: CSS error at {}: {}", section.to_str(), err));
    provider.load_from_string(&build_material_css(&load_material_colors()));
    gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
}

/// GTK loads the user's `~/.config/gtk-4.0/gtk.css` into every GTK4 app and prints a
/// "Theme parser error" for each rule it rejects. That file is not ours to fix (here its
/// `row:insensitive` is GTK3 syntax), so those warnings are dropped; errors in this app's own CSS
/// are reported by [`install_css`] instead. GTK logs through GLib's structured API, so this has
/// to be a log writer, and it must be installed before anything logs.
pub fn quiet_user_theme_warnings() {
    glib::log_set_writer_func(|level, fields| {
        let user_theme_noise = fields
            .iter()
            .any(|f| f.key() == "MESSAGE" && f.value_str().is_some_and(|m| m.starts_with("Theme parser error: gtk.css")));
        if user_theme_noise {
            glib::LogWriterOutput::Handled
        } else {
            glib::log_writer_default(level, fields)
        }
    });
}
