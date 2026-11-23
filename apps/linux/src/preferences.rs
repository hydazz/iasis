//! Preferences: USB Internet Sharing and opening at login, each board's name,
//! lights and IasisOS, and the restore images.

use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::{Rc, Weak},
    time::Duration,
};

use adw::prelude::*;
use gtk::{gio, glib};
use iasis_api::{BoardView, CatalogModel, Image, Images, Release, Reply, Request, State};

use crate::{
    agent,
    device::{self, Mark},
    row::ActivityMark,
    window::Window,
};

pub struct Preferences {
    pub dialog: adw::PreferencesDialog,
    window: Weak<Window>,
    sharing: adw::SwitchRow,
    /// Set while the state is shown, so the switches send nothing back.
    showing: Cell<bool>,
    boards: adw::PreferencesPage,
    no_boards: adw::PreferencesGroup,
    settings: RefCell<Vec<BoardSettings>>,
    images: adw::PreferencesGroup,
    image_rows: RefCell<Vec<adw::ActionRow>>,
    importing: RefCell<Option<Importing>>,
    store: RefCell<Images>,
}

/// An archive on its way into the store: the bytes sent, or `None` once the
/// agent is checking it.
struct Importing {
    name: String,
    bytes: u64,
    sent: Option<u64>,
}

struct BoardSettings {
    serial: String,
    group: adw::PreferencesGroup,
    name: adw::EntryRow,
    brightness: gtk::Scale,
    firmware: adw::ActionRow,
    update: gtk::Button,
    /// A brightness waiting for the slider to settle.
    pending: Rc<RefCell<Option<glib::SourceId>>>,
}

impl Preferences {
    pub fn new(window: &Rc<Window>) -> Rc<Self> {
        let dialog = adw::PreferencesDialog::new();

        let general = adw::PreferencesPage::builder()
            .title("General")
            .icon_name("preferences-system-symbolic")
            .build();
        let sharing_group = adw::PreferencesGroup::builder()
            .title("USB Internet Sharing")
            .build();
        let sharing = adw::SwitchRow::builder()
            .title("Share Internet over USB")
            .subtitle("Targets with USB Ethernet use this computer’s internet connection")
            .build();
        sharing_group.add(&sharing);
        general.add(&sharing_group);
        #[cfg(target_os = "linux")]
        {
            let startup = adw::PreferencesGroup::builder().title("Startup").build();
            let login = adw::SwitchRow::builder()
                .title("Open at Login")
                .subtitle("Iasis starts in the tray, where the desktop shows one")
                .active(crate::login::enabled())
                .build();
            let weak = Rc::downgrade(window);
            login.connect_active_notify(move |row| {
                if let Err(error) = crate::login::set(row.is_active())
                    && let Some(window) = weak.upgrade()
                {
                    window.toast(&format!("Open at Login: {error}"));
                }
            });
            startup.add(&login);
            general.add(&startup);
        }
        dialog.add(&general);

        let boards = adw::PreferencesPage::builder()
            .title("Boards")
            .icon_name("media-removable-symbolic")
            .build();
        let no_boards = adw::PreferencesGroup::builder()
            .description("No boards connected.")
            .build();
        boards.add(&no_boards);
        dialog.add(&boards);

        let images_page = adw::PreferencesPage::builder()
            .title("Images")
            .icon_name(device::DOWNLOAD_ICON)
            .build();
        let add = gio::Menu::new();
        add.append(Some("Download…"), Some("prefs.download"));
        add.append(Some("From File…"), Some("prefs.import"));
        let images = adw::PreferencesGroup::builder()
            .title("Restore Images")
            .description("A restore uses the newest image here that Apple still signs, or downloads the newest release. Images stay until you remove them.")
            .header_suffix(
                &gtk::MenuButton::builder()
                    .icon_name("list-add-symbolic")
                    .menu_model(&add)
                    .css_classes(["flat"])
                    .tooltip_text("Add Image")
                    .valign(gtk::Align::Center)
                    .build(),
            )
            .build();
        images_page.add(&images);
        dialog.add(&images_page);

        let this = Rc::new(Self {
            dialog,
            window: Rc::downgrade(window),
            sharing,
            showing: Cell::new(false),
            boards,
            no_boards,
            settings: RefCell::default(),
            images,
            image_rows: RefCell::default(),
            importing: RefCell::default(),
            store: RefCell::default(),
        });

        let weak = Rc::downgrade(&this);
        this.sharing.connect_active_notify(move |row| {
            if let Some(this) = weak.upgrade()
                && !this.showing.get()
            {
                this.perform(Request::Sharing {
                    enabled: row.is_active(),
                });
            }
        });
        let actions = gio::SimpleActionGroup::new();
        let download = gio::SimpleAction::new("download", None);
        let weak = Rc::downgrade(&this);
        download.connect_activate(move |_, _| {
            if let Some(this) = weak.upgrade() {
                Download::open(&this);
            }
        });
        actions.add_action(&download);
        let import = gio::SimpleAction::new("import", None);
        let weak = Rc::downgrade(&this);
        import.connect_activate(move |_, _| {
            if let Some(this) = weak.upgrade() {
                this.choose_import();
            }
        });
        actions.add_action(&import);
        this.dialog.insert_action_group("prefs", Some(&actions));
        this
    }

