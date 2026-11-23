//! One port on one line: its number in its light's colour, the device, its
//! connection marks and one mark for what it is doing.

use std::{
    cell::{Cell, RefCell},
    f64::consts::{FRAC_PI_2, TAU},
    rc::Rc,
};

use adw::prelude::*;
use gtk::glib;
use iasis_api::{Hue, Light, UsbLink};

use crate::device::{self, Activity, Device, Mark};

pub struct PortRow {
    pub row: adw::ActionRow,
    /// Shown in selection mode.
    pub check: gtk::CheckButton,
    badge: gtk::Label,
    kind: gtk::Image,
    identifying: gtk::Spinner,
    power: gtk::Image,
    link: gtk::Image,
    sharing: gtk::Image,
    wrong_port: gtk::Label,
    mark: ActivityMark,
    shown: RefCell<Option<Device>>,
}

impl PortRow {
    pub fn new(number: u8) -> Self {
        let row = adw::ActionRow::builder()
            .title_lines(1)
            .subtitle_lines(1)
            .build();
        let check = gtk::CheckButton::builder()
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        check.add_css_class("selection-mode");
        let badge = gtk::Label::builder()
            .label(number.to_string())
            .valign(gtk::Align::Center)
            .css_classes(["port-badge"])
            .tooltip_text(format!("Port {number}"))
            .build();
        let kind = gtk::Image::builder().css_classes(["dim-label"]).build();
        let identifying = gtk::Spinner::builder()
            .spinning(true)
            .visible(false)
            .build();
        row.add_prefix(&check);
        row.add_prefix(&badge);
        row.add_prefix(&kind);
        row.add_prefix(&identifying);

        let marks = gtk::Box::builder()
            .spacing(8)
            .valign(gtk::Align::Center)
            .css_classes(["dim-label"])
            .build();
        let power = gtk::Image::new();
        let link = gtk::Image::from_icon_name("media-removable-symbolic");
        let sharing = gtk::Image::new();
        let wrong_port = gtk::Label::builder()
            .label("PORT")
            .css_classes(["caption-heading"])
            .tooltip_text(device::DFU_PORT_HELP)
            .build();
        for widget in [
            power.upcast_ref::<gtk::Widget>(),
            link.upcast_ref(),
            sharing.upcast_ref(),
            wrong_port.upcast_ref(),
        ] {
            marks.append(widget);
        }
        let mark = ActivityMark::new();
        row.add_suffix(&marks);
        row.add_suffix(&mark.widget);
        Self {
            row,
            check,
            badge,
            kind,
            identifying,
            power,
            link,
            sharing,
            wrong_port,
            mark,
            shown: RefCell::new(None),
        }
    }

    pub fn update(&self, device: &Device) {
        if self.shown.borrow().as_ref() == Some(device) {
            return;
        }
        let port = &device.port;
        let name = glib::markup_escape_text(&device.name());
        self.row.set_title(&if port.identifying {
            format!("<i>{name}</i>")
        } else {
            name.to_string()
        });
        let quiet = port.identifying || !port.occupied;
        set_class(&self.row, "quiet", quiet);
        let activity = device.activity();
        // A mode is the device's own state; an operation is the mark's to show.
        let subtitle = if activity.mark == Mark::None {
            activity.text.as_str()
        } else {
            ""
        };
        self.row.set_subtitle(&glib::markup_escape_text(subtitle));

        show_light(&self.badge, &port.light);
        match device.icon() {
            Some(icon) if !port.identifying => {
                self.kind.set_icon_name(Some(icon));
                self.kind.set_visible(true);
            }
            _ => self.kind.set_visible(false),
        }
        self.identifying.set_visible(port.identifying);

        self.power.set_visible(port.powered);
        if port.powered {
            let reading = port
                .power
                .filter(|power| power.source == iasis_api::PowerSource::Pd)
                .map_or_else(
                    || "USB power".to_owned(),
                    |power| {
                        format!(
                            "{}: {}",
                            device::power_title(&power),
                            device::power_reading(&power)
                        )
                    },
                );
            self.power.set_icon_name(Some(if port.short {
                "battery-caution-symbolic"
            } else {
                "ac-adapter-symbolic"
            }));
            set_class(&self.power, "warning", port.short);
            self.power.set_tooltip_text(Some(&if port.short {
                format!("{reading}. The device reports insufficient power.")
            } else {
                reading
            }));
        }

        self.link.set_visible(port.link.is_some());
        if let Some(link) = port.link {
            self.link.set_tooltip_text(Some(device::link_title(link)));
            set_class(&self.link, "accent", link == UsbLink::Usb3);
        }

        match &device.sharing {
            Some(sharing) if sharing.problem.is_some() => {
                self.sharing.set_icon_name(Some("dialog-warning-symbolic"));
                self.sharing.add_css_class("warning");
                self.sharing.set_tooltip_text(sharing.problem.as_deref());
                self.sharing.set_visible(true);
            }
            Some(sharing) if sharing.active => {
                self.sharing.set_icon_name(Some(device::SHARING_ICON));
                self.sharing.remove_css_class("warning");
                self.sharing
                    .set_tooltip_text(Some("Sharing internet over USB"));
                self.sharing.set_visible(true);
            }
            _ => self.sharing.set_visible(false),
        }
        self.wrong_port.set_visible(port.wrong_dfu_port);
        self.mark.show(&activity);

        self.row.set_selectable(device.selectable());
        self.row.set_activatable(device.selectable());
        self.check.set_sensitive(device.selectable());
        *self.shown.borrow_mut() = Some(device.clone());
    }
}

