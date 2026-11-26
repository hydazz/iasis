//! The utility pane: the selected device's operation step by step, the
//! actions for it, and its identity, USB connection and power. Its rows are
//! made once and updated in place, so focus, scrolling and selected text
//! last through live updates.

use std::{cell::RefCell, rc::Rc};

use adw::prelude::*;
use gtk::glib;

use crate::{
    device::{self, Activity, Device, DeviceId, Mark, OperationStatus, Step, StepState},
    row::{self, ActivityMark, set_class},
};

pub struct Inspector {
    pub widget: adw::ToolbarView,
    lead: adw::PreferencesGroup,
    fault: Note,
    operation: OperationRow,
    /// What the operation is doing or how it ended, when no step says it.
    detail: adw::ActionRow,
    steps: RefCell<Vec<StepRow>>,
    /// The device and operation the step rows belong to.
    steps_of: RefCell<Option<(DeviceId, String)>>,
    reason: Note,
    get: adw::ActionRow,
    url: Rc<RefCell<String>>,
    details: adw::PreferencesGroup,
    ecid: adw::ActionRow,
    link: adw::ActionRow,
    mode: adw::ActionRow,
    wrong_port: Note,
    sharing: Note,
    power: adw::PreferencesGroup,
    reading: adw::ActionRow,
    power_note: Note,
    shown: RefCell<Option<Device>>,
}

impl Inspector {
    pub fn new() -> Self {
        let lead = adw::PreferencesGroup::new();
        let fault = Note::new(Some("error"));
        let operation = OperationRow::new();
        let detail = adw::ActionRow::builder()
            .title_lines(0)
            .css_classes(["dim-label"])
            .build();
        let reason = Note::new(Some("warning"));
        let get = adw::ActionRow::builder().activatable(true).build();
        get.add_suffix(&gtk::Image::from_icon_name("adw-external-link-symbolic"));
        let url = Rc::new(RefCell::new(String::new()));
        let opened = url.clone();
        get.connect_activated(move |row| {
            let window = row.root().and_downcast::<gtk::Window>();
            gtk::UriLauncher::new(&opened.borrow()).launch(
                window.as_ref(),
                gtk::gio::Cancellable::NONE,
                |_| {},
            );
        });
        lead.add(&fault.row);
        lead.add(&operation.row);
        lead.add(&detail);
        lead.add(&reason.row);
        lead.add(&get);

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
        let ecid = property("ECID");
        let link = property("USB Link");
        let mode = property("USB Mode");
        let wrong_port = Note::new(None);
        wrong_port.show(Some((device::DFU_PORT_HELP, "dialog-warning-symbolic")));
        let sharing = Note::new(None);
        for row in [&ecid, &link, &mode, &wrong_port.row, &sharing.row] {
            details.add(row);
        }

        let power = adw::PreferencesGroup::builder().title("Power").build();
        let reading = property("");
        let power_note = Note::new(Some("warning"));
        power.add(&reading);
        power.add(&power_note.row);

        let page = adw::PreferencesPage::new();
        for group in [&lead, &actions, &details, &power] {
            page.add(group);
        }
        let widget = adw::ToolbarView::new();
        widget.add_top_bar(&adw::HeaderBar::builder().show_title(false).build());
        widget.set_content(Some(&page));
        Self {
            widget,
            lead,
            fault,
            operation,
            detail,
            steps: RefCell::default(),
            steps_of: RefCell::default(),
            reason,
            get,
            url,
            details,
            ecid,
            link,
            mode,
            wrong_port,
            sharing,
            power,
            reading,
            power_note,
            shown: RefCell::default(),
        }
    }