    /// Show the agent's latest state.
    pub fn update(self: &Rc<Self>, state: &State) {
        self.showing.set(true);
        self.sharing.set_active(state.sharing.enabled);
        self.sharing.set_subtitle(&glib::markup_escape_text(
            state
                .sharing
                .problem
                .as_deref()
                .unwrap_or("Targets with USB Ethernet use this computer’s internet connection"),
        ));
        self.showing.set(false);
        self.show_boards(&state.boards);
        *self.store.borrow_mut() = state.images.clone();
        self.show_images();
    }

    fn perform(&self, request: Request) {
        let window = self.window.clone();
        glib::spawn_future_local(async move {
            if let (Err(message), Some(window)) = (agent::perform(request).await, window.upgrade())
            {
                window.toast(&message);
            }
        });
    }

    fn show_boards(self: &Rc<Self>, boards: &[BoardView]) {
        let readable: Vec<&BoardView> = boards
            .iter()
            .filter(|board| !board.ports.is_empty())
            .collect();
        let mut settings = self.settings.borrow_mut();
        if settings
            .iter()
            .map(|settings| settings.serial.as_str())
            .ne(readable.iter().map(|board| board.serial.as_str()))
        {
            for old in settings.drain(..) {
                self.boards.remove(&old.group);
            }
            for board in &readable {
                let new = BoardSettings::new(self, board);
                self.boards.add(&new.group);
                settings.push(new);
            }
        }
        self.no_boards.set_visible(readable.is_empty());
        for (settings, board) in settings.iter().zip(&readable) {
            settings.show(board);
        }
    }

