//! StatusNotifierItem tray icon, implemented directly on the session bus against
//! `org.kde.StatusNotifierWatcher` (owned by the shell itself). libayatana-appindicator is not
//! used: it never exposes SNI's `Activate` (left click) to the application.

use gtk::gio::{self, prelude::*};
use gtk::glib;

use crate::hw::TRAY_ICON_NAME;

pub const SNI_PATH: &str = "/StatusNotifierItem";
const SNI_IFACE: &str = "org.kde.StatusNotifierItem";

const SNI_XML: &str = r#"
<node>
  <interface name="org.kde.StatusNotifierItem">
    <property name="Category" type="s" access="read"/>
    <property name="Id" type="s" access="read"/>
    <property name="Title" type="s" access="read"/>
    <property name="Status" type="s" access="read"/>
    <property name="IconName" type="s" access="read"/>
    <property name="IconThemePath" type="s" access="read"/>
    <property name="ItemIsMenu" type="b" access="read"/>
    <property name="ToolTip" type="(sa(iiay)ss)" access="read"/>
    <method name="Activate"><arg type="i" name="x" direction="in"/><arg type="i" name="y" direction="in"/></method>
    <method name="SecondaryActivate"><arg type="i" name="x" direction="in"/><arg type="i" name="y" direction="in"/></method>
    <method name="ContextMenu"><arg type="i" name="x" direction="in"/><arg type="i" name="y" direction="in"/></method>
    <method name="Scroll"><arg type="i" name="delta" direction="in"/><arg type="s" name="orientation" direction="in"/></method>
    <signal name="NewIcon"/>
    <signal name="NewStatus"><arg type="s" name="status"/></signal>
  </interface>
</node>"#;

const TOOLTIP_TEXT: &str = "Left-click: open · scroll: cycle profile · right-click: quit · ROG key / Ctrl+Shift+F12: open";

pub struct TrayHandlers {
    pub activate: Box<dyn Fn()>,
    pub secondary_activate: Box<dyn Fn()>,
    pub scroll: Box<dyn Fn(i32)>,
}

/// Value of one SNI property; `icon_theme_path` is where the installed `hicolor` icon tree lives.
pub fn property(name: &str, icon_theme_path: &str) -> glib::Variant {
    match name {
        "Category" => "Hardware".to_variant(),
        "Id" => "rog-control".to_variant(),
        "Title" => "ROG Control".to_variant(),
        "Status" => "Active".to_variant(),
        "IconName" => TRAY_ICON_NAME.to_variant(),
        "IconThemePath" => icon_theme_path.to_variant(),
        "ItemIsMenu" => false.to_variant(),
        "ToolTip" => ("", Vec::<(i32, i32, Vec<u8>)>::new(), "ROG Control", TOOLTIP_TEXT).to_variant(),
        _ => "".to_variant(),
    }
}

/// Exports the tray item at `/StatusNotifierItem` on `connection`.
pub fn register_item(
    connection: &gio::DBusConnection,
    icon_theme_path: String,
    handlers: TrayHandlers,
) -> Result<gio::RegistrationId, glib::Error> {
    let node = gio::DBusNodeInfo::for_xml(SNI_XML)?;
    let iface = node.lookup_interface(SNI_IFACE).ok_or_else(|| glib::Error::new(gio::DBusError::Failed, "interface missing"))?;
    let handlers = std::rc::Rc::new(handlers);
    connection
        .register_object(SNI_PATH, &iface)
        .method_call(move |_conn, _sender, _path, _iface, method, params, invocation| {
            invocation.return_value(None);
            // Reply first, act on the next idle turn (the caller must not wait on window work).
            let handlers = handlers.clone();
            match method {
                "Activate" => {
                    glib::idle_add_local_once(move || (handlers.activate)());
                }
                "SecondaryActivate" => {
                    glib::idle_add_local_once(move || (handlers.secondary_activate)());
                }
                "Scroll" => {
                    let delta = params.child_value(0).get::<i32>().unwrap_or(0);
                    glib::idle_add_local_once(move || (handlers.scroll)(delta));
                }
                _ => {}
            }
        })
        .property(move |_conn, _sender, _path, _iface, name| property(name, &icon_theme_path))
        .build()
}

/// Registers the item with the watcher, retrying every 3 s up to `attempts_left` more times: the
/// shell (which owns the watcher) may still be starting when we are.
pub fn register_with_watcher(connection: gio::DBusConnection, attempts_left: u32) {
    let Some(unique) = connection.unique_name() else { return };
    let service_id = format!("{unique}{SNI_PATH}");
    let retry_connection = connection.clone();
    connection.call(
        Some("org.kde.StatusNotifierWatcher"),
        "/StatusNotifierWatcher",
        "org.kde.StatusNotifierWatcher",
        "RegisterStatusNotifierItem",
        Some(&(service_id,).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        5000,
        gio::Cancellable::NONE,
        move |result| {
            if result.is_err() && attempts_left > 0 {
                glib::timeout_add_seconds_local_once(3, move || register_with_watcher(retry_connection, attempts_left - 1));
            }
        },
    );
}
