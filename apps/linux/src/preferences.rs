//! Preferences: USB Internet Sharing, the supervision identity and opening at
//! login, each hub's name, lights and IasisOS, and the restore images.

use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::{Rc, Weak},
    time::Duration,
};

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use iasis_api::{
    CatalogModel, Fetch, HubView, Image, Images, Release, Reply, Request, State, Supervision,
};

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
    supervision_row: adw::ActionRow,
    export: gio::SimpleAction,
    supervision: RefCell<Supervision>,
    /// Set while the state is shown, so the switches send nothing back.
    showing: Cell<bool>,
    hubs: adw::PreferencesPage,
    no_hubs: adw::PreferencesGroup,
    settings: RefCell<Vec<HubSettings>>,
    images: adw::PreferencesGroup,
    import_row: adw::ActionRow,
    import_mark: ActivityMark,
    no_images: adw::ActionRow,
    image_rows: RefCell<Vec<ImageRow>>,
    importing: RefCell<Option<Importing>>,
    store: RefCell<Images>,
}

/// One image's row, kept while the store lists the image and updated in
/// place. A held image's actions are in its menu; one still to download shows
/// its progress and a stop button.
struct ImageRow {
    id: String,
    row: adw::ActionRow,
    mark: ActivityMark,
    stop: gtk::Button,
    menu: gtk::MenuButton,
    /// Whether a restore reads the image, as the menu was last built for.
    in_use: Rc<Cell<Option<bool>>>,
}

/// An archive on its way into the store: the bytes sent, or `None` once the
/// agent is checking it.
struct Importing {
    name: String,
    bytes: u64,
    sent: Option<u64>,
}

struct HubSettings {
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
        let supervision_group = adw::PreferencesGroup::builder()
            .title("Supervision Identity")
            .description("Supervised iPhones and iPads whose enrollment profile lists this certificate as a supervising host pair with this computer without asking for Trust.")
            .build();
        let identity = gio::Menu::new();
        identity.append(Some("Export…"), Some("prefs.export-identity"));
        identity.append(Some("Import…"), Some("prefs.import-identity"));
        let fingerprint_section = gio::Menu::new();
        fingerprint_section.append(Some("Copy Fingerprint"), Some("prefs.copy-fingerprint"));
        identity.append_section(None, &fingerprint_section);
        let supervision_row = adw::ActionRow::builder().subtitle_lines(1).build();
        supervision_row.add_suffix(
            &gtk::MenuButton::builder()
                .icon_name("view-more-symbolic")
                .menu_model(&identity)
                .css_classes(["flat"])
                .tooltip_text("Export or Import the Identity")
                .valign(gtk::Align::Center)
                .build(),
        );
        supervision_group.add(&supervision_row);
        general.add(&supervision_group);
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

        let hubs = adw::PreferencesPage::builder()
            .title("Hubs")
            .icon_name("media-removable-symbolic")
            .build();
        let no_hubs = adw::PreferencesGroup::builder()
            .description("No hubs connected.")
            .build();
        hubs.add(&no_hubs);
        dialog.add(&hubs);

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
        let import_row = adw::ActionRow::builder().visible(false).build();
        import_row.add_prefix(&gtk::Image::from_icon_name("package-x-generic-symbolic"));
        let import_mark = ActivityMark::new();
        import_row.add_suffix(&import_mark.widget);
        images.add(&import_row);
        let no_images = adw::ActionRow::builder()
            .title("No Images")
            .css_classes(["dim-label"])
            .build();
        images.add(&no_images);
        images_page.add(&images);
        dialog.add(&images_page);