/// Colour a port number as its light shows: the same hue, pulsing while the
/// light pulses and dim while it is dim.
pub fn show_light(badge: &impl IsA<gtk::Widget>, light: &Light) {
    for hue in HUES {
        badge.remove_css_class(hue);
    }
    badge.add_css_class(hue_class(light.hue));
    set_class(badge, "pulse", light.pulse);
    set_class(badge, "dim", light.dim);
}

const HUES: [&str; 7] = ["off", "red", "green", "white", "blue", "amber", "purple"];

pub fn hue_class(hue: Hue) -> &'static str {
    match hue {
        Hue::Off => "off",
        Hue::Red => "red",
        Hue::Green => "green",
        Hue::White => "white",
        Hue::Blue => "blue",
        Hue::Amber => "amber",
        Hue::Purple => "purple",
    }
}

pub fn set_class(widget: &impl IsA<gtk::Widget>, class: &str, on: bool) {
    if on {
        widget.add_css_class(class);
    } else {
        widget.remove_css_class(class);
    }
}

/// What a port is doing or how it ended, as one mark. Work with a known
/// fraction is a ring drawn that far round.
pub struct ActivityMark {
    pub widget: gtk::Stack,
    spinner: gtk::Spinner,
    ring: gtk::DrawingArea,
    fraction: Rc<Cell<f64>>,
    icon: gtk::Image,
}

impl ActivityMark {
    pub fn new() -> Self {
        let widget = gtk::Stack::builder()
            .valign(gtk::Align::Center)
            .hhomogeneous(true)
            .vhomogeneous(true)
            .build();
        widget.add_named(
            &gtk::Box::new(gtk::Orientation::Horizontal, 0),
            Some("none"),
        );
        let spinner = gtk::Spinner::new();
        widget.add_named(&spinner, Some("spinner"));
        let fraction = Rc::new(Cell::new(0.0));
        let ring = gtk::DrawingArea::builder()
            .content_width(18)
            .content_height(18)
            .css_classes(["accent"])
            .build();
        let drawn = fraction.clone();
        ring.set_draw_func(move |area, cairo, width, height| {
            let color = area.color();
            let (x, y) = (f64::from(width) / 2.0, f64::from(height) / 2.0);
            let radius = x.min(y) - 1.5;
            let (red, green, blue) = (
                f64::from(color.red()),
                f64::from(color.green()),
                f64::from(color.blue()),
            );
            cairo.set_line_width(2.5);
            cairo.set_source_rgba(red, green, blue, 0.2);
            cairo.arc(x, y, radius, 0.0, TAU);
            let _ = cairo.stroke();
            cairo.set_source_rgba(red, green, blue, 1.0);
            cairo.arc(x, y, radius, -FRAC_PI_2, -FRAC_PI_2 + TAU * drawn.get());
            let _ = cairo.stroke();
        });
        widget.add_named(&ring, Some("ring"));
        let icon = gtk::Image::new();
        widget.add_named(&icon, Some("icon"));
        widget.set_visible_child_name("none");
        Self {
            widget,
            spinner,
            ring,
            fraction,
            icon,
        }
    }

    pub fn show(&self, activity: &Activity) {
        self.widget
            .set_tooltip_text((!activity.text.is_empty()).then_some(activity.text.as_str()));
        self.spinner
            .set_spinning(activity.mark == Mark::Working(None));
        let (icon, class) = match activity.mark {
            Mark::None => {
                self.widget.set_visible_child_name("none");
                return;
            }
            Mark::Working(None) => {
                self.widget.set_visible_child_name("spinner");
                return;
            }
            Mark::Working(Some(fraction)) => {
                self.fraction.set(fraction);
                self.ring.queue_draw();
                self.widget.set_visible_child_name("ring");
                return;
            }
            Mark::Done => ("object-select-symbolic", "success"),
            Mark::Failed => ("dialog-error-symbolic", "error"),
            Mark::Uncertain => ("dialog-question-symbolic", "uncertain"),
            Mark::Caution => ("dialog-warning-symbolic", "warning"),
        };
        self.icon.set_icon_name(Some(icon));
        for other in ["success", "error", "uncertain", "warning"] {
            self.icon.remove_css_class(other);
        }
        self.icon.add_css_class(class);
        self.widget.set_visible_child_name("icon");
    }
}