    pub fn show(&self, device: &Device) {
        if self.shown.borrow().as_ref() == Some(device) {
            return;
        }
        let port = &device.port;
        self.lead
            .set_title(&glib::markup_escape_text(&device.name()));
        self.lead
            .set_description(Some(&glib::markup_escape_text(&format!(
                "{} · port {}",
                device.hub_title, port.number
            ))));
        self.fault.show(
            port.fault
                .as_deref()
                .map(|fault| (fault, "dialog-error-symbolic")),
        );
        let operation = device.operation();
        self.operation.show(operation.as_ref());
        self.show_steps(device, operation.as_ref());
        let detail = operation
            .as_ref()
            .filter(|operation| operation.steps.is_empty())
            .and_then(|operation| operation.detail.as_deref());
        self.detail.set_visible(detail.is_some());
        self.detail
            .set_title(&glib::markup_escape_text(detail.unwrap_or_default()));
        let reason = device.needs.as_ref().map(|needs| needs.reason.as_str());
        self.reason
            .show(reason.map(|reason| (reason, "dialog-warning-symbolic")));
        let get = device
            .needs
            .as_ref()
            .and_then(|needs| Some((&needs.name, needs.url.as_ref()?)));
        self.get.set_visible(get.is_some());
        if let Some((name, url)) = get {
            self.get
                .set_title(&glib::markup_escape_text(&format!("Get {name}")));
            self.url.replace(url.clone());
        }

        show_property(&self.ecid, port.ecid.as_deref());
        show_property(&self.link, port.link.map(device::link_title));
        show_property(&self.mode, port.mode.map(device::mode_title));
        self.wrong_port.row.set_visible(port.wrong_dfu_port);
        self.sharing.show(match &device.sharing {
            Some(sharing) if sharing.problem.is_some() => Some((
                sharing.problem.as_deref().unwrap_or_default(),
                "dialog-warning-symbolic",
            )),
            Some(sharing) if sharing.active => {
                Some(("Sharing internet over USB", device::SHARING_ICON))
            }
            _ => None,
        });
        self.details.set_visible(
            [
                &self.ecid,
                &self.link,
                &self.mode,
                &self.wrong_port.row,
                &self.sharing.row,
            ]
            .iter()
            .any(|row| row.is_visible()),
        );

        self.power
            .set_visible(port.power.is_some() || port.waiting_for_power || port.short);
        self.reading.set_visible(port.power.is_some());
        if let Some(reading) = &port.power {
            self.reading.set_title(device::power_title(reading));
            self.reading.set_subtitle(&device::power_reading(reading));
        }
        self.power_note.show(if port.waiting_for_power {
            Some(("Waiting for power", "dialog-warning-symbolic"))
        } else if port.short {
            Some(("The device reports insufficient power.", row::POWER))
        } else {
            None
        });
        *self.shown.borrow_mut() = Some(device.clone());
    }

    /// One row per step, updated in place while the operation lasts and made
    /// anew for the next.
    fn show_steps(&self, device: &Device, operation: Option<&OperationStatus>) {
        let owner = operation.map(|operation| (device.id(), operation.title.clone()));
        let steps = operation.map_or(&[][..], |operation| &operation.steps);
        let mut rows = self.steps.borrow_mut();
        if *self.steps_of.borrow() != owner {
            for row in rows.drain(..) {
                self.lead.remove(&row.row);
            }
            self.steps_of.replace(owner);
        }
        while rows.len() > steps.len() {
            if let Some(row) = rows.pop() {
                self.lead.remove(&row.row);
            }
        }
        if rows.len() < steps.len() {
            while rows.len() < steps.len() {
                let row = StepRow::new();
                self.lead.add(&row.row);
                rows.push(row);
            }
            // What the host lacks follows the operation.
            for row in [&self.reason.row, &self.get] {
                self.lead.remove(row);
                self.lead.add(row);
            }
        }
        let detail = operation.and_then(|operation| operation.detail.as_deref());
        for (row, step) in rows.iter().zip(steps) {
            row.show(step, detail);
        }
    }
}

/// The operation's title and mark, and its progress bar while it has a known
/// fraction, together in one row.
struct OperationRow {
    row: adw::PreferencesRow,
    icon: gtk::Image,
    title: gtk::Label,
    percent: gtk::Label,
    mark: ActivityMark,
    bar: gtk::ProgressBar,
}

