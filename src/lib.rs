//! ROG Control: a GTK4/libadwaita tray app for ASUS ROG laptop hardware (performance profiles,
//! GPU MUX / dGPU switching, fan curves, RGB keyboard lighting, battery limit, refresh rate).
//! All hardware control is delegated to `asusctl`, `nvidia-smi`, `nvidia-settings`,
//! `powerprofilesctl`, `hyprctl`, `systemctl`, `notify-send`, sysfs reads and asusd's D-Bus
//! properties; nothing here writes sysfs.

pub mod app;
pub mod automation;
pub mod config;
pub mod control;
pub mod extra;
pub mod fans;
pub mod hw;
pub mod lighting;
pub mod panels;
pub mod popup;
pub mod sys;
pub mod theme;
pub mod tray;
pub mod widgets;