        let this = Rc::new(Self {
            dialog,
            window: Rc::downgrade(window),
            sharing,
            supervision_row,
            export: gio::SimpleAction::new("export-identity", None),
            supervision: RefCell::default(),
            showing: Cell::new(false),
            hubs,
            no_hubs,
            settings: RefCell::default(),
            images,
            import_row,
            import_mark,
            no_images,
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
        let remove_image = gio::SimpleAction::new("remove-image", Some(glib::VariantTy::STRING));
        let weak = Rc::downgrade(&this);
        remove_image.connect_activate(move |_, image| {
            if let Some(this) = weak.upgrade()
                && let Some(image) = image.and_then(String::from_variant)
            {
                this.perform(Request::Remove { image });
            }
        });
        actions.add_action(&remove_image);
        // The Remove Image item of an image a restore is reading.
        let held_image = gio::SimpleAction::new("remove-held-image", Some(glib::VariantTy::STRING));
        held_image.set_enabled(false);
        actions.add_action(&held_image);
        let show_image = gio::SimpleAction::new("show-image", Some(glib::VariantTy::STRING));
        let weak = Rc::downgrade(&this);
        show_image.connect_activate(move |_, image| {
            if let Some(this) = weak.upgrade()
                && let Some(image) = image.and_then(String::from_variant)
            {
                this.show_in_folder(&image);
            }
        });
        actions.add_action(&show_image);
        let image_info = gio::SimpleAction::new("image-info", Some(glib::VariantTy::STRING));
        let weak = Rc::downgrade(&this);
        image_info.connect_activate(move |_, image| {
            if let Some(this) = weak.upgrade()
                && let Some(image) = image.and_then(String::from_variant)
            {
                this.show_info(&image);
            }
        });
        actions.add_action(&image_info);
        let weak = Rc::downgrade(&this);
        this.export.connect_activate(move |_, _| {
            if let Some(this) = weak.upgrade() {
                this.export_identity();
            }
        });
        actions.add_action(&this.export);
        let import_identity = gio::SimpleAction::new("import-identity", None);
        let weak = Rc::downgrade(&this);
        import_identity.connect_activate(move |_, _| {
            if let Some(this) = weak.upgrade() {
                this.import_identity();
            }
        });
        actions.add_action(&import_identity);
        let copy_fingerprint = gio::SimpleAction::new("copy-fingerprint", None);
        let weak = Rc::downgrade(&this);
        copy_fingerprint.connect_activate(move |_, _| {
            if let Some(this) = weak.upgrade()
                && let Some(fingerprint) = fingerprint(&this.supervision.borrow())
            {
                this.dialog.clipboard().set_text(&fingerprint);
            }
        });
        actions.add_action(&copy_fingerprint);
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
        let supervision = &state.supervision;
        let fingerprint = fingerprint(supervision);
        self.supervision_row
            .set_title(&glib::markup_escape_text(match &supervision.certificate {
                Some(_) => supervision.name.as_deref().unwrap_or("Unnamed"),
                None => supervision.problem.as_deref().unwrap_or("No Identity"),
            }));
        self.supervision_row
            .set_subtitle(fingerprint.as_deref().unwrap_or_default());
        self.supervision_row.set_tooltip_text(
            fingerprint
                .map(|fingerprint| format!("SHA-256 {fingerprint}"))
                .as_deref(),
        );
        self.export.set_enabled(supervision.certificate.is_some());
        *self.supervision.borrow_mut() = supervision.clone();
        self.show_hubs(&state.hubs);
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

    fn show_hubs(self: &Rc<Self>, hubs: &[HubView]) {
        let readable: Vec<&HubView> = hubs.iter().filter(|hub| !hub.ports.is_empty()).collect();
        let mut settings = self.settings.borrow_mut();
        if settings
            .iter()
            .map(|settings| settings.serial.as_str())
            .ne(readable.iter().map(|hub| hub.serial.as_str()))
        {
            for old in settings.drain(..) {
                self.hubs.remove(&old.group);
            }
            for hub in &readable {
                let new = HubSettings::new(self, hub);
                self.hubs.add(&new.group);
                settings.push(new);
            }
        }
        self.no_hubs.set_visible(readable.is_empty());
        for (settings, hub) in settings.iter().zip(&readable) {
            settings.show(hub);
        }
    }

    fn show_images(self: &Rc<Self>) {
        let store = self.store.borrow();
        let mut rows = self.image_rows.borrow_mut();
        if rows
            .iter()
            .map(|row| row.id.as_str())
            .ne(store.images.iter().map(|image| image.id.as_str()))
        {
            for old in rows.drain(..) {
                self.images.remove(&old.row);
            }
            for image in &store.images {
                let new = ImageRow::new(&image.id);
                self.images.add(&new.row);
                rows.push(new);
            }
        }
        for (row, image) in rows.iter().zip(&store.images) {
            row.show(image, store.fetch.as_ref());
        }
        let importing = self.importing.borrow();
        if let Some(importing) = &*importing {
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
            self.import_row
                .set_title(&glib::markup_escape_text(&importing.name));
            self.import_row.set_subtitle(&text);
            self.import_mark.show(&device::Activity { text, mark });
        }
        self.import_row.set_visible(importing.is_some());
        self.no_images
            .set_visible(importing.is_none() && store.images.is_empty());
    }

    /// Open the image's folder in the file manager, with the archive selected.
    fn show_in_folder(&self, id: &str) {
        let store = self.store.borrow();
        let Some(path) = store
            .images
            .iter()
            .find(|image| image.id == id)
            .and_then(|image| image.path.clone())
        else {
            return;
        };
        let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(path)));
        let parent = self.dialog.root().and_downcast::<gtk::Window>();
        let window = self.window.clone();
        glib::spawn_future_local(async move {
            if let Err(error) = launcher
                .open_containing_folder_future(parent.as_ref())
                .await
                && let Some(window) = window.upgrade()
            {
                window.toast(&format!("Show in Folder: {error}"));
            }
        });
    }

