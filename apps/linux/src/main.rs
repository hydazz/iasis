//! Iasis for Linux desktops: every port of every hub the agent attends, the
//! actions it allows on them, and a tray icon where the desktop shows one.

mod agent;
mod device;
mod inspector;
mod lights;
#[cfg(target_os = "linux")]
mod login;
mod preferences;
mod row;
mod setup;
#[cfg(target_os = "linux")]
mod tray;
mod window;

use std::{
    cell::{Cell, RefCell},
    io,
    path::PathBuf,
    rc::Rc,
};

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::{agent::Event, window::Window};

pub const APP_ID: &str = "com.hydaz.Iasis";

/// This program's file. A package upgrade replaces it under a running app,
/// and Linux then names the running one as deleted.
pub fn program() -> io::Result<PathBuf> {
    let path = std::env::current_exe()?;
    Ok(path
        .to_str()
        .and_then(|text| text.strip_suffix(" (deleted)"))
        .map_or(path.clone(), PathBuf::from))
}

fn replaced() -> bool {
    std::env::current_exe().is_ok_and(|path| path.to_string_lossy().ends_with(" (deleted)"))
}

struct Shell {
    app: adw::Application,
    window: Rc<Window>,
    #[cfg(target_os = "linux")]
    tray: Option<ksni::blocking::Handle<tray::Tray>>,
    _hold: Option<gio::ApplicationHoldGuard>,
    /// Start the replaced program once this one has quit: in the background
    /// when its window was not showing.
    restart: Cell<Option<bool>>,
}

fn main() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.add_main_option(
        "background",
        glib::Char::from(0),
        glib::OptionFlags::NONE,
        glib::OptionArg::None,
        "Start with the tray icon alone",
        None,
    );
    let background = Rc::new(Cell::new(false));
    let shell: Rc<RefCell<Option<Rc<Shell>>>> = Rc::default();
    let asked = background.clone();
    app.connect_handle_local_options(move |_, options| {
        asked.set(options.contains("background"));
        std::ops::ControlFlow::Continue(())
    });
    let started = shell.clone();
    app.connect_startup(move |app| {
        *started.borrow_mut() = Some(Shell::start(app));
    });
    let activated = shell.clone();
    let first = Cell::new(true);
    app.connect_activate(move |app| {
        let Some(shell) = activated.borrow().clone() else {
            return;
        };
        // A login start shows the tray alone, and with no tray nothing at all.
        if first.replace(false) && background.get() {
            if !shell.has_tray() {
                app.quit();
            }
            return;
        }
        shell.window.present();
    });
    let code = app.run();
    let restart = shell
        .borrow()
        .as_ref()
        .and_then(|shell| shell.restart.get());
    if let (Some(background), Ok(program)) = (restart, program()) {
        use std::os::unix::process::CommandExt;
        let mut command = std::process::Command::new(program);
        if background {
            command.arg("--background");
        }
        eprintln!("Starting the new version: {}", command.exec());
    }
    code
}