    fn show_images(self: &Rc<Self>) {
        for row in self.image_rows.take() {
            self.images.remove(&row);
        }
        let store = self.store.borrow();
        let mut rows = Vec::new();
        if let Some(importing) = &*self.importing.borrow() {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&importing.name))
                .build();
            row.add_prefix(&gtk::Image::from_icon_name("package-x-generic-symbolic"));
            let (text, mark) = match importing.sent {
                Some(sent) => {
                    let fraction = sent as f64 / importing.bytes.max(1) as f64;
                    (
                        format!("Adding, {:.0}%", fraction * 100.0),
                        Mark::Working(Some(fraction)),
                    )
                }
                None => ("Checking".to_owned(), Mark::Working(None)),
            };
            row.set_subtitle(&text);
            let shown = ActivityMark::new();
            shown.show(&device::Activity { text, mark });
            row.add_suffix(&shown.widget);
            rows.push(row);
        }
        for image in &store.images {
            rows.push(self.image_row(image, store.fetch.as_ref()));
        }
        if rows.is_empty() {
            let row = adw::ActionRow::builder()
                .title("No Images")
                .css_classes(["dim-label"])
                .build();
            rows.push(row);
        }
        for row in &rows {
            self.images.add(row);
        }
        *self.image_rows.borrow_mut() = rows;
    }

    fn image_row(
        self: &Rc<Self>,
        image: &Image,
        fetch: Option<&iasis_api::Fetch>,
    ) -> adw::ActionRow {
        let mut detail = vec![image.build.clone(), device::size(image.bytes)];
        if image.os != "macOS" && image.models.len() <= 2 {
            detail.push(image.models.join(", "));
        }
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&format!(
                "{} {}",
                image.os, image.version
            )))
            .subtitle(glib::markup_escape_text(&detail.join(" · ")))
            .build();
        let activity = match (
            image.ready,
            &image.error,
            fetch.filter(|fetch| fetch.id == image.id),
        ) {
            (true, _, _) => device::Activity {
                text: "Downloaded".into(),
                mark: Mark::Done,
            },
            (false, Some(error), _) => device::Activity {
                text: format!("Not downloaded: {error}"),
                mark: Mark::Caution,
            },
            (false, None, Some(fetch)) => {
                let fraction = fetch.done as f64 / fetch.bytes.max(1) as f64;
                device::Activity {
                    text: format!("Downloading, {:.0}%", fraction * 100.0),
                    mark: Mark::Working(Some(fraction)),
                }
            }
            (false, None, None) => device::Activity {
                text: "Waiting to download".into(),
                mark: Mark::Working(Some(0.0)),
            },
        };
        let shown = ActivityMark::new();
        shown.show(&activity);
        row.add_suffix(&shown.widget);
        let remove = gtk::Button::builder()
            .icon_name(if image.ready {
                "user-trash-symbolic"
            } else {
                "process-stop-symbolic"
            })
            .tooltip_text(if image.ready {
                "Remove This Image"
            } else {
                "Stop Downloading"
            })
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        let weak = Rc::downgrade(self);
        let id = image.id.clone();
        remove.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.perform(Request::Remove { image: id.clone() });
            }
        });
        row.add_suffix(&remove);
        row
    }

    fn choose_import(self: &Rc<Self>) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Restore Images"));
        filter.add_suffix("ipsw");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let chooser = gtk::FileDialog::builder()
            .title("Add Restore Image")
            .filters(&filters)
            .modal(true)
            .build();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(this) = weak.upgrade() else { return };
            let parent = this.dialog.root().and_downcast::<gtk::Window>();
            let Ok(file) = chooser.open_future(parent.as_ref()).await else {
                return;
            };
            if let Some(path) = file.path() {
                this.import(path).await;
            }
        });
    }

    async fn import(self: &Rc<Self>, path: PathBuf) {
        let bytes = std::fs::metadata(&path).map_or(0, |metadata| metadata.len());
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        *self.importing.borrow_mut() = Some(Importing {
            name,
            bytes,
            sent: Some(0),
        });
        self.show_images();
        let (progress, sent) = async_channel::unbounded();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while let Ok(done) = sent.recv().await {
                let Some(this) = weak.upgrade() else { return };
                if let Some(importing) = &mut *this.importing.borrow_mut() {
                    importing.sent = (done < importing.bytes).then_some(done);
                }
                this.show_images();
            }
        });
        let result = agent::import(path, progress).await;
        self.importing.take();
        self.show_images();
        if let (Err(message), Some(window)) = (result, self.window.upgrade()) {
            window.toast(&message);
        }
    }
}

