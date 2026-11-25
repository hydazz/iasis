//! The tray icon, where the desktop shows StatusNotifierItems: each board's
//! devices with their actions, the download under way, and the window,
//! Preferences and Quit.

use iasis_api::{Hue, Request, State};
use ksni::{
    MenuItem, ToolTip,
    blocking::{Handle, TrayMethods},
    menu::{StandardItem, SubMenu},
};

use crate::device::{self, Actions, Board, Device, DeviceId};

/// What a choice in the tray asks the app to do.
#[derive(Clone, Debug)]
pub enum Command {
    Open,
    Show(DeviceId),
    Restore(DeviceId),
    Perform(Vec<Request>),
    Preferences,
    Quit,
}

pub struct Tray {
    pub state: Option<State>,
    commands: async_channel::Sender<Command>,
}

/// Show the tray icon, unless the desktop has nowhere to show it.
pub fn spawn(commands: async_channel::Sender<Command>) -> Option<Handle<Tray>> {
    Tray {
        state: None,
        commands,
    }
    .spawn()
    .inspect_err(|error| eprintln!("No tray icon: {error}"))
    .ok()
}

/// How the boards stand, as the icon shows it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Offline,
    Ready,
    Busy,
    Attention,
}

impl Tray {
    fn status(&self) -> Status {
        let Some(state) = &self.state else {
            return Status::Offline;
        };
        if state.boards.is_empty() {
            return Status::Offline;
        }
        let mut ports = state.boards.iter().flat_map(|board| &board.ports);
        if state.boards.iter().any(|board| board.notice.is_some())
            || ports.clone().any(|port| port.light.hue == Hue::Red)
        {
            Status::Attention
        } else if state.images.fetch.is_some() || ports.any(|port| port.light.pulse) {
            Status::Busy
        } else {
            Status::Ready
        }
    }

    fn item(&self, label: impl Into<String>, command: Option<Command>) -> MenuItem<Self> {
        StandardItem {
            label: label.into(),
            enabled: command.is_some(),
            activate: Box::new(move |tray: &mut Self| {
                if let Some(command) = &command {
                    let _ = tray.commands.send_blocking(command.clone());
                }
            }),
            ..Default::default()
        }
        .into()
    }

    fn device(&self, device: &Device) -> MenuItem<Self> {
        let actions = Actions::new(std::slice::from_ref(device));
        let name = format!("{}  {}", device.port.number, device.short_name());
        let label = match status(device) {
            Some(status) => format!("{name} — {status}"),
            None => name,
        };
        let perform =
            |requests: Vec<Request>| (!requests.is_empty()).then_some(Command::Perform(requests));
        SubMenu {
            label,
            submenu: vec![
                self.item("Show Details", Some(Command::Show(device.id()))),
                MenuItem::Separator,
                self.item("Enter DFU", perform(actions.dfu)),
                self.item("Restart", perform(actions.restart)),
                MenuItem::Separator,
                self.item("Revive", perform(actions.revive.into_iter().collect())),
                self.item(
                    "Restore…",
                    actions
                        .restore
                        .is_some()
                        .then(|| Command::Restore(device.id())),
                ),
            ],
            ..Default::default()
        }
        .into()
    }

    fn board(&self, board: &Board) -> Vec<MenuItem<Self>> {
        let mut items = vec![self.item(board.title.clone(), None)];
        let view = &board.view;
        if let Some(update) = view.update.as_ref().filter(|update| update.required) {
            items.push(self.item(
                format!("Update IasisOS to {}", update.version),
                Some(Command::Perform(vec![Request::Update {
                    board: view.serial.clone(),
                    bytes: None,
                }])),
            ));
        } else if let Some(notice) = &view.notice {
            items.push(self.item(notice.clone(), None));
        }
        let devices: Vec<&Device> = board
            .devices
            .iter()
            .filter(|device| device.selectable())
            .collect();
        if devices.is_empty() && view.notice.is_none() && view.update.is_none() {
            items.push(self.item("No Devices", None));
        }
        items.extend(devices.into_iter().map(|device| self.device(device)));
        items
    }
}

/// One line on what the port is doing, or the PORT limitation.
fn status(device: &Device) -> Option<String> {
    let text = device.activity().text;
    if !text.is_empty() {
        Some(text)
    } else {
        device.port.wrong_dfu_port.then(|| "PORT".to_owned())
    }
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        crate::APP_ID.into()
    }

    fn title(&self) -> String {
        "Iasis".into()
    }

    fn icon_name(&self) -> String {
        match self.status() {
            Status::Offline => "com.hydaz.Iasis-offline-symbolic",
            Status::Ready => "com.hydaz.Iasis-symbolic",
            Status::Busy => "com.hydaz.Iasis-busy-symbolic",
            Status::Attention => "com.hydaz.Iasis-attention-symbolic",
        }
        .into()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: match self.status() {
                Status::Offline => "Iasis, no board connected",
                Status::Ready => "Iasis, ready",
                Status::Busy => "Iasis, busy",
                Status::Attention => "Iasis, needs attention",
            }
            .into(),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.commands.send_blocking(Command::Open);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items = Vec::new();
        match &self.state {
            None => items.push(self.item("Iasis Service Not Running", None)),
            Some(state) if state.boards.is_empty() => {
                items.push(self.item("No Boards Connected", None));
            }
            Some(state) => {
                for board in device::boards(state) {
                    items.extend(self.board(&board));
                    items.push(MenuItem::Separator);
                }
                if let Some(fetch) = &state.images.fetch {
                    let percent = fetch.done.saturating_mul(100) / fetch.bytes.max(1);
                    let image = state
                        .images
                        .images
                        .iter()
                        .find(|image| image.id == fetch.id)
                        .map_or_else(
                            || "Image".to_owned(),
                            |image| format!("{} {}", image.os, image.version).trim().to_owned(),
                        );
                    items.push(self.item(format!("Downloading {image} — {percent}%"), None));
                }
            }
        }
        if !matches!(items.last(), Some(MenuItem::Separator)) {
            items.push(MenuItem::Separator);
        }
        items.push(self.item("Open Iasis", Some(Command::Open)));
        items.push(self.item("Preferences", Some(Command::Preferences)));
        items.push(MenuItem::Separator);
        items.push(self.item("Quit", Some(Command::Quit)));
        items
    }
}

#[cfg(test)]
mod tests {
    use iasis_api::Progress;
    use ksni::Tray as _;

    use super::*;

    #[test]
    fn a_device_shows_its_port_name_and_status_and_the_download_its_whole_percent() {
        let mut state: State =
            serde_json::from_str(include_str!("../../../crates/api/fixtures/state.json")).unwrap();
        let port = &mut state.boards[0].ports[0];
        port.awaiting = None;
        port.stage = Some(Progress {
            name: "Sending Filesystem".into(),
            percent: 43,
            steps: Vec::new(),
            step: 0,
        });
        let tray = Tray {
            state: Some(state),
            commands: async_channel::unbounded().0,
        };
        let labels: Vec<String> = tray
            .menu()
            .into_iter()
            .filter_map(|item| match item {
                MenuItem::Standard(item) => Some(item.label),
                MenuItem::SubMenu(item) => Some(item.label),
                _ => None,
            })
            .collect();
        for label in [
            "1  MacBook Air — Restoring 43%",
            "Downloading iPadOS 26.2 — 12%",
        ] {
            assert!(labels.iter().any(|shown| shown == label), "{labels:?}");
        }
    }
}
