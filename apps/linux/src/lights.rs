//! What the hub's lights mean, port by port and for the status light.

use adw::prelude::*;
use gtk::glib;

const PORTS: &[(&str, &str, &str, &str)] = &[
    ("off", "", "Off", "Empty, turned off or identifying"),
    ("white", "", "White", "Device Iasis can't act on"),
    (
        "green",
        "",
        "Green",
        "Target Iasis can act on, or restore complete",
    ),
    ("blue", "", "Blue", "Host sees DFU or Recovery"),
    (
        "blue",
        "pulse",
        "Pulsing Blue",
        "DFU requested, waiting for the host",
    ),
    ("amber", "", "Amber", "Waiting for power"),
    ("amber", "pulse", "Pulsing Amber", "Restart requested"),
    (
        "purple",
        "",
        "Purple",
        "Restore outcome unknown. Check the host.",
    ),
    (
        "purple",
        "pulse",
        "Pulsing Purple",
        "Fetching an image or restoring",
    ),
    ("red", "", "Red", "Port fault or restore failed"),
];

const STATUS: &[(&str, &str, &str, &str)] = &[
    ("white", "", "White", "Hub running on its own"),
    ("green", "", "Green", "Host connected"),
    ("blue", "", "Blue", "Release the button to enter DFU"),
    ("amber", "", "Amber", "Release the button to restart"),
    (
        "purple",
        "",
        "Purple",
        "Release the button to request a restore",
    ),
    (
        "purple",
        "blink",
        "Blinking Purple",
        "Hold to confirm erasing, or tap to cancel",
    ),
    (
        "red",
        "",
        "Red",
        "Shared power protection or I²C fault. The target ports are off. This can flash briefly when the charging supply is unplugged.",
    ),
];

pub fn show(parent: &impl IsA<gtk::Widget>) {
    let page = adw::PreferencesPage::new();
    for (title, description, lights) in [
        (
            "Port Lights",
            Some("Unselected ports dim. Faults stay bright."),
            PORTS,
        ),
        ("Status Light", None, STATUS),
    ] {
        let group = adw::PreferencesGroup::builder().title(title).build();
        group.set_description(description);
        for &(hue, pattern, name, meaning) in lights {
            let row = adw::ActionRow::builder()
                .title(name)
                .subtitle(glib::markup_escape_text(meaning))
                .subtitle_lines(0)
                .build();
            let lens = gtk::Box::builder()
                .valign(gtk::Align::Center)
                .css_classes(["light", hue])
                .build();
            if !pattern.is_empty() {
                lens.add_css_class(pattern);
            }
            row.add_prefix(&lens);
            group.add(&row);
        }
        page.add(&group);
    }
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&page));
    adw::Dialog::builder()
        .title("Indicator Lights")
        .content_width(480)
        .content_height(640)
        .child(&toolbar)
        .build()
        .present(Some(parent));
}
