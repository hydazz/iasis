//! Open at Login: an XDG autostart entry in the user's configuration,
//! switched in Preferences.

use std::{fs, io, path::PathBuf};

use gtk::glib;

fn entry() -> PathBuf {
    glib::user_config_dir()
        .join("autostart")
        .join(format!("{}.desktop", crate::APP_ID))
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
