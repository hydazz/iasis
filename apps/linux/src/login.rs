//! Open at Login: an XDG autostart entry in the user's configuration, made
//! on the app's first launch and switched in Preferences.

use std::{fs, io, path::PathBuf};

use gtk::glib;

fn entry() -> PathBuf {
    glib::user_config_dir()
        .join("autostart")
        .join(format!("{}.desktop", crate::APP_ID))
}

/// Where the app's first launch is recorded.
fn launched() -> PathBuf {
    glib::user_config_dir().join("iasis").join("launched")
}

pub fn enabled() -> bool {
    entry().exists()
}

pub fn set(on: bool) -> io::Result<()> {
    let entry = entry();
    if !on {
        return match fs::remove_file(&entry) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        };
    }
    let program = crate::program()?;
    fs::create_dir_all(entry.parent().expect("the entry has a directory"))?;
    fs::write(
        &entry,
        format!(
            "[Desktop Entry]\nType=Application\nName=Iasis\nIcon={id}\nExec={program} --background\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\n",
            id = crate::APP_ID,
            program = program.display(),
        ),
    )
}

/// On the first launch, open at login from then on.
pub fn first_launch() {
    let launched = launched();
    if launched.exists() {
        return;
    }
    if let Err(error) = set(true) {
        eprintln!("Open at Login: {error}");
    }
    if let Some(parent) = launched.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(&launched, "");
}
