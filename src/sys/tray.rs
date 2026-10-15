//! The system tray icon and its menu.
//!
//! Menu and icon events are handled directly in their callbacks, on the thread
//! running the Win32 message loop, rather than queued for the app: eframe does
//! not run the app's update while its window is hidden.

use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::sys::instance;

pub struct Tray {
    _icon: TrayIcon,
    topmost: CheckMenuItem,
}

impl Tray {
    /// Must be called on the main thread. `on_exit` runs right before "Exit"
    /// ends the process.
    pub fn new(on_exit: impl Fn() + Send + Sync + 'static) -> Option<Self> {
        let icon = crate::sys::icon::app_icon();
        let icon = tray_icon::Icon::from_rgba(icon.rgba, icon.width, icon.height).ok()?;

        let menu = Menu::new();
        let show = MenuItem::new("Show Tasky", true, None);
        let topmost = CheckMenuItem::new("Always on top", true, false, None);
        let exit = MenuItem::new("Exit", true, None);
        menu.append_items(&[&show, &topmost, &PredefinedMenuItem::separator(), &exit])
            .ok()?;

        let show_id = show.id().clone();
        let topmost_id = topmost.id().clone();
        let exit_id = exit.id().clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if event.id == show_id {
                instance::show_main_window();
            } else if event.id == topmost_id {
                instance::set_always_on_top(!instance::is_always_on_top());
            } else if event.id == exit_id {
                on_exit();
                std::process::exit(0);
            }
        }));

        TrayIconEvent::set_event_handler(Some(|event: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                instance::show_main_window();
            }
        }));

        let icon = TrayIconBuilder::new()
            .with_tooltip("Tasky")
            .with_icon(icon)
            .with_menu(Box::new(menu))
            .build()
            .ok()?;

        Some(Self {
            _icon: icon,
            topmost,
        })
    }

    pub fn set_topmost_checked(&self, checked: bool) {
        self.topmost.set_checked(checked);
    }
}
