//! The session-bus control service the hotkeys talk to, and the CLI client that calls it.
//!
//! `com.orneyfish.RogControl.Hotkeys` is a plain D-Bus method interface, deliberately separate
//! from GApplication's own remote-command-line machinery (which hung indefinitely on this
//! system's session bus). It also carries `OpenWindow(s)`, a test seam that opens any of the four
//! windows without simulated clicks; no CLI flag exposes it.

use std::rc::Rc;
use std::time::Duration;

use gtk::gio::{self, prelude::*};
use gtk::glib;

use crate::hw::APP_ID;

pub const CONTROL_NAME: &str = "com.orneyfish.RogControl.Hotkeys";
pub const CONTROL_PATH: &str = "/com/orneyfish/RogControl/Hotkeys";

const CONTROL_XML: &str = r#"
<node>
  <interface name="com.orneyfish.RogControl.Hotkeys">
    <method name="Open"/>
    <method name="CycleProfile"/>
    <method name="CycleProfileBack"/>
    <method name="OpenWindow"><arg type="s" name="name" direction="in"/></method>
  </interface>
</node>"#;

type OpenWindowFn = Box<dyn Fn(&str) -> Result<(), String>>;

/// What the service does for each method. `open_window` returns an error message for a name it
/// does not know or a window it cannot build.
pub struct ControlHandlers {
    pub open: Box<dyn Fn()>,
    pub cycle: Box<dyn Fn()>,
    pub cycle_back: Box<dyn Fn()>,
    pub open_window: OpenWindowFn,
}

impl ControlHandlers {
    /// `cycle_step(+1)` is the next profile, `cycle_step(-1)` the previous one: `CycleProfile`
    /// goes forward and `CycleProfileBack` backward.
    pub fn new(
        open: impl Fn() + 'static,
        cycle_step: impl Fn(i32) + 'static,
        open_window: impl Fn(&str) -> Result<(), String> + 'static,
    ) -> Self {
        let cycle_step = Rc::new(cycle_step);
        let back = cycle_step.clone();
        Self {
            open: Box::new(open),
            cycle: Box::new(move || cycle_step(1)),
            cycle_back: Box::new(move || back(-1)),
            open_window: Box::new(open_window),
        }
    }
}

/// Runs one method call; `Err` carries the D-Bus error to send back.
pub fn dispatch(handlers: &ControlHandlers, method: &str, params: &glib::Variant) -> Result<(), (gio::DBusError, String)> {
    match method {
        "Open" => (handlers.open)(),
        "CycleProfile" => (handlers.cycle)(),
        "CycleProfileBack" => (handlers.cycle_back)(),
        "OpenWindow" => {
            let name = params.child_value(0);
            let name = name.str().ok_or((gio::DBusError::InvalidArgs, "expected a window name".to_string()))?;
            (handlers.open_window)(name).map_err(|msg| (gio::DBusError::InvalidArgs, msg))?;
        }
        other => return Err((gio::DBusError::UnknownMethod, format!("no such method: {other}"))),
    }
    Ok(())
}

/// Exports the control object on `connection`. The well-known name is requested separately
/// ([`own_control_name`]) so the object is in place before anyone can see the name.
pub fn register_control(connection: &gio::DBusConnection, handlers: ControlHandlers) -> Result<gio::RegistrationId, glib::Error> {
    let node = gio::DBusNodeInfo::for_xml(CONTROL_XML)?;
    let iface = node.lookup_interface(CONTROL_NAME).ok_or_else(|| glib::Error::new(gio::DBusError::Failed, "interface missing"))?;
    let handlers = Rc::new(handlers);
    connection
        .register_object(CONTROL_PATH, &iface)
        .method_call(move |_conn, _sender, _path, _iface, method, params, invocation| match method {
            // Hotkey calls answer first and act on the next idle turn: the caller (a key bind)
            // must not wait for a window to be built.
            "Open" | "CycleProfile" | "CycleProfileBack" => {
                invocation.return_value(None);
                let (handlers, method) = (handlers.clone(), method.to_string());
                glib::idle_add_local_once(move || {
                    let _ = dispatch(&handlers, &method, &().to_variant());
                });
            }
            _ => match dispatch(&handlers, method, &params) {
                Ok(()) => invocation.return_value(None),
                Err((code, msg)) => invocation.return_error(code, &msg),
            },
        })
        .build()
}

/// Claims `com.orneyfish.RogControl.Hotkeys`; `on_lost` runs if another instance already has it.
pub fn own_control_name(connection: &gio::DBusConnection, on_lost: impl Fn() + 'static) -> gio::OwnerId {
    gio::bus_own_name_on_connection(connection, CONTROL_NAME, gio::BusNameOwnerFlags::DO_NOT_QUEUE, |_, _| {}, move |_, _| on_lost())
}

fn session_bus() -> Result<gio::DBusConnection, glib::Error> {
    gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
}

/// Is `name` currently owned on the session bus? Unreachable bus counts as "no".
pub fn name_has_owner(name: &str) -> bool {
    let Ok(bus) = session_bus() else { return false };
    bus.call_sync(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "NameHasOwner",
        Some(&(name,).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        2000,
        gio::Cancellable::NONE,
    )
    .ok()
    .and_then(|reply| reply.child_value(0).get::<bool>())
    .unwrap_or(false)
}

/// Which control method a command line asks for; `None` for anything else.
pub fn method_for_args(args: &[String]) -> Option<&'static str> {
    let has = |flag: &str| args.iter().any(|a| a == flag);
    if has("--cycle-profile") {
        Some("CycleProfile")
    } else if has("--cycle-profile-back") {
        Some("CycleProfileBack")
    } else if has("--open") {
        Some("Open")
    } else {
        None
    }
}

/// CLI mode: call the running instance over the session bus and return. Never starts GTK. If the
/// service is not reachable, says so on stderr; the exit status is 0 either way (a hotkey with
/// nothing to talk to is not an error worth a notification).
pub fn call_service(method: &str) {
    let result = session_bus().and_then(|bus| {
        bus.call_sync(
            Some(CONTROL_NAME),
            CONTROL_PATH,
            CONTROL_NAME,
            method,
            None,
            None,
            gio::DBusCallFlags::NONE,
            Duration::from_secs(5).as_millis() as i32,
            gio::Cancellable::NONE,
        )
    });
    if let Err(e) = result {
        eprintln!("ROG Control service not reachable: {}", e.message());
    }
}

/// True when a previous instance already owns the application id.
pub fn instance_running() -> bool {
    name_has_owner(APP_ID) || name_has_owner(CONTROL_NAME)
}