impl OperationRow {
    fn new() -> Self {
        let icon = gtk::Image::new();
        let title = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .css_classes(["heading"])
            .build();
        let percent = gtk::Label::builder()
            .css_classes(["dim-label", "numeric"])
            .build();
        let mark = ActivityMark::new();
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        header.append(&icon);
        header.append(&title);
        header.append(&percent);
        header.append(&mark.widget);
        let bar = gtk::ProgressBar::new();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .margin_top(15)
            .margin_bottom(15)
            .margin_start(12)
            .margin_end(12)
            .build();
        content.append(&header);
        content.append(&bar);
        let row = adw::PreferencesRow::builder()
            .activatable(false)
            .use_markup(false)
            .child(&content)
            .build();
        Self {
            row,
            icon,
            title,
            percent,
            mark,
            bar,
        }
    }

    fn show(&self, operation: Option<&OperationStatus>) {
        self.row.set_visible(operation.is_some());
        let Some(operation) = operation else {
            return;
        };
        // A row's title only names it; the label draws it.
        self.row.set_title(&operation.title);
        self.title.set_label(&operation.title);
        self.icon.set_icon_name(Some(operation.icon));
        let fraction = match operation.mark {
            Mark::Working(fraction) => fraction,
            _ => None,
        };
        self.percent.set_visible(fraction.is_some());
        self.bar.set_visible(fraction.is_some());
        self.mark.widget.set_visible(fraction.is_none());
        match fraction {
            Some(fraction) => {
                self.percent.set_label(&format!("{:.0}%", fraction * 100.0));
                self.bar.set_fraction(fraction);
                self.bar
                    .update_property(&[gtk::accessible::Property::Label(&operation.title)]);
            }
            None => self.mark.show(&Activity {
                text: operation.title.clone(),
                mark: operation.mark,
            }),
        }
    }
}

struct StepRow {
    row: adw::ActionRow,
    icon: gtk::Image,
}

impl StepRow {
    fn new() -> Self {
        let row = adw::ActionRow::new();
        let icon = gtk::Image::new();
        row.add_prefix(&icon);
        Self { row, icon }
    }

    /// The step, with what the worker is doing within it while it is under
    /// way.
    fn show(&self, step: &Step, detail: Option<&str>) {
        self.row.set_title(&glib::markup_escape_text(&step.name));
        let active = step.state == StepState::Active;
        self.row.set_subtitle(&glib::markup_escape_text(
            detail.filter(|_| active).unwrap_or_default(),
        ));
        self.icon.set_icon_name(Some(match step.state {
            StepState::Done => "object-select-symbolic",
            StepState::Active => row::TURNING,
            StepState::Pending => "content-loading-symbolic",
        }));
        set_class(&self.icon, "turning", active);
        set_class(&self.icon, "dim-label", !active);
        set_class(&self.row, "dim-label", step.state == StepState::Pending);
    }
}

/// A line of text after an icon, shown while there is something to say.
struct Note {
    row: adw::ActionRow,
    icon: gtk::Image,
}

impl Note {
    fn new(class: Option<&str>) -> Self {
        let row = adw::ActionRow::builder().title_lines(0).build();
        let icon = gtk::Image::new();
        if let Some(class) = class {
            icon.add_css_class(class);
        }
        row.add_prefix(&icon);
        Self { row, icon }
    }

    /// Say `text` after the named icon, or hide.
    fn show(&self, said: Option<(&str, &str)>) {
        self.row.set_visible(said.is_some());
        if let Some((text, icon_name)) = said {
            self.row.set_title(&glib::markup_escape_text(text));
            self.icon.set_icon_name(Some(icon_name));
        }
    }
}

fn property(title: &str) -> adw::ActionRow {
    adw::ActionRow::builder()
        .title(title)
        .subtitle_selectable(true)
        .css_classes(["property"])
        .build()
}

fn show_property(row: &adw::ActionRow, value: Option<&str>) {
    row.set_visible(value.is_some());
    if let Some(value) = value {
        row.set_subtitle(&glib::markup_escape_text(value));
    }
}