impl Shell {
    fn start(app: &adw::Application) -> Rc<Self> {
        let provider = gtk::CssProvider::new();
        provider.load_from_string(include_str!("style.css"));
        gtk::style_context_add_provider_for_display(
            &gdk::Display::default().expect("a display"),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let window = Window::new(app);
        #[cfg(target_os = "linux")]
        let (tray, commands) = {
            let (sender, commands) = async_channel::unbounded();
            (tray::spawn(sender), commands)
        };
        #[cfg(target_os = "linux")]
        let has_tray = tray.is_some();
        #[cfg(not(target_os = "linux"))]
        let has_tray = false;
        // With a tray the app outlives its window.
        window.window.set_hide_on_close(has_tray);
        let shell = Rc::new(Self {
            app: app.clone(),
            window,
            #[cfg(target_os = "linux")]
            tray,
            _hold: has_tray.then(|| app.hold()),
            restart: Cell::new(None),
        });
        shell.add_actions();

        let events = agent::follow();
        let weak = Rc::downgrade(&shell);
        glib::spawn_future_local(async move {
            while let Ok(event) = events.recv().await {
                let Some(shell) = weak.upgrade() else { break };
                shell.show(event);
            }
        });
        #[cfg(target_os = "linux")]
        {
            let weak = Rc::downgrade(&shell);
            glib::spawn_future_local(async move {
                while let Ok(command) = commands.recv().await {
                    let Some(shell) = weak.upgrade() else { break };
                    shell.command(command);
                }
            });
        }
        shell
    }

    fn has_tray(&self) -> bool {
        #[cfg(target_os = "linux")]
        return self.tray.is_some();
        #[cfg(not(target_os = "linux"))]
        false
    }

    fn show(&self, event: Event) {
        // A package upgrade replaced this program. The agent it restarts may
        // already speak a contract this one cannot read, and a rebuild keeps
        // the version, so any word from the agent hands over to the new one.
        if replaced() {
            self.restart.set(Some(!self.window.window.is_visible()));
            self.app.quit();
            return;
        }
        #[cfg(target_os = "linux")]
        if let Some(tray) = &self.tray {
            let state = match &event {
                Event::State(state) => Some(state.as_ref().clone()),
                Event::Absent | Event::Refused(_) | Event::Unreadable => None,
            };
            tray.update(|tray| tray.state = state);
        }
        self.window.show(event);
    }

    #[cfg(target_os = "linux")]
    fn command(&self, command: tray::Command) {
        use tray::Command;
        match command {
            Command::Open => self.window.present(),
            Command::Show(id) => {
                self.window.present();
                self.window.show_device(id);
            }
            Command::Restore(id) => {
                self.window.present();
                self.window.show_device(id);
                let _ = WidgetExt::activate_action(&self.window.window, "win.restore", None);
            }
            Command::Perform(requests) => {
                let window = Rc::downgrade(&self.window);
                glib::spawn_future_local(async move {
                    for request in requests {
                        if let (Err(message), Some(window)) =
                            (agent::perform(request).await, window.upgrade())
                        {
                            window.present();
                            window.toast(&message);
                        }
                    }
                });
            }
            Command::Preferences => {
                self.window.present();
                self.window.open_preferences();
            }
            Command::Quit => self.app.quit(),
        }
    }

    fn add_actions(&self) {
        let add = |name: &str, run: Box<dyn Fn()>| {
            let action = gio::SimpleAction::new(name, None);
            action.connect_activate(move |_, _| run());
            self.app.add_action(&action);
        };
        let window = Rc::downgrade(&self.window);
        add(
            "preferences",
            Box::new(move || {
                if let Some(window) = window.upgrade() {
                    window.present();
                    window.open_preferences();
                }
            }),
        );
        let window = Rc::downgrade(&self.window);
        add(
            "lights",
            Box::new(move || {
                if let Some(window) = window.upgrade() {
                    lights::show(&window.window);
                }
            }),
        );
        let window = Rc::downgrade(&self.window);
        add(
            "about",
            Box::new(move || {
                if let Some(window) = window.upgrade() {
                    adw::AboutDialog::builder()
                        .application_name("Iasis")
                        .application_icon(APP_ID)
                        .version(env!("CARGO_PKG_VERSION"))
                        .developer_name("hydazz")
                        .website("https://github.com/hydazz/iasis")
                        .issue_url("https://github.com/hydazz/iasis/issues")
                        .license_type(gtk::License::Apache20)
                        .build()
                        .present(Some(&window.window));
                }
            }),
        );
        let app = self.app.downgrade();
        add(
            "quit",
            Box::new(move || {
                if let Some(app) = app.upgrade() {
                    app.quit();
                }
            }),
        );
        for (action, keys) in [
            ("app.quit", ["<Control>q"].as_slice()),
            ("app.preferences", &["<Control>comma"]),
            ("window.close", &["<Control>w"]),
            ("win.dfu", &["<Control><Shift>d"]),
            ("win.restart", &["<Control><Shift>r"]),
            ("win.restore", &["<Control>r"]),
        ] {
            self.app.set_accels_for_action(action, keys);
        }
    }
}
