//! The utility pane: the selected device's operation step by step, the
//! actions for it, and its identity, USB connection and power. Its rows are
//! made once and updated in place, so focus, scrolling and selected text
//! last through live updates.

use std::{cell::RefCell, rc::Rc};

use adw::prelude::*;
use gtk::glib;
use iasis_api::StepState;

use crate::{
    device::{self, Device, Step},
    row,
};

pub struct Inspector {
    pub widget: adw::ToolbarView,
    lead: adw::PreferencesGroup,
    fault: Note,
    steps: RefCell<Vec<StepRow>>,
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
            steps: RefCell::default(),
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
        self.show_steps(device);
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

        show_property(&self.ecid, device.ecid());
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

    fn show_steps(&self, device: &Device) {
        let steps = device.operation_steps();
        let mut rows = self.steps.borrow_mut();
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
        for (row, step) in rows.iter().zip(&steps) {
            row.show(step);
        }
    }
}

struct StepRow {
    row: adw::PreferencesRow,
    icon: gtk::Image,
    title: gtk::Label,
    status: gtk::Label,
    detail: gtk::Label,
    bar: gtk::ProgressBar,
    activity: gtk::Label,
}

impl StepRow {
    fn new() -> Self {
        let icon = gtk::Image::new();
        let title = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .css_classes(["heading"])
            .build();
        let status = gtk::Label::builder()
            .css_classes(["dim-label", "caption", "numeric"])
            .build();
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        heading.append(&icon);
        heading.append(&title);
        heading.append(&status);
        let detail = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .selectable(true)
            .build();
        let bar = gtk::ProgressBar::new();
        let activity = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label", "caption"])
            .build();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .margin_top(10)
            .margin_bottom(10)
            .margin_start(12)
            .margin_end(12)
            .build();
        content.append(&heading);
        content.append(&detail);
        content.append(&bar);
        content.append(&activity);
        let row = adw::PreferencesRow::builder()
            .activatable(false)
            .use_markup(false)
            .child(&content)
            .build();
        Self {
            row,
            icon,
            title,
            status,
            detail,
            bar,
            activity,
        }
    }

    fn show(&self, step: &Step) {
        self.row.set_title(&step.title);
        self.title.set_label(&step.title);
        self.detail.set_label(&step.detail);
        self.detail.set_visible(!step.detail.is_empty());
        let status = show_state(&self.icon, step.state);
        self.bar.set_visible(step.percent.is_some());
        if let Some(percent) = step.percent {
            self.status.set_label(&format!("{percent}%"));
            self.bar.set_fraction(f64::from(percent) / 100.0);
            self.bar
                .update_property(&[gtk::accessible::Property::Label(&step.title)]);
        } else {
            self.status.set_label(status);
        }

        self.activity.set_label(&step.activity);
        self.activity.set_visible(!step.activity.is_empty());
    }
}

fn show_state(icon: &gtk::Image, state: StepState) -> &'static str {
    let (label, symbol, class) = match state {
        StepState::Pending => ("Pending", "content-loading-symbolic", "dim-label"),
        StepState::Active => ("In progress", row::TURNING, "turning"),
        StepState::Complete => ("Complete", "object-select-symbolic", "success"),
        StepState::Failed => ("Failed", "dialog-error-symbolic", "error"),
        StepState::Unconfirmed => ("Unconfirmed", "dialog-question-symbolic", "uncertain"),
        StepState::Skipped => ("Skipped", "go-next-symbolic", "dim-label"),
    };
    icon.set_icon_name(Some(symbol));
    icon.set_css_classes(&[class]);
    icon.set_tooltip_text(Some(label));
    icon.update_property(&[gtk::accessible::Property::Label(label)]);
    label
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

#[cfg(test)]
mod tests {
    use super::*;

    #[gtk::test]
    fn restore_displays_live_progress_through_reconnection_then_keeps_the_outcomes() {
        adw::init().unwrap();
        let state =
            serde_json::from_str(include_str!("../../../crates/api/fixtures/state.json")).unwrap();
        let mut device = device::hubs(&state).remove(0).devices.remove(0);
        device.port.awaiting = None;
        device.port.request = None;
        device.port.job = None;
        let inspector = Inspector::new();
        inspector.show(&device);
        let window = gtk::Window::builder().child(&inspector.widget).build();
        window.present();
        let restore = {
            let rows = inspector.steps.borrow();
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[0].status.text(), "42%");
            assert!(rows[0].bar.is_mapped());
            assert_eq!(rows[0].bar.fraction(), 0.42);
            assert!(rows[0].activity.is_mapped());
            assert_eq!(rows[0].activity.text(), "Restoring the system");
            assert!(!rows[1].bar.is_mapped());
            assert!(!rows[1].activity.is_mapped());
            rows[0].row.clone()
        };

        device.port.occupied = false;
        device.port.ecid = None;
        device.port.generation += 1;
        let progress = device.port.run.as_mut().unwrap().steps[0]
            .progress
            .as_mut()
            .unwrap();
        progress.percent = 75;
        progress.name = "Verifying firmware".into();
        inspector.show(&device);
        assert_eq!(inspector.steps.borrow()[0].row, restore);
        assert_eq!(inspector.steps.borrow()[0].status.text(), "75%");
        assert_eq!(inspector.steps.borrow()[0].bar.fraction(), 0.75);
        assert_eq!(
            inspector.steps.borrow()[0].activity.text(),
            "Verifying firmware"
        );

        device.port.occupied = true;
        let steps = &mut device.port.run.as_mut().unwrap().steps;
        steps[0].state = StepState::Complete;
        steps[1].state = StepState::Failed;
        steps[1].error = Some("Unsupported locale".into());
        steps[2].state = StepState::Skipped;
        inspector.show(&device);
        let rows = inspector.steps.borrow();
        assert_eq!(rows[0].detail.text(), "iPadOS 26.2 (24C101)");
        assert_eq!(rows[0].status.text(), "Complete");
        assert!(!rows[0].bar.is_mapped());
        assert!(!rows[0].activity.is_mapped());
        assert_eq!(rows[1].detail.text(), "en-AU, en_AU\nUnsupported locale");
        assert_eq!(rows[1].status.text(), "Failed");
        assert_eq!(rows[2].status.text(), "Skipped");
        window.close();
    }
}
