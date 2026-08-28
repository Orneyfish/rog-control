"""Persisted user config: automation memory. Mirrors G-Helper's own model (there is
no single global "enable automation" switch there either -- see AppConfig.cs /
Mode/ModeControl.cs): performance-mode and keyboard-backlight level are learned
implicitly, one remembered value per power source, with no opt-in UI at all; GPU
mode and screen refresh rate instead have their own explicit "Optimized"/"Auto"
choice among their normal options. Everything else (current profile, battery
limit, fan curves, RGB state) is read live from asusctl/sysfs."""
import json
import os

CONFIG_DIR = os.path.expanduser("~/.config/rog-control")
CONFIG_PATH = os.path.join(CONFIG_DIR, "config.json")

DEFAULTS = {
    "automation": {
        "gpu_optimized": False,
        "screen_auto": False,
        "profile_battery": None,
        "profile_ac": None,
        "kbd_battery": None,
        "kbd_ac": None,
    },
}


def _merge_defaults(cfg, defaults):
    for key, value in defaults.items():
        if isinstance(value, dict):
            cfg[key] = _merge_defaults(cfg.get(key, {}), value)
        else:
            cfg.setdefault(key, value)
    return cfg


def load():
    try:
        with open(CONFIG_PATH) as f:
            cfg = json.load(f)
    except (OSError, ValueError):
        cfg = {}
    return _merge_defaults(cfg, DEFAULTS)


def save(cfg):
    os.makedirs(CONFIG_DIR, exist_ok=True)
    tmp_path = CONFIG_PATH + ".tmp"
    with open(tmp_path, "w") as f:
        json.dump(cfg, f, indent=2)
    os.replace(tmp_path, CONFIG_PATH)
