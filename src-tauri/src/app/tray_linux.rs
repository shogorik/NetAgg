//! The Linux tray icon, as a StatusNotifierItem of its own (ksni): a left
//! click toggles the popup, a right click opens the menu, as on Windows.
//! Tauri's tray goes through libappindicator, which only ever shows the menu.

use std::sync::Mutex;

use ksni::menu::StandardItem;
use ksni::{MenuItem, TrayMethods};
use tauri::image::Image;
use tauri::{AppHandle, Manager};

use super::{commands, popup};

const ICON_ON: &[u8] = include_bytes!("../../icons/tray-on.png");
const ICON_OFF: &[u8] = include_bytes!("../../icons/tray-off.png");

struct NetTray {
    app: AppHandle,
    active: bool,
    tooltip: String,
    icon_on: Vec<ksni::Icon>,
    icon_off: Vec<ksni::Icon>,
}

impl ksni::Tray for NetTray {
    fn id(&self) -> String {
        "netagg".into()
    }

    fn title(&self) -> String {
        "NetAgg".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        match self.active {
            true => self.icon_on.clone(),
            false => self.icon_off.clone(),
        }
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: self.tooltip.clone(),
            ..Default::default()
        }
    }

    fn activate(&mut self, x: i32, y: i32) {
        // Wayland doesn't say where the click was: (0, 0).
        if (x, y) != (0, 0) {
            popup::set_anchor(&self.app, x.into(), y.into());
        }
        popup::toggle(&self.app);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem {
                label: "Open NetAgg".into(),
                activate: Box::new(|tray: &mut Self| popup::show(&tray.app)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: if self.active { "Turn off" } else { "Turn on" }.into(),
                activate: Box::new(|tray: &mut Self| {
                    let (app, on) = (tray.app.clone(), !tray.active);
                    tauri::async_runtime::spawn(async move {
                        if let Err(err) = commands::switch(&app, on).await {
                            log::warn!("{err}");
                        }
                    });
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|tray: &mut Self| tray.app.exit(0)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// The running tray, and what it shows now.
struct TrayState {
    handle: Mutex<Option<ksni::Handle<NetTray>>>,
    shown: Mutex<Option<(bool, String)>>,
}

/// SNI pixmaps are ARGB32 in network byte order; the PNG decodes to RGBA.
fn pixmap(png: &[u8]) -> tauri::Result<Vec<ksni::Icon>> {
    let image = Image::from_bytes(png)?;
    let data = image
        .rgba()
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[r, g, b, a]| [a, r, g, b])
        .collect();
    Ok(vec![ksni::Icon {
        width: image.width() as i32,
        height: image.height() as i32,
        data,
    }])
}

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let tray = NetTray {
        app: app.clone(),
        active: false,
        tooltip: "NetAgg — off".into(),
        icon_on: pixmap(ICON_ON)?,
        icon_off: pixmap(ICON_OFF)?,
    };
    app.manage(TrayState {
        handle: Mutex::new(None),
        shown: Mutex::new(None),
    });
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        match tray.spawn().await {
            Ok(handle) => *app.state::<TrayState>().handle.lock().unwrap() = Some(handle),
            // No tray on this desktop: the popup still opens from the app menu.
            Err(err) => log::warn!("no tray icon: {err}"),
        }
    });
    Ok(())
}

/// Shows whether NetAgg is combining networks, in the icon, its tooltip
/// and the menu.
pub fn set_active(app: &AppHandle, active: bool, tooltip: &str) -> tauri::Result<()> {
    let state = app.state::<TrayState>();
    let Some(handle) = state.handle.lock().unwrap().clone() else {
        return Ok(());
    };
    let wanted = (active, tooltip.to_owned());
    let mut shown = state.shown.lock().unwrap();
    if shown.as_ref() == Some(&wanted) {
        return Ok(());
    }
    *shown = Some(wanted);
    let tooltip = tooltip.to_owned();
    tauri::async_runtime::spawn(async move {
        handle
            .update(|tray| {
                tray.active = active;
                tray.tooltip = tooltip;
            })
            .await;
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_become_argb() {
        let icon = &pixmap(ICON_ON).unwrap()[0];
        assert!(icon.width > 0 && icon.height > 0);
        assert_eq!(icon.data.len(), (icon.width * icon.height * 4) as usize);
        let rgba = Image::from_bytes(ICON_ON).unwrap();
        assert_eq!(
            icon.data[..4],
            [
                rgba.rgba()[3],
                rgba.rgba()[0],
                rgba.rgba()[1],
                rgba.rgba()[2]
            ]
        );
    }
}
