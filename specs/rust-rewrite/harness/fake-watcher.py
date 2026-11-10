# Stand-in for the shell's org.kde.StatusNotifierWatcher on the CURRENT session bus.
# usage: dbus-run-session -- python3 fake-watcher.py <file>   (appends each registered item to <file>)
import sys
import dbus, dbus.service
from dbus.mainloop.glib import DBusGMainLoop
from gi.repository import GLib

DBusGMainLoop(set_as_default=True)
bus = dbus.SessionBus()
_name = dbus.service.BusName("org.kde.StatusNotifierWatcher", bus)


class Watcher(dbus.service.Object):
    @dbus.service.method("org.kde.StatusNotifierWatcher", in_signature="s")
    def RegisterStatusNotifierItem(self, service):
        with open(sys.argv[1], "a") as f:
            f.write(service + "\n")


Watcher(bus, "/StatusNotifierWatcher")
GLib.MainLoop().run()
