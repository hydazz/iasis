//! The utility pane: the selected device's operation step by step, the
//! actions for it, and its identity, USB connection and power.

use std::cell::RefCell;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::device::{self, Device, Mark, OperationStatus, StepState};

/// What the pane shows.
#[derive(Clone, Debug, PartialEq)]
pub enum Shown {
    Nothing,
    Many(usize),
    Device(Box<Device>),
}

pub struct Inspector {
    pub widget: adw::ToolbarView,
    stack: gtk::Stack,
    empty: adw::StatusPage,
    page: adw::PreferencesPage,
    groups: RefCell<Vec<adw::PreferencesGroup>>,
    shown: RefCell<Option<Shown>>,
}

impl Inspector {
    pub fn new() -> Self {
        let empty = adw::StatusPage::builder()
            .icon_name("media-removable-symbolic")
            .title("No Selection")
            .css_classes(["compact"])
            .build();
        let page = adw::PreferencesPage::new();
        let stack = gtk::Stack::new();
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&page, Some("device"));
        let widget = adw::ToolbarView::new();
        widget.add_top_bar(&adw::HeaderBar::builder().show_title(false).build());
        widget.set_content(Some(&stack));
        Self {
            widget,
            stack,
            empty,
            page,
            groups: RefCell::default(),
            shown: RefCell::default(),
        }
    }

    pub fn show(&self, shown: Shown) {
        if self.shown.borrow().as_ref() == Some(&shown) {
            return;
        }
        for group in self.groups.take() {
            self.page.remove(&group);
        }
        match &shown {
            Shown::Nothing => {
                self.empty.set_title("No Selection");
                self.stack.set_visible_child_name("empty");
            }
            Shown::Many(count) => {
                self.empty.set_title(&format!("{count} Devices Selected"));
                self.stack.set_visible_child_name("empty");
            }
            Shown::Device(device) => {
                for group in groups(device) {
                    self.page.add(&group);
                    self.groups.borrow_mut().push(group);
                }
                self.stack.set_visible_child_name("device");
            }
        }
        *self.shown.borrow_mut() = Some(shown);
    }
}

fn groups(device: &Device) -> Vec<adw::PreferencesGroup> {
    let port = &device.port;
    let lead = adw::PreferencesGroup::builder()
        .title(glib::markup_escape_text(&device.name()))
        .description(format!("{} · port {}", device.board_title, port.number))
        .build();
    if let Some(fault) = &port.fault {
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(fault))
            .title_lines(0)
            .build();
        row.add_prefix(&icon("dialog-error-symbolic", "error"));
        lead.add(&row);
    } else if let Some(operation) = device.operation() {
        operation_rows(&lead, &operation);
    }

    let actions = adw::PreferencesGroup::new();
    for (title, icon_name, action) in [
        ("Enter DFU", device::DFU_ICON, "win.dfu"),
        ("Restart", device::RESTART_ICON, "win.restart"),
        ("Revive", device::REVIVE_ICON, "win.revive"),
        ("Restore…", device::RESTORE_ICON, "win.restore"),
    ] {
        let row = adw::ActionRow::builder()
            .title(title)
            .activatable(true)
            .action_name(action)
            .build();
        row.add_prefix(&gtk::Image::from_icon_name(icon_name));
        actions.add(&row);
    }

    let details = adw::PreferencesGroup::builder().title("Device").build();
    let property = |title: &str, value: &str| {
        let row = adw::ActionRow::builder()
            .title(title)
            .subtitle(glib::markup_escape_text(value))
            .subtitle_selectable(true)
            .css_classes(["property"])
            .build();
        details.add(&row);
    };
    if let Some(ecid) = &port.ecid {
        property("ECID", ecid);
    }
    if let Some(link) = port.link {
        property("USB Link", device::link_title(link));
    }
    if let Some(mode) = port.mode {
        property("USB Mode", device::mode_title(mode));
    }
    if port.wrong_dfu_port {
        details.add(&note(device::DFU_PORT_HELP, "dialog-warning-symbolic"));
    }
    match &device.sharing {
        Some(sharing) if sharing.problem.is_some() => details.add(&note(
            sharing.problem.as_deref().unwrap_or_default(),
            "dialog-warning-symbolic",
        )),
        Some(sharing) if sharing.active => {
            details.add(&note("Sharing internet over USB", device::SHARING_ICON));
        }
        _ => {}
    }

    if port.power.is_none() && !port.waiting_for_power && !port.short {
        return vec![lead, actions, details];
    }
    let power = adw::PreferencesGroup::builder().title("Power").build();
    if let Some(reading) = &port.power {
        let row = adw::ActionRow::builder()
            .title(device::power_title(reading))
            .subtitle(device::power_reading(reading))
            .subtitle_selectable(true)
            .css_classes(["property"])
            .build();
        power.add(&row);
    }
    if port.waiting_for_power {
        power.add(&note("Waiting for power", "dialog-warning-symbolic"));
    } else if port.short {
        power.add(&note(
            "The device reports insufficient power.",
            "battery-caution-symbolic",
        ));
    }

    vec![lead, actions, details, power]
}