impl BoardSettings {
    fn new(preferences: &Rc<Preferences>, board: &BoardView) -> Self {
        let group = adw::PreferencesGroup::builder()
            .title(glib::markup_escape_text(device::display_name(board)))
            .build();
        let name = adw::EntryRow::builder()
            .title("Name")
            .text(&board.name)
            .show_apply_button(true)
            .build();
        let weak = Rc::downgrade(preferences);
        let serial = board.serial.clone();
        name.connect_apply(move |row| {
            if let Some(preferences) = weak.upgrade() {
                preferences.perform(Request::Rename {
                    board: serial.clone(),
                    name: row.text().to_string(),
                });
            }
        });
        group.add(&name);

        let brightness = gtk::Scale::with_range(gtk::Orientation::Horizontal, 10.0, 100.0, 1.0);
        brightness.set_draw_value(false);
        brightness.set_hexpand(true);
        brightness.set_valign(gtk::Align::Center);
        brightness.set_value(f64::from(board.brightness));
        let light = adw::ActionRow::builder().title("Light Brightness").build();
        light.add_suffix(&brightness);
        brightness.set_width_request(180);
        let pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::default();
        let weak = Rc::downgrade(preferences);
        let serial = board.serial.clone();
        let waiting = pending.clone();
        brightness.connect_change_value(move |scale, _, value| {
            if let Some(previous) = waiting.take() {
                previous.remove();
            }
            let percent = value.round().clamp(10.0, 100.0) as u8;
            let weak = weak.clone();
            let serial = serial.clone();
            let done = waiting.clone();
            *waiting.borrow_mut() = Some(glib::timeout_add_local_once(
                Duration::from_millis(400),
                move || {
                    done.take();
                    if let Some(preferences) = weak.upgrade() {
                        preferences.perform(Request::Brightness {
                            board: serial,
                            percent,
                        });
                    }
                },
            ));
            scale.set_value(f64::from(percent));
            glib::Propagation::Stop
        });
        group.add(&light);

        let firmware = adw::ActionRow::builder()
            .title("IasisOS")
            .css_classes(["property"])
            .build();
        let update = gtk::Button::builder()
            .valign(gtk::Align::Center)
            .action_name("win.update-board")
            .action_target(&board.serial.to_variant())
            .build();
        firmware.add_suffix(&update);
        group.add(&firmware);
        let serial_row = adw::ActionRow::builder()
            .title("Serial")
            .subtitle(&board.serial)
            .subtitle_selectable(true)
            .css_classes(["property"])
            .build();
        group.add(&serial_row);
        Self {
            serial: board.serial.clone(),
            group,
            name,
            brightness,
            firmware,
            update,
            pending,
        }
    }

    fn show(&self, board: &BoardView) {
        self.group
            .set_title(&glib::markup_escape_text(device::display_name(board)));
        self.group.set_description(
            board
                .settings_busy
                .then_some("Settings are locked while the board is busy."),
        );
        self.name.set_sensitive(!board.settings_busy);
        self.brightness.set_sensitive(!board.settings_busy);
        if !self.name.has_focus() && self.name.text() != board.name {
            self.name.set_text(&board.name);
        }
        if self.pending.borrow().is_none() {
            self.brightness.set_value(f64::from(board.brightness));
        }
        self.firmware.set_subtitle(&board.firmware);
        match &board.update {
            Some(update) => {
                self.update
                    .set_label(&format!("Update to {}", update.version));
                self.update.set_visible(true);
            }
            None => self.update.set_visible(false),
        }
    }
}

/// Choose a model, then one of the releases Apple signs for it, and have the
/// agent download it.
struct Download {
    dialog: adw::Dialog,
    navigation: adw::NavigationView,
    models: gtk::ListBox,
    catalog: RefCell<Vec<CatalogModel>>,
    search: gtk::SearchEntry,
}

impl Download {
    fn open(preferences: &Rc<Preferences>) {
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search Models")
            .hexpand(true)
            .build();
        let models = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .valign(gtk::Align::Start)
            .build();
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        column.append(&search);
        column.append(&models);
        let page = toolbar_page(
            "Download Image",
            &gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(&column)
                .build(),
        );
        let navigation = adw::NavigationView::new();
        navigation.add(&page);
        let dialog = adw::Dialog::builder()
            .title("Download Image")
            .content_width(460)
            .content_height(560)
            .child(&navigation)
            .build();
        let this = Rc::new(Self {
            dialog,
            navigation,
            models,
            catalog: RefCell::default(),
            search,
        });
        let weak = Rc::downgrade(&this);
        this.search.connect_search_changed(move |_| {
            if let Some(this) = weak.upgrade() {
                this.show_models();
            }
        });
        this.dialog.present(Some(&preferences.dialog));
        let weak = Rc::downgrade(&this);
        let window = preferences.window.clone();
        glib::spawn_future_local(async move {
            let reply = agent::ask(Request::Catalog).await;
            let Some(this) = weak.upgrade() else { return };
            match reply {
                Ok(Reply::Catalog(models)) => {
                    *this.catalog.borrow_mut() = models;
                    this.show_models();
                }
                Ok(Reply::Error(message)) | Err(message) => {
                    if let Some(window) = window.upgrade() {
                        window.toast(&message);
                    }
                    this.dialog.close();
                }
                Ok(_) => {}
            }
        });
        // The dialog holds the controller for as long as it is open.
        let held = RefCell::new(Some(this.clone()));
        this.dialog.connect_closed(move |_| {
            held.take();
        });
    }

