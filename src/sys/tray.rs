//! The system tray icon and its menu.
//!
//! The tray icon must be created on the thread that runs the message loop, which
//! on Windows is the main thread, so this is built from inside the app.
//! Menu activations are delivered through the `muda` event handler channel set
//! up at construction time, instead of relying on global channels.

use std::sync::mpsc;

pub enum TrayEvent {
    /// Show (or focus) the main window.
    Show,
    /// Quit for real.
    Exit,
    /// Toggle always-on-top.
    ToggleTopmost,
}

#[allow(dead_code)]
pub struct Tray {
    icon: tray_icon::TrayIcon,
    show: tray_icon::menu::MenuItem,
    topmost: tray_icon::menu::CheckMenuItem,
    exit: tray_icon::menu::MenuItem,
    event_rx: mpsc::Receiver<TrayEvent>,
}

impl Tray {
    pub fn new() -> Option<Self> {
        let icon = crate::sys::icon::app_icon();
        let icon = tray_icon::Icon::from_rgba(icon.rgba, icon.width, icon.height).ok()?;

        let menu = tray_icon::menu::Menu::new();
        let show = tray_icon::menu::MenuItem::new("Show Tasky", true, None);
        let topmost = tray_icon::menu::CheckMenuItem::new("Always on top", true, false, None);
        let exit = tray_icon::menu::MenuItem::new("Exit", true, None);

        menu.append_items(&[
            &show,
            &topmost,
            &tray_icon::menu::PredefinedMenuItem::separator(),
            &exit,
        ])
        .ok()?;

        // We need the item ids before move-ing the items into the icon builder.
        let show_id = show.id().clone();
        let topmost_id = topmost.id().clone();
        let exit_id = exit.id().clone();
        let (tx, rx) = mpsc::channel();
        let _ = tx.clone();

        tray_icon::menu::MenuEvent::set_event_handler(Some(Box::new(move |event: tray_icon::menu::MenuEvent| {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(
                r"C:\Users\jury\AppData\Local\Temp\opencode\tasky_debug.log",
            ) {
                use std::io::Write;
                let _ = writeln!(f, "HANDLER MenuEvent id={:?}", event.id);
            }
            if event.id == show_id {
                // Act immediately instead of queueing through the app loop,
                // which eframe pauses while its window is hidden.
                crate::sys::instance::show_stored_main_window();
            } else if event.id == topmost_id {
                let next = !crate::sys::instance::is_always_on_top();
                crate::sys::instance::apply_always_on_top(next);
            } else if event.id == exit_id {
                std::process::exit(0);
            }
        })));

        let built = tray_icon::TrayIconBuilder::new()
            .with_tooltip("Tasky")
            .with_icon(icon)
            .with_menu(Box::new(menu))
            .build()
            .ok()?;

        // Keep the _ (placeholder) etc out of the dirt; just tidy up.
        let _ = tx;

        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(
            r"C:\Users\jury\AppData\Local\Temp\opencode\tasky_debug.log",
        ) {
            use std::io::Write;
            let _ = writeln!(
                f,
                "tray ids show={:?} topmost={:?} exit={:?}",
                *show.id(),
                *topmost.id(),
                *exit.id()
            );
        }

        Some(Self {
            icon: built,
            show,
            topmost,
            exit,
            event_rx: rx,
        })
    }

    /// Drain queued menu events into a fresh vector.
    pub fn poll(&self) -> Vec<TrayEvent> {
        let mut out = Vec::new();
        while let Ok(event) = self.event_rx.try_recv() {
            out.push(event);
        }
        out
    }

    pub fn set_topmost_checked(&self, checked: bool) {
        self.topmost.set_checked(checked);
    }
}