fn operation_rows(group: &adw::PreferencesGroup, operation: &OperationStatus) {
    let title = adw::ActionRow::builder()
        .title(glib::markup_escape_text(&operation.title))
        .css_classes(["heading"])
        .build();
    title.add_prefix(&gtk::Image::from_icon_name(operation.icon));
    match operation.mark {
        Mark::Working(Some(fraction)) => {
            title.add_suffix(
                &gtk::Label::builder()
                    .label(format!("{:.0}%", fraction * 100.0))
                    .css_classes(["dim-label", "numeric"])
                    .build(),
            );
        }
        mark => {
            let activity = device::Activity {
                text: operation.title.clone(),
                mark,
            };
            let shown = crate::row::ActivityMark::new();
            shown.show(&activity);
            title.add_suffix(&shown.widget);
        }
    }
    group.add(&title);
    if let Mark::Working(Some(fraction)) = operation.mark {
        let bar = gtk::ProgressBar::builder()
            .fraction(fraction)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        group.add(&bar);
    }
    for step in &operation.steps {
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&step.name))
            .build();
        match step.state {
            StepState::Done => row.add_prefix(&icon("object-select-symbolic", "dim-label")),
            StepState::Active => {
                row.add_prefix(&gtk::Spinner::builder().spinning(true).build());
                if let Some(detail) = &operation.detail {
                    row.set_subtitle(&glib::markup_escape_text(detail));
                }
            }
            StepState::Pending => {
                row.add_prefix(&icon("content-loading-symbolic", "dim-label"));
                row.add_css_class("dim-label");
            }
        }
        group.add(&row);
    }
    if operation.steps.is_empty()
        && let Some(detail) = &operation.detail
    {
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(detail))
            .title_lines(0)
            .css_classes(["dim-label"])
            .build();
        group.add(&row);
    }
    if let Some(directory) = &operation.log_directory {
        let row = adw::ActionRow::builder()
            .title("Open Logs")
            .activatable(true)
            .build();
        row.add_suffix(&gtk::Image::from_icon_name("folder-open-symbolic"));
        let directory = gio::File::for_path(directory);
        row.connect_activated(move |row| {
            let window = row.root().and_downcast::<gtk::Window>();
            gtk::FileLauncher::new(Some(&directory)).launch(
                window.as_ref(),
                gio::Cancellable::NONE,
                |_| {},
            );
        });
        group.add(&row);
    }
}

fn icon(name: &str, class: &str) -> gtk::Image {
    let icon = gtk::Image::from_icon_name(name);
    icon.add_css_class(class);
    icon
}

fn note(text: &str, icon_name: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(glib::markup_escape_text(text))
        .title_lines(0)
        .build();
    row.add_prefix(&gtk::Image::from_icon_name(icon_name));
    row
}