    /// What an image's row leaves out: where it came from and every model it
    /// restores.
    fn show_info(&self, id: &str) {
        let store = self.store.borrow();
        let Some(image) = store.images.iter().find(|image| image.id == id) else {
            return;
        };
        let about = adw::PreferencesGroup::new();
        about.set_description(
            image.in_use.then_some(
                "A restore is using this image. It can be removed once the restore ends.",
            ),
        );
        let source = if image.fetched {
            "Downloaded from Apple"
        } else {
            "Added from a file"
        };
        for (title, value) in [
            ("Build", image.build.clone()),
            ("Size", device::size(image.bytes)),
            ("Source", source.to_owned()),
        ] {
            about.add(
                &adw::ActionRow::builder()
                    .title(title)
                    .subtitle(glib::markup_escape_text(&value))
                    .subtitle_selectable(true)
                    .css_classes(["property"])
                    .build(),
            );
        }
        let models = adw::PreferencesGroup::builder().title("Models").build();
        for model in &image.models {
            models.add(
                &adw::ActionRow::builder()
                    .title(glib::markup_escape_text(model))
                    .title_selectable(true)
                    .build(),
            );
        }
        let page = adw::PreferencesPage::new();
        page.add(&about);
        page.add(&models);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&page));
        adw::Dialog::builder()
            .title(image_title(image))
            .content_width(360)
            .content_height(480)
            .child(&toolbar)
            .build()
            .present(Some(&self.dialog));
    }

    /// Save the identity's certificate in DER, for an enrollment profile.
    fn save_certificate(self: &Rc<Self>) {
        let supervision = self.supervision.borrow().clone();
        let Some(certificate) = supervision.certificate else {
            return;
        };
        let chooser = gtk::FileDialog::builder()
            .title("Export Certificate")
            .initial_name(format!(
                "{}.cer",
                supervision.name.as_deref().unwrap_or("Iasis")
            ))
            .modal(true)
            .build();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(this) = weak.upgrade() else { return };
            let parent = this.dialog.root().and_downcast::<gtk::Window>();
            let Ok(file) = chooser.save_future(parent.as_ref()).await else {
                return;
            };
            let Some(path) = file.path() else { return };
            if let Err(error) = std::fs::write(&path, glib::base64_decode(&certificate))
                && let Some(window) = this.window.upgrade()
            {
                window.toast(&format!("Export Certificate: {error}"));
            }
        });
    }

    /// Save the certificate alone, for an enrollment profile, or with the
    /// private key in a PKCS #12 archive protected by a password the user
    /// gives twice, to back the identity up or move it.
    fn export_identity(self: &Rc<Self>) {
        let name = self.supervision.borrow().name.clone();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(this) = weak.upgrade() else { return };
            let password = match ask_export(&this.dialog).await {
                None => return,
                Some(Export::Certificate) => return this.save_certificate(),
                Some(Export::Identity(password)) => password,
            };
            let archive = match agent::ask(Request::ExportSupervision { password }).await {
                Ok(Reply::Supervision(archive)) => glib::base64_decode(&archive),
                Ok(Reply::Error(message)) | Err(message) => return this.report(&message),
                Ok(_) => return,
            };
            let chooser = gtk::FileDialog::builder()
                .title("Export Supervision Identity")
                .initial_name(format!("{}.p12", name.as_deref().unwrap_or("Iasis")))
                .modal(true)
                .build();
            let parent = this.dialog.root().and_downcast::<gtk::Window>();
            let Ok(file) = chooser.save_future(parent.as_ref()).await else {
                return;
            };
            if let Some(path) = file.path()
                && let Err(error) = std::fs::write(&path, archive)
            {
                this.report(&format!("Export Supervision Identity: {error}"));
            }
        });
    }

    /// Replace the identity with the one in a PKCS #12 archive.
    fn import_identity(self: &Rc<Self>) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("PKCS #12 Archives"));
        filter.add_suffix("p12");
        filter.add_suffix("pfx");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let chooser = gtk::FileDialog::builder()
            .title("Import Supervision Identity")
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
            let Some(archive) = file.path().and_then(|path| std::fs::read(path).ok()) else {
                return this.report("Import Supervision Identity: the file could not be read");
            };
            let Some(password) = ask_password(
                &this.dialog,
                "Import Supervision Identity",
                "This replaces the computer's identity. Devices that list the current certificate ask for Trust until they enroll with the new one.",
                "_Replace",
            )
            .await
            else {
                return;
            };
            if let Err(message) = agent::perform(Request::ImportSupervision {
                pkcs12: glib::base64_encode(&archive).to_string(),
                password,
            })
            .await
            {
                this.report(&message);
            }
        });
    }

    fn report(&self, message: &str) {
        if let Some(window) = self.window.upgrade() {
            window.toast(message);
        }
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

impl ImageRow {
    fn new(id: &str) -> Self {
        let row = adw::ActionRow::new();
        let mark = ActivityMark::new();
        row.add_suffix(&mark.widget);
        let target = id.to_variant();
        let stop = gtk::Button::builder()
            .icon_name("process-stop-symbolic")
            .action_name("prefs.remove-image")
            .action_target(&target)
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        row.add_suffix(&stop);
        row.set_action_target_value(Some(&target));
        let menu = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text("Image Options")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        row.add_suffix(&menu);
        // A secondary click or a long press opens the same menu.
        let click = gtk::GestureClick::builder()
            .button(gdk::BUTTON_SECONDARY)
            .build();
        let shown = menu.clone();
        click.connect_pressed(move |_, _, _, _| {
            if shown.is_visible() {
                shown.popup();
            }
        });
        row.add_controller(click);
        let press = gtk::GestureLongPress::builder().touch_only(true).build();
        let shown = menu.clone();
        press.connect_pressed(move |_, _, _| {
            if shown.is_visible() {
                shown.popup();
            }
        });
        row.add_controller(press);
        let this = Self {
            id: id.to_owned(),
            row,
            mark,
            stop,
            menu,
            in_use: Rc::default(),
        };
        // Delete removes the image or stops its download, as its menu or
        // stop button would.
        let keys = gtk::EventControllerKey::new();
        let in_use = this.in_use.clone();
        keys.connect_key_pressed(move |keys, key, _, _| {
            if !matches!(
                key,
                gdk::Key::Delete | gdk::Key::KP_Delete | gdk::Key::BackSpace
            ) {
                return glib::Propagation::Proceed;
            }
            if in_use.get() != Some(true)
                && let Some(row) = keys.widget()
            {
                let _ = row.activate_action("prefs.remove-image", Some(&target));
            }
            glib::Propagation::Stop
        });
        this.row.add_controller(keys);
        this
    }

    /// The held image's actions; Remove Image is unavailable while a restore
    /// reads the image.
    fn actions(&self, in_use: bool) -> gio::Menu {
        let target = self.id.to_variant();
        let item = |label: &str, action: &str| {
            let item = gio::MenuItem::new(Some(label), None);
            item.set_action_and_target_value(Some(action), Some(&target));
            item
        };
        let removing = gio::Menu::new();
        removing.append_item(&item(
            "Remove Image",
            if in_use {
                "prefs.remove-held-image"
            } else {
                "prefs.remove-image"
            },
        ));
        let actions = gio::Menu::new();
        actions.append_item(&item("Show in Folder", "prefs.show-image"));
        actions.append_item(&item("Get Info", "prefs.image-info"));
        actions.append_section(None, &removing);
        actions
    }

    fn show(&self, image: &Image, fetch: Option<&Fetch>) {
        if self.in_use.replace(Some(image.in_use)) != Some(image.in_use) {
            self.menu.set_menu_model(Some(&self.actions(image.in_use)));
        }
        // Activating a held image's row shows its information.
        self.row.set_activatable(image.ready);
        self.row
            .set_action_name(image.ready.then_some("prefs.image-info"));
        let mut detail = vec![image.build.clone(), device::size(image.bytes)];
        // macOS says which devices by itself; the others restore only some.
        if image.os != "macOS" && image.models.len() <= 2 {
            detail.push(image.models.join(", "));
        }
        self.row
            .set_title(&glib::markup_escape_text(&image_title(image)));
        self.row
            .set_subtitle(&glib::markup_escape_text(&detail.join(" · ")));
        self.mark.widget.set_visible(!image.ready);
        self.stop.set_visible(!image.ready);
        self.menu.set_visible(image.ready);
        if image.ready {
            return;
        }
        let activity = match (&image.error, fetch.filter(|fetch| fetch.id == image.id)) {
            (Some(error), _) => device::Activity {
                text: format!("Not downloaded: {error}"),
                mark: Mark::Caution,
            },
            (None, Some(fetch)) => {
                let fraction = fetch.done as f64 / fetch.bytes.max(1) as f64;
                device::Activity {
                    text: format!("Downloading, {:.0}%", fraction * 100.0),
                    mark: Mark::Working(Some(fraction)),
                }
            }
            (None, None) => device::Activity {
                text: "Waiting to download".into(),
                mark: Mark::Working(Some(0.0)),
            },
        };
        self.mark.show(&activity);
        self.stop.set_tooltip_text(Some(if image.error.is_some() {
            "Remove"
        } else {
            "Stop Downloading"
        }));
    }
}

fn image_title(image: &Image) -> String {
    if image.os.is_empty() {
        image.version.clone()
    } else {
        format!("{} {}", image.os, image.version)
    }
}

impl HubSettings {
    fn new(preferences: &Rc<Preferences>, hub: &HubView) -> Self {
        let group = adw::PreferencesGroup::builder()
            .title(glib::markup_escape_text(device::display_name(hub)))
            .build();
        let name = adw::EntryRow::builder()
            .title("Name")
            .text(&hub.name)
            .show_apply_button(true)
            .build();
        let weak = Rc::downgrade(preferences);
        let serial = hub.serial.clone();
        name.connect_apply(move |row| {
            if let Some(preferences) = weak.upgrade() {
                preferences.perform(Request::Rename {
                    hub: serial.clone(),
                    name: row.text().to_string(),
                });
            }
        });
        group.add(&name);

        let brightness = gtk::Scale::with_range(gtk::Orientation::Horizontal, 10.0, 100.0, 1.0);
        brightness.set_draw_value(false);
        brightness.set_hexpand(true);
        brightness.set_valign(gtk::Align::Center);
        brightness.set_value(f64::from(hub.brightness));
        let light = adw::ActionRow::builder().title("Light Brightness").build();
        light.add_suffix(&brightness);
        brightness.set_width_request(180);
        let pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::default();
        let weak = Rc::downgrade(preferences);
        let serial = hub.serial.clone();
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
                            hub: serial,
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
            .action_name("win.update-hub")
            .action_target(&hub.serial.to_variant())
            .build();
        firmware.add_suffix(&update);
        group.add(&firmware);
        let serial_row = adw::ActionRow::builder()
            .title("Serial")
            .subtitle(&hub.serial)
            .subtitle_selectable(true)
            .css_classes(["property"])
            .build();
        group.add(&serial_row);
        Self {
            serial: hub.serial.clone(),
            group,
            name,
            brightness,
            firmware,
            update,
            pending,
        }
    }

    fn show(&self, hub: &HubView) {
        self.group
            .set_title(&glib::markup_escape_text(device::display_name(hub)));
        if !self.name.has_focus() && self.name.text() != hub.name {
            self.name.set_text(&hub.name);
        }
        if self.pending.borrow().is_none() {
            self.brightness.set_value(f64::from(hub.brightness));
        }
        self.firmware.set_subtitle(&hub.firmware);
        match &hub.update {
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

/// What an export holds: the certificate alone, or with the private key in
/// an archive under this password.
enum Export {
    Certificate,
    Identity(String),
}

/// Ask what to export and, with the private key, a password twice; `None`
/// when the user cancels.
async fn ask_export(parent: &impl IsA<gtk::Widget>) -> Option<Export> {
    let dialog = adw::AlertDialog::new(Some("Export Supervision Identity"), None);
    dialog.add_response("cancel", "_Cancel");
    dialog.add_response("export", "_Export…");
    dialog.set_response_appearance("export", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("export"));
    dialog.set_close_response("cancel");
    let certificate = gtk::CheckButton::with_label("Certificate, for an enrollment profile");
    let identity = gtk::CheckButton::with_label(
        "Certificate and private key, to back up or move the identity",
    );
    identity.set_group(Some(&certificate));
    certificate.set_active(true);
    let password = gtk::PasswordEntry::builder()
        .placeholder_text("Password")
        .show_peek_icon(true)
        .activates_default(true)
        .build();
    let again = gtk::PasswordEntry::builder()
        .placeholder_text("Verify")
        .activates_default(true)
        .build();
    let secret = gtk::Box::new(gtk::Orientation::Vertical, 6);
    secret.append(
        &gtk::Label::builder()
            .label("Anyone with the file and its password can pair with every device that lists the certificate.")
            .wrap(true)
            .xalign(0.0)
            .css_classes(["dim-label"])
            .build(),
    );
    secret.append(&password);
    secret.append(&again);
    let fields = gtk::Box::new(gtk::Orientation::Vertical, 6);
    fields.append(&certificate);
    fields.append(&identity);
    fields.append(&secret);
    dialog.set_extra_child(Some(&fields));
    let check = {
        let (dialog, identity, password, again, secret) = (
            dialog.clone(),
            identity.clone(),
            password.clone(),
            again.clone(),
            secret.clone(),
        );
        move || {
            let keyed = identity.is_active();
            secret.set_visible(keyed);
            let text = password.text();
            dialog.set_response_enabled(
                "export",
                !keyed || (!text.is_empty() && text == again.text()),
            );
        }
    };
    check();
    identity.connect_toggled({
        let check = check.clone();
        move |_| check()
    });
    password.connect_changed({
        let check = check.clone();
        move |_| check()
    });
    again.connect_changed(move |_| check());
    if dialog.choose_future(Some(parent)).await != "export" {
        return None;
    }
    Some(if identity.is_active() {
        Export::Identity(password.text().to_string())
    } else {
        Export::Certificate
    })
}

/// Ask for a password; `None` when the user cancels.
async fn ask_password(
    parent: &impl IsA<gtk::Widget>,
    heading: &str,
    body: &str,
    action: &str,
) -> Option<String> {
    let dialog = adw::AlertDialog::new(Some(heading), Some(body));
    dialog.add_response("cancel", "_Cancel");
    dialog.add_response("ok", action);
    dialog.set_response_appearance("ok", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("ok"));
    dialog.set_close_response("cancel");
    dialog.set_response_enabled("ok", false);
    let password = gtk::PasswordEntry::builder()
        .placeholder_text("Password")
        .show_peek_icon(true)
        .activates_default(true)
        .build();
    dialog.set_extra_child(Some(&password));
    password.connect_changed({
        let dialog = dialog.clone();
        move |password| dialog.set_response_enabled("ok", !password.text().is_empty())
    });
    (dialog.choose_future(Some(parent)).await == "ok").then(|| password.text().to_string())
}

/// The certificate's SHA-256 fingerprint, in colon-separated hexadecimal
/// pairs, to check which identity an enrollment profile lists.
fn fingerprint(supervision: &Supervision) -> Option<String> {
    let certificate = glib::base64_decode(supervision.certificate.as_deref()?);
    let hex = glib::compute_checksum_for_data(glib::ChecksumType::Sha256, &certificate)?;
    Some(
        hex.as_bytes()
            .chunks(2)
            .map(|pair| String::from_utf8_lossy(pair).to_uppercase())
            .collect::<Vec<_>>()
            .join(":"),
    )
}