    fn show_models(self: &Rc<Self>) {
        while let Some(row) = self.models.first_child() {
            self.models.remove(&row);
        }
        let search = self.search.text().to_lowercase();
        for model in self.catalog.borrow().iter().filter(|model| {
            search.is_empty()
                || model.name.to_lowercase().contains(&search)
                || model.identifier.to_lowercase().contains(&search)
        }) {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&model.name))
                .subtitle(&model.identifier)
                .activatable(true)
                .build();
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            let weak = Rc::downgrade(self);
            let model = model.clone();
            row.connect_activated(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.show_releases(model.clone());
                }
            });
            self.models.append(&row);
        }
    }

    fn show_releases(self: &Rc<Self>, model: CatalogModel) {
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .valign(gtk::Align::Start)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        let stack = gtk::Stack::new();
        stack.add_named(
            &adw::StatusPage::builder()
                .title("Asking Apple")
                .child(&gtk::Spinner::builder().spinning(true).build())
                .css_classes(["compact"])
                .build(),
            Some("loading"),
        );
        stack.add_named(
            &gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .child(&list)
                .build(),
            Some("releases"),
        );
        self.navigation.push(&toolbar_page(&model.name, &stack));
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let reply = agent::ask(Request::Releases {
                model: model.identifier.clone(),
            })
            .await;
            let Some(this) = weak.upgrade() else { return };
            let releases: Vec<Release> = match reply {
                Ok(Reply::Releases(releases)) => releases,
                Ok(Reply::Error(message)) | Err(message) => {
                    stack.add_named(
                        &adw::StatusPage::builder()
                            .title("No Releases")
                            .description(glib::markup_escape_text(&message))
                            .build(),
                        Some("problem"),
                    );
                    stack.set_visible_child_name("problem");
                    return;
                }
                Ok(_) => Vec::new(),
            };
            if releases.is_empty() {
                stack.add_named(
                    &adw::StatusPage::builder()
                        .title("No Signed Releases")
                        .description("Apple signs no release for this model.")
                        .build(),
                    Some("problem"),
                );
                stack.set_visible_child_name("problem");
                return;
            }
            for release in releases {
                let row = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(&device::release_title(&release)))
                    .subtitle(format!(
                        "{} · {}",
                        release.build,
                        device::size(release.bytes)
                    ))
                    .build();
                let download = gtk::Button::builder()
                    .label("Download")
                    .valign(gtk::Align::Center)
                    .build();
                let weak = Rc::downgrade(&this);
                let identifier = model.identifier.clone();
                download.connect_clicked(move |_| {
                    let Some(this) = weak.upgrade() else { return };
                    let request = Request::Download {
                        model: identifier.clone(),
                        build: Some(release.build.clone()),
                    };
                    glib::spawn_future_local(async move {
                        match agent::perform(request).await {
                            Ok(()) => {
                                this.dialog.close();
                            }
                            Err(message) => {
                                let toast = adw::Toast::new(&message);
                                if let Some(overlay) = this
                                    .dialog
                                    .root()
                                    .and_downcast::<gtk::Window>()
                                    .and_then(|window| window.child())
                                    .and_downcast::<adw::ToastOverlay>()
                                {
                                    overlay.add_toast(toast);
                                }
                            }
                        }
                    });
                });
                row.add_suffix(&download);
                list.append(&row);
            }
            stack.set_visible_child_name("releases");
        });
    }
}

fn toolbar_page(title: &str, content: &impl IsA<gtk::Widget>) -> adw::NavigationPage {
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(content));
    adw::NavigationPage::new(&toolbar, title)
}
