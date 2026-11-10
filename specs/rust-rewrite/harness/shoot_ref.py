# Screenshots the REFERENCE (Python) UI windows one at a time, read-only: python3 shoot_ref.py <repo-or-worktree> <outdir>
# Run with PATH=$FAKE:$PATH so nothing can write to hardware. Uses app id ...RogControl.dev1 (the Hyprland rules match it).
import json, subprocess, sys
sys.path.insert(0, sys.argv[1])
import gi
gi.require_version("Gtk", "4.0"); gi.require_version("Adw", "1")
from gi.repository import Adw, Gdk, GLib, Gtk
import main, config

OUT = sys.argv[2]
app = Adw.Application(application_id="com.orneyfish.RogControl.dev1")

def shoot(name):
    clients = json.loads(subprocess.run(["/usr/bin/hyprctl", "clients", "-j"], capture_output=True, text=True).stdout)
    for c in clients:
        if c["class"].startswith("com.orneyfish.RogControl") and c["title"] == name:
            (x, y), (w, h) = c["at"], c["size"]
            subprocess.run(["/usr/bin/grim", "-g", f"{x},{y} {w}x{h}", f"{OUT}/{name.replace(' ', '_')}.png"])
            print("shot", name, w, h, x, y, flush=True)
            return
    print("NOT FOUND", name, flush=True)

def activate(app):
    main.register_icons()
    css = Gtk.CssProvider()
    css.load_from_data(main.build_material_css(main.load_material_colors()).encode())
    Gtk.StyleContext.add_provider_for_display(Gdk.Display.get_default(), css, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)
    popup = main.RogPopup(app, config.load(), lambda: None, lambda: False, app.quit)
    popup.toggle()
    steps = [
        (2500, lambda: shoot("ROG Control")),
        (2600, popup.open_fans), (5500, lambda: shoot("Fans and Power")), (5600, lambda: popup.fans_window.close()),
        (6000, popup.open_lighting), (8000, lambda: shoot("Keyboard Lighting")), (8100, lambda: popup.lighting_window.close()),
        (8500, popup.open_extra), (10500, lambda: shoot("Extra")), (10600, app.quit),
    ]
    for ms, fn in steps:
        GLib.timeout_add(ms, lambda f=fn: (f(), False)[1])

app.connect("activate", activate)
app.run(None)
