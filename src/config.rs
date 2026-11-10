//! Persisted user config: automation memory (remembered profile / keyboard level per power
//! source, GPU-Optimized and Screen-Auto opt-ins) and the last-set power limits.
//!
//! The file is `$HOME/.config/rog-control/config.json` with the exact schema the Python app
//! wrote. It is held as a plain JSON value so keys this version does not know about survive a
//! load/save round trip. Everything else (current profile, battery limit, fan curves, RGB
//! state) is read live from asusctl/sysfs and never cached here.

use std::cell::RefCell;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde_json::{json, Map, Value};

use crate::sys::home_join;

/// The one config object shared (and mutated in place) by every panel and window.
pub type Cfg = Rc<RefCell<Value>>;
/// Persists the shared config.
pub type SaveCb = Rc<dyn Fn()>;

pub fn config_path() -> PathBuf {
    home_join(".config/rog-control/config.json")
}

pub fn defaults() -> Value {
    json!({
        "automation": {
            "gpu_optimized": false,
            "screen_auto": false,
            "profile_battery": null,
            "profile_ac": null,
            "kbd_battery": null,
            "kbd_ac": null,
        },
        // PL1/PL2/GPU boost/temp target/clock offsets have no hardware value to read back on
        // this laptop, so the last value a user picked is remembered here instead.
        "power_limits": {
            "pl1": null,
            "pl2": null,
            "apply_limits": false,
            "boost": null,
            "temp_target": null,
            "core_clock": null,
            "mem_clock": null,
            "gpu_power": null,
        },
    })
}

/// Fills every key missing from `cfg` with its default, recursing into objects. A value of the
/// wrong shape where an object is expected is replaced rather than crashing on it.
pub fn merge_defaults(cfg: Value, defaults: &Value) -> Value {
    let mut cfg = match cfg {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    if let Value::Object(defs) = defaults {
        for (key, default) in defs {
            if default.is_object() {
                let existing = cfg.get(key).cloned().unwrap_or(Value::Null);
                cfg.insert(key.clone(), merge_defaults(existing, default)); // keeps the key's position
            } else {
                cfg.entry(key.clone()).or_insert_with(|| default.clone());
            }
        }
    }
    Value::Object(cfg)
}

/// A missing or corrupt file gives the defaults; nothing is written.
pub fn load_from(path: &Path) -> Value {
    let parsed = std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str(&text).ok());
    merge_defaults(parsed.unwrap_or(Value::Null), &defaults())
}

pub fn load() -> Value {
    load_from(&config_path())
}

/// True when `merge_defaults` would throw something the file holds away: the file is not a JSON
/// object, or a section that must be an object holds something else (`null` holds nothing).
fn would_discard(value: &Value, defaults: &Value) -> bool {
    match (value, defaults) {
        (Value::Null, _) => false,
        (Value::Object(map), Value::Object(defs)) => defs.iter().any(|(key, default)| map.get(key).is_some_and(|v| would_discard(v, default))),
        (_, Value::Object(_)) => true,
        _ => false,
    }
}

/// `<path>.corrupt`, or the first of `<path>.corrupt.1`, `.2`, ... that does not exist yet.
fn free_backup_name(path: &Path) -> Option<PathBuf> {
    (0..1000).find_map(|n| {
        let mut name = path.as_os_str().to_owned();
        name.push(".corrupt");
        if n > 0 {
            name.push(format!(".{n}"));
        }
        let name = PathBuf::from(name);
        std::fs::symlink_metadata(&name).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound).then_some(name)
    })
}

/// A file that cannot be read as the config (unreadable, not JSON, not an object, or an object
/// whose sections `merge_defaults` would replace) was read as defaults at startup and never
/// rewritten there. The save about to replace it would destroy it, so it is kept first, without
/// being read, as `config.json.corrupt` (or the next free `.corrupt.N`; an earlier copy is never
/// overwritten). An empty or healthy file needs no copy. `Err` means no copy could be made: the
/// caller must not replace the file.
fn keep_unusable(path: &Path) -> std::io::Result<()> {
    let unusable = match std::fs::read(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
        // An empty file holds nothing to keep (and an empty copy is noise).
        Ok(bytes) if bytes.trim_ascii().is_empty() => false,
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes).map_or(true, |v| would_discard(&v, &defaults())),
    };
    if !unusable {
        return Ok(());
    }
    let kept = free_backup_name(path)
        .ok_or_else(|| std::io::Error::other("too many .corrupt copies already"))
        .and_then(|backup| match std::fs::hard_link(path, &backup) {
            // Links refused (another owner, a directory, an odd filesystem): move it aside instead.
            Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => std::fs::rename(path, &backup).map(|()| backup),
            other => other.map(|()| backup),
        });
    match kept {
        Ok(backup) => {
            eprintln!("rog-control: {} cannot be used as the config; kept it as {}", path.display(), backup.display());
            Ok(())
        }
        // The caller logs this (`could not save config: ...`), so the give-up is never silent.
        Err(e) => Err(std::io::Error::new(e.kind(), format!("{} cannot be used as the config and no copy could be made ({e}); it was left untouched", path.display()))),
    }
}

/// Atomic: write `config.json.tmp`, then rename over the real file. Whatever unusable file is
/// there is kept first (see [`keep_unusable`]); if that fails the file is left exactly as it was.
pub fn save_to(path: &Path, cfg: &Value) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let text = serde_json::to_string_pretty(cfg).map_err(std::io::Error::other)?;
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    drop(f);
    if let Err(e) = keep_unusable(path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, path)
}

pub fn save(cfg: &Value) -> std::io::Result<()> {
    save_to(&config_path(), cfg)
}

pub fn new_cfg(value: Value) -> Cfg {
    Rc::new(RefCell::new(value))
}

/// `section.key` as a bool (false when absent or not a bool).
pub fn get_bool(cfg: &Cfg, section: &str, key: &str) -> bool {
    cfg.borrow()[section][key].as_bool().unwrap_or(false)
}

/// `section.key` as a string, `None` when absent, null or not a string.
pub fn get_str(cfg: &Cfg, section: &str, key: &str) -> Option<String> {
    cfg.borrow()[section][key].as_str().map(str::to_string)
}

/// `section.key` as a number, `None` when absent or null.
pub fn get_f64(cfg: &Cfg, section: &str, key: &str) -> Option<f64> {
    cfg.borrow()[section][key].as_f64()
}

/// Sets `section.key`, creating the section if a hand-edited file lost it.
pub fn set(cfg: &Cfg, section: &str, key: &str, value: Value) {
    let mut root = cfg.borrow_mut();
    if !root.is_object() {
        *root = Value::Object(Map::new());
    }
    let section_val = root.as_object_mut().map(|m| m.entry(section).or_insert_with(|| Value::Object(Map::new())));
    if let Some(section_val) = section_val {
        if !section_val.is_object() {
            *section_val = Value::Object(Map::new());
        }
        section_val[key] = value;
    }
}
