//! The main window: every port of every hub in a boxed list per hub,
//! the selected device in a utility pane beside them, and the actions the
//! agent allows.

use std::{
    cell::RefCell,
    rc::{Rc, Weak},
};

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use iasis_api::{Operation, Reply, Request, Selection, State};

use crate::{
    agent::{self, Event},
    device::{self, Actions, Device, DeviceId, Hub, RestoreTargets},
    inspector::Inspector,
    preferences::Preferences,
    row::PortRow,
    setup::SetupDialog,
};

pub struct Window {
    pub window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    split: adw::OverlaySplitView,
    banner: adw::Banner,
    stack: gtk::Stack,
    status: adw::StatusPage,
    hubs: gtk::Box,
    groups: RefCell<Vec<HubGroup>>,
    inspector: Inspector,
    /// Whether the pane shows while one device is selected.
    show_inspector: gtk::ToggleButton,
    select: gtk::ToggleButton,
    action_bar: gtk::ActionBar,
    context: gtk::PopoverMenu,
    advance_setup: gio::SimpleAction,
    menu_probe: RefCell<Option<Rc<Vec<Selection>>>>,
    setup_dialog: RefCell<Option<Rc<SetupDialog>>>,
    title: adw::WindowTitle,
    preferences: RefCell<Option<Rc<Preferences>>>,
    model: RefCell<Model>,
}

#[derive(Default)]
struct Model {
    state: Option<State>,
    hubs: Vec<Hub>,
    selected: Vec<DeviceId>,
    selecting: bool,
    actions: Actions,
}

/// One hub: a boxed list of its ports under its name, with a row for why
/// it cannot be read or what it needs.
struct HubGroup {
    serial: String,
    group: adw::PreferencesGroup,
    summary: gtk::Label,
    menu: gio::Menu,
    notice: adw::ActionRow,
    update: gtk::Button,
    list: gtk::ListBox,
    rows: RefCell<Vec<PortRow>>,
}

impl Window {
    pub fn new(app: &adw::Application) -> Rc<Self> {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Iasis")
            .default_width(880)
            .default_height(600)
            .width_request(360)
            .height_request(294)
            .build();

        let select = gtk::ToggleButton::builder()
            .icon_name("selection-mode-symbolic")
            .tooltip_text("Select")
            .build();
        let menu = gio::Menu::new();
        let section = gio::Menu::new();
        section.append(Some("_Preferences"), Some("app.preferences"));
        section.append(Some("_Indicator Lights"), Some("app.lights"));
        section.append(Some("_About Iasis"), Some("app.about"));
        menu.append_section(None, &section);
        let quit = gio::Menu::new();
        quit.append(Some("_Quit"), Some("app.quit"));
        menu.append_section(None, &quit);
        let primary = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu)
            .primary(true)
            .tooltip_text("Main Menu")
            .build();
        let show_inspector = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-right-symbolic")
            .tooltip_text("Device Details")
            .active(true)
            .sensitive(false)
            .build();
        let title = adw::WindowTitle::new("Iasis", "");
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        header.pack_start(&select);
        header.pack_end(&primary);
        header.pack_end(&show_inspector);

        let banner = adw::Banner::new("");
        let status = adw::StatusPage::new();
        let hubs = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(24)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(12)
            .margin_end(12)
            .build();
        let clamp = adw::Clamp::builder().maximum_size(720).child(&hubs).build();
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&clamp)
            .build();
        let stack = gtk::Stack::new();
        stack.add_named(&status, Some("status"));
        stack.add_named(&scrolled, Some("hubs"));

        let action_bar = gtk::ActionBar::builder().revealed(false).build();
        for (label, action, destructive) in [
            ("Enter DFU", "win.dfu", false),
            ("Restart", "win.restart", false),
        ] {
            action_bar.pack_start(&action_button(label, action, destructive));
        }
        action_bar.pack_end(&action_button("Restore…", "win.restore", true));

        let content = adw::ToolbarView::new();
        content.add_top_bar(&header);
        content.add_top_bar(&banner);
        content.set_content(Some(&stack));
        content.add_bottom_bar(&action_bar);

        let inspector = Inspector::new();
        let split = adw::OverlaySplitView::builder()
            .sidebar_position(gtk::PackType::End)
            .content(&content)
            .sidebar(&inspector.widget)
            .min_sidebar_width(280.0)
            .max_sidebar_width(360.0)
            .pin_sidebar(true)
            .show_sidebar(false)
            .build();
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&split));
        window.set_content(Some(&toasts));
        let narrow = adw::Breakpoint::new(
            adw::BreakpointCondition::parse("max-width: 720sp").expect("a valid breakpoint"),
        );
        narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
        window.add_breakpoint(narrow);

        let this = Rc::new(Self {
            window,
            toasts,
            split,
            banner,
            stack,
            status,
            hubs,
            groups: RefCell::default(),
            inspector,
            show_inspector,
            select,
            action_bar,
            context: device_menu(),
            advance_setup: gio::SimpleAction::new("advance-setup", None),
            menu_probe: RefCell::default(),
            setup_dialog: RefCell::default(),
            title,
            preferences: RefCell::default(),
            model: RefCell::default(),
        });
        this.add_actions();
        let weak = Rc::downgrade(&this);
        this.show_inspector.connect_toggled(move |_| {
            if let Some(this) = weak.upgrade() {
                this.refresh();
            }
        });
        // Dismissing the pane from over a narrow window turns it off.
        let weak = Rc::downgrade(&this);
        this.split.connect_show_sidebar_notify(move |split| {
            if let Some(this) = weak.upgrade()
                && !split.shows_sidebar()
                && this.show_inspector.is_sensitive()
            {
                this.show_inspector.set_active(false);
            }
        });
        let weak = Rc::downgrade(&this);
        this.select.connect_toggled(move |button| {
            if let Some(this) = weak.upgrade() {
                this.set_selecting(button.is_active());
            }
        });
        let weak = Rc::downgrade(&this);
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape
                && let Some(this) = weak.upgrade()
                && this.model.borrow().selecting
            {
                this.select.set_active(false);
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        this.window.add_controller(keys);
        this.show(Event::Absent);
        this
    }

    fn add_actions(self: &Rc<Self>) {
        let simple = |name: &str, run: fn(&Rc<Self>)| {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(this) = weak.upgrade() {
                    run(&this);
                }
            });
            self.window.add_action(&action);
        };
        simple("dfu", |this| {
            this.perform_all(this.model.borrow().actions.dfu.clone())
        });
        simple("restart", |this| {
            this.perform_all(this.model.borrow().actions.restart.clone());
        });
        simple("revive", |this| this.install_selected(Operation::Revive));
        simple("restore", |this| this.install_selected(Operation::Restore));

        let group = gio::SimpleActionGroup::new();
        self.advance_setup.set_enabled(false);
        let weak = Rc::downgrade(self);
        self.advance_setup.connect_activate(move |_, _| {
            if let Some(this) = weak.upgrade() {
                this.open_setup();
            }
        });
        group.add_action(&self.advance_setup);
        self.context.insert_action_group("device", Some(&group));
        let weak = Rc::downgrade(self);
        self.context.connect_closed(move |popover| {
            if let Some(this) = weak.upgrade() {
                this.menu_probe.take();
            }
            // GTK closes the popover before activating its menu item.
            let popover = popover.clone();
            glib::idle_add_local_once(move || {
                if !popover.is_visible() && popover.parent().is_some() {
                    popover.unparent();
                }
            });
        });

        let update = gio::SimpleAction::new("update-hub", Some(glib::VariantTy::STRING));
        let weak = Rc::downgrade(self);
        update.connect_activate(move |_, serial| {
            if let (Some(this), Some(hub)) = (weak.upgrade(), serial.and_then(|s| s.str())) {
                this.update_hub(hub.to_owned());
            }
        });
        self.window.add_action(&update);
    }

    /// Bring the window up, or forward.
    pub fn present(&self) {
        self.window.present();
    }

    /// Show one device and its details, as Show Details in the tray does.
    pub fn show_device(self: &Rc<Self>, id: DeviceId) {
        if self.model.borrow().selecting {
            self.select.set_active(false);
        }
        self.show_inspector.set_active(true);
        self.select_one(id);
    }

    pub fn toast(&self, message: &str) {
        self.toasts.add_toast(adw::Toast::new(message));
    }

    /// Show what the agent says.
    pub fn show(self: &Rc<Self>, event: Event) {
        match event {
            Event::State(state) => {
                let hubs = device::hubs(&state);
                if hubs.is_empty() {
                    self.status.set_icon_name(Some("media-removable-symbolic"));
                    self.status.set_title("No Hubs Connected");
                    self.status
                        .set_description(Some("Connect an Iasis hub to this computer."));
                    self.stack.set_visible_child_name("status");
                } else {
                    self.stack.set_visible_child_name("hubs");
                }
                self.show_banner(&state);
                let lost_selection = {
                    let mut model = self.model.borrow_mut();
                    let count = model.selected.len();
                    model.selected.retain(|id| {
                        hubs.iter()
                            .flat_map(|hub| &hub.devices)
                            .any(|device| device.selectable() && device.id() == *id)
                    });
                    model.selected.len() != count
                };
                if lost_selection {
                    self.context.popdown();
                }
                self.show_hubs(&hubs);
                if let Some(preferences) = &*self.preferences.borrow() {
                    preferences.update(&state);
                }
                let mut model = self.model.borrow_mut();
                model.hubs = hubs;
                model.state = Some(*state);
            }
            Event::Absent | Event::Refused(_) | Event::Unreadable => {
                let (icon, title, description) = match &event {
                    Event::Refused(reason) => (
                        "changes-prevent-symbolic",
                        "No Access to Iasis",
                        format!("{reason}."),
                    ),
                    Event::Unreadable => (
                        "dialog-warning-symbolic",
                        "Iasis Version Mismatch",
                        "Restart it with “sudo systemctl restart iasisd”.".to_owned(),
                    ),
                    _ => (
                        "computer-fail-symbolic",
                        "Iasis Is Offline",
                        "Start Iasis with “sudo systemctl start iasisd”.".to_owned(),
                    ),
                };
                self.status.set_icon_name(Some(icon));
                self.status.set_title(title);
                self.status.set_description(Some(&description));
                self.stack.set_visible_child_name("status");
                self.banner.set_revealed(false);
                self.context.popdown();
                self.show_hubs(&[]);
                let mut model = self.model.borrow_mut();
                model.hubs.clear();
                model.selected.clear();
                model.state = None;
            }
        }
        self.refresh();
    }

    pub fn state(&self) -> Option<State> {
        self.model.borrow().state.clone()
    }

    fn show_banner(&self, state: &State) {
        let message = state.images.fetch.as_ref().map(|fetch| {
            let percent = fetch.done.saturating_mul(100) / fetch.bytes.max(1);
            let waiting = state
                .hubs
                .iter()
                .flat_map(|hub| &hub.ports)
                .filter(|port| port.awaiting.as_deref() == Some(fetch.id.as_str()))
                .count();
            let waiting = match waiting {
                0 => String::new(),
                1 => ", 1 device waiting".into(),
                waiting => format!(", {waiting} devices waiting"),
            };
            format!(
                "Downloading {} ({}), {percent}%{waiting}",
                fetch.version, fetch.build
            )
        });
        if let Some(message) = &message {
            self.banner.set_title(&glib::markup_escape_text(message));
        }
        self.banner.set_revealed(message.is_some());
    }

    fn show_hubs(self: &Rc<Self>, hubs: &[Hub]) {
        let mut groups = self.groups.borrow_mut();
        groups.retain(|group| {
            let keep = hubs.iter().any(|hub| hub.view.serial == group.serial);
            if !keep {
                self.hubs.remove(&group.group);
            }
            keep
        });
        for (index, hub) in hubs.iter().enumerate() {
            let position = groups
                .iter()
                .position(|group| group.serial == hub.view.serial);
            let group = match position {
                Some(position) => groups.remove(position),
                None => HubGroup::new(self, &hub.view.serial),
            };
            group.show(self, hub);
            // Keep the agent's hub order.
            let previous = index
                .checked_sub(1)
                .and_then(|index| groups.get(index))
                .map(|previous| previous.group.clone().upcast::<gtk::Widget>());
            if group.group.parent().is_none() {
                self.hubs.append(&group.group);
            }
            self.hubs
                .reorder_child_after(&group.group, previous.as_ref());
            groups.insert(index, group);
        }
    }

    /// The devices the model holds selected, as the latest state shows them.
    fn selected(&self) -> Vec<Device> {
        let model = self.model.borrow();
        model
            .hubs
            .iter()
            .flat_map(|hub| &hub.devices)
            .filter(|device| device.selectable() && model.selected.contains(&device.id()))
            .cloned()
            .collect()
    }

    /// Bring the actions, the pane and the selection controls up to date.
    fn refresh(&self) {
        let selected = self.selected();
        let actions = Actions::new(&selected);
        let enable = |name: &str, on: bool| {
            if let Some(action) = self
                .window
                .lookup_action(name)
                .and_downcast::<gio::SimpleAction>()
            {
                action.set_enabled(on);
            }
        };
        enable("dfu", !actions.dfu.is_empty());
        enable("restart", !actions.restart.is_empty());
        enable("revive", actions.restore.is_some());
        enable("restore", actions.restore.is_some());
        let selecting = self.model.borrow().selecting;
        // Selection mode names what is selected where the app's name was.
        self.title.set_title(&match (selecting, selected.len()) {
            (false, _) => "Iasis".to_owned(),
            (true, 0) => "No Devices Selected".to_owned(),
            (true, 1) => "1 Device Selected".to_owned(),
            (true, count) => format!("{count} Devices Selected"),
        });
        // The pane shows only while one device is selected.
        let one = match selected.as_slice() {
            [device] => Some(device),
            _ => None,
        };
        if let Some(device) = one {
            self.inspector.show(device);
        }
        self.show_inspector.set_sensitive(one.is_some());
        self.split
            .set_show_sidebar(one.is_some() && self.show_inspector.is_active());
        let targets = actions.setup_targets.as_ref();
        if self
            .menu_probe
            .borrow()
            .as_deref()
            .is_some_and(|seen| Some(seen) != targets)
        {
            self.menu_probe.take();
            self.advance_setup.set_enabled(false);
        }
        if let Some(form) = self.setup_dialog.borrow().as_ref()
            && targets != Some(&form.hubs)
        {
            form.invalidate();
        }
        self.model.borrow_mut().actions = actions;
    }

    fn set_selecting(self: &Rc<Self>, selecting: bool) {
        self.context.popdown();
        {
            let mut model = self.model.borrow_mut();
            model.selecting = selecting;
            model.selected.clear();
        }
        self.action_bar.set_revealed(selecting);
        for group in self.groups.borrow().iter() {
            group.list.unselect_all();
            group.list.set_selection_mode(if selecting {
                gtk::SelectionMode::None
            } else {
                gtk::SelectionMode::Single
            });
            for row in group.rows.borrow().iter() {
                row.check.set_active(false);
                row.check.set_visible(selecting);
                row.row
                    .set_activatable_widget(selecting.then_some(&row.check));
            }
        }
        self.refresh();
    }

    /// Select one device, as a click or a context menu does outside
    /// selection mode.
    fn select_one(self: &Rc<Self>, id: DeviceId) {
        if self.model.borrow().selected == [id.clone()] {
            return;
        }
        self.context.popdown();
        self.model.borrow_mut().selected = vec![id.clone()];
        let selecting = self.model.borrow().selecting;
        for group in self.groups.borrow().iter() {
            if selecting {
                for (index, row) in group.rows.borrow().iter().enumerate() {
                    row.check
                        .set_active(group.serial == id.0 && index + 1 == usize::from(id.1));
                }
            } else if group.serial != id.0 {
                group.list.unselect_all();
            } else if let Some(row) = group.rows.borrow().get(usize::from(id.1) - 1) {
                group.list.select_row(Some(&row.row));
            }
        }
        self.refresh();
    }

    fn check(self: &Rc<Self>, id: DeviceId, on: bool) {
        if self.model.borrow().selected.contains(&id) == on {
            return;
        }
        self.context.popdown();
        {
            let mut model = self.model.borrow_mut();
            model.selected.retain(|selected| *selected != id);
            if on {
                model.selected.push(id);
            }
        }
        self.refresh();
    }

    fn open_device_menu(self: &Rc<Self>, row: &adw::ActionRow, id: &DeviceId, x: f64, y: f64) {
        if !self.model.borrow().selected.contains(id) {
            self.select_one(id.clone());
        }
        let model = self.model.borrow();
        let actions = &model.actions;
        if actions.dfu.is_empty()
            && actions.restart.is_empty()
            && actions.restore.is_none()
            && actions.setup_targets.is_none()
        {
            return;
        }
        drop(model);
        self.context.popdown();
        if self.context.parent().is_some() {
            self.context.unparent();
        }
        self.context.set_parent(row);
        self.context
            .set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        self.advance_setup.set_enabled(false);
        self.context.popup();
        let Some(hubs) = self.model.borrow().actions.setup_targets.clone() else {
            return;
        };
        let seen = Rc::new(hubs);
        self.menu_probe.replace(Some(seen.clone()));
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let check = Request::AdvanceSetup {
                hubs: (*seen).clone(),
                settings: None,
                dep: false,
                check_only: true,
            };
            let eligible = matches!(agent::ask(check).await, Ok(Reply::Ok));
            if let Some(this) = weak.upgrade()
                && this
                    .menu_probe
                    .borrow()
                    .as_ref()
                    .is_some_and(|current| Rc::ptr_eq(current, &seen))
            {
                this.advance_setup.set_enabled(eligible);
            }
        });
    }

    fn open_setup(self: &Rc<Self>) {
        if self.setup_dialog.borrow().is_some() {
            return;
        }
        let model = self.model.borrow();
        let (Some(hubs), Some(state)) = (model.actions.setup_targets.clone(), &model.state) else {
            return;
        };
        let subject = match self.selected().as_slice() {
            [device] => format!(
                "{} · port {} · {}",
                device.hub_title,
                device.port.number,
                device.short_name()
            ),
            devices => format!("{} selected devices", devices.len()),
        };
        let form = SetupDialog::new(hubs, &state.post_restore, &subject);
        drop(model);
        let weak = Rc::downgrade(self);
        form.dialog.connect_response(None, move |_, response| {
            if let Some(this) = weak.upgrade()
                && let Some(form) = this.setup_dialog.take()
                && response == "apply"
                && this.model.borrow().actions.setup_targets.as_ref() == Some(&form.hubs)
                && let Some(request) = form.approved_request()
            {
                this.perform_all([request]);
            }
        });
        self.setup_dialog.replace(Some(form.clone()));
        form.present(&self.window);
    }

    /// Send each request the actions make, and say why any was refused.
    fn perform_all(self: &Rc<Self>, requests: impl IntoIterator<Item = Request> + 'static) {
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            for request in requests {
                let result = agent::perform(request).await;
                if let (Err(message), Some(this)) = (result, weak.upgrade()) {
                    this.toast(&message);
                }
            }
        });
    }

    fn update_hub(self: &Rc<Self>, serial: String) {
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = agent::perform(Request::Update {
                hub: serial,
                bytes: None,
            })
            .await;
            if let (Err(message), Some(this)) = (result, weak.upgrade()) {
                this.toast(&message);
            }
        });
    }

    fn install_selected(self: &Rc<Self>, operation: Operation) {
        if let Some(targets) = self.model.borrow().actions.restore.clone() {
            self.install(operation, targets);
        }
    }

    pub fn install(self: &Rc<Self>, operation: Operation, targets: RestoreTargets) {
        let this = self.clone();
        glib::spawn_future_local(async move {
            let warning = targets.power_warning(&this.model.borrow().hubs);
            if let Some(warning) = warning {
                this.present();
                let dialog = adw::AlertDialog::new(Some("Limited Hub Power"), Some(&warning));
                dialog.add_response("cancel", "_Cancel");
                dialog.add_response("continue", "_Continue");
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");
                if dialog.choose_future(Some(&this.window)).await != "continue" {
                    return;
                }
            }
            if !targets.still_available(&this.model.borrow().hubs) {
                this.present();
                this.toast("The selected devices changed. Select them again.");
                return;
            }
            let request = match operation {
                Operation::Revive => Request::Revive {
                    hubs: targets.hubs,
                    latest: false,
                },
                Operation::Restore => {
                    let Some(request) = this.confirm_erase(targets).await else {
                        return;
                    };
                    request
                }
            };
            if let Err(message) = agent::perform(request).await {
                this.present();
                this.toast(&message);
            }
        });
    }

    async fn confirm_erase(&self, mut targets: RestoreTargets) -> Option<Request> {
        match agent::ask(Request::Plan {
            hubs: targets.hubs.clone(),
        })
        .await
        {
            Ok(Reply::Plan(planned)) => targets.planned = planned,
            Ok(Reply::Error(message)) | Err(message) => {
                self.toast(&message);
                return None;
            }
            Ok(_) => {
                self.toast("The service returned no restore plan.");
                return None;
            }
        }
        let dialog = adw::AlertDialog::new(Some(&targets.heading()), Some(&targets.body()));
        dialog.add_response("cancel", "_Cancel");
        let choices = targets.choices();
        for (index, choice) in choices.iter().enumerate() {
            let id = index.to_string();
            dialog.add_response(&id, &choice.label);
            dialog.set_response_appearance(&id, adw::ResponseAppearance::Destructive);
        }
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let response = dialog.choose_future(Some(&self.window)).await;
        let choice = choices.into_iter().nth(response.parse::<usize>().ok()?)?;
        Some(choice.request)
    }

    pub fn open_preferences(self: &Rc<Self>) {
        let preferences = Preferences::new(self);
        if let Some(state) = self.state() {
            preferences.update(&state);
        }
        let weak = Rc::downgrade(self);
        preferences.dialog.connect_closed(move |_| {
            if let Some(this) = weak.upgrade() {
                this.preferences.take();
            }
        });
        preferences.dialog.present(Some(&self.window));
        *self.preferences.borrow_mut() = Some(preferences);
    }
}

fn action_button(label: &str, action: &str, destructive: bool) -> gtk::Button {
    let button = gtk::Button::builder()
        .label(label)
        .action_name(action)
        .build();
    if destructive {
        button.add_css_class("destructive-action");
    }
    button
}

impl HubGroup {
    fn new(window: &Rc<Window>, serial: &str) -> Self {
        let summary = gtk::Label::builder()
            .css_classes(["dim-label", "caption", "numeric"])
            .valign(gtk::Align::Center)
            .build();
        let menu = gio::Menu::new();
        let menu_button = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .menu_model(&menu)
            .css_classes(["flat"])
            .valign(gtk::Align::Center)
            .tooltip_text("Hub")
            .build();
        let suffix = gtk::Box::builder().spacing(12).build();
        suffix.append(&summary);
        suffix.append(&menu_button);
        let group = adw::PreferencesGroup::builder()
            .header_suffix(&suffix)
            .build();
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(["boxed-list"])
            .build();
        let notice = adw::ActionRow::builder()
            .title_lines(0)
            .selectable(false)
            .activatable(false)
            .visible(false)
            .build();
        notice.add_prefix(&gtk::Image::from_icon_name("dialog-warning-symbolic"));
        let update = gtk::Button::builder()
            .label("Update")
            .valign(gtk::Align::Center)
            .css_classes(["suggested-action"])
            .action_name("win.update-hub")
            .action_target(&serial.to_variant())
            .build();
        notice.add_suffix(&update);
        list.append(&notice);
        group.add(&list);

        let weak: Weak<Window> = Rc::downgrade(window);
        let serial_owned = serial.to_owned();
        list.connect_row_selected(move |_, row| {
            let (Some(window), Some(row)) = (weak.upgrade(), row) else {
                return;
            };
            if window.model.borrow().selecting {
                return;
            }
            let groups = window.groups.borrow();
            let number =
                groups
                    .iter()
                    .find(|group| group.serial == serial_owned)
                    .and_then(|group| {
                        group.rows.borrow().iter().position(|candidate| {
                            candidate.row.upcast_ref::<gtk::ListBoxRow>() == row
                        })
                    });
            drop(groups);
            if let Some(index) = number {
                window.select_one((serial_owned.clone(), index as u8 + 1));
            }
        });
        Self {
            serial: serial.to_owned(),
            group,
            summary,
            menu,
            notice,
            update,
            list,
            rows: RefCell::default(),
        }
    }

    fn show(&self, window: &Rc<Window>, hub: &Hub) {
        let view = &hub.view;
        self.group.set_title(&glib::markup_escape_text(&hub.title));
        self.summary.set_label(&match view.pool {
            Some(pool) => format!(
                "{} W of {} W",
                device::whole_watts(pool.allocated_mw),
                device::whole_watts(pool.budget_mw)
            ),
            None => String::new(),
        });
        self.summary.set_tooltip_text(
            view.pool
                .map(|_| "Power reserved for all ports, including voltage tolerance"),
        );
        self.menu.remove_all();
        if let Some(update) = &view.update {
            let item =
                gio::MenuItem::new(Some(&format!("Update IasisOS to {}", update.version)), None);
            item.set_action_and_target_value(
                Some("win.update-hub"),
                Some(&view.serial.to_variant()),
            );
            self.menu.append_item(&item);
        }
        self.menu
            .append(Some("Hub Preferences"), Some("app.preferences"));

        let required = view.update.as_ref().filter(|update| update.required);
        let notice = match (&view.notice, required) {
            (_, Some(update)) => Some(format!(
                "IasisOS {} is from another build. Update the hub to {} to use it.",
                view.firmware, update.version
            )),
            (Some(notice), None) => Some(notice.clone()),
            (None, None) => None,
        };
        self.notice.set_visible(notice.is_some());
        self.notice.set_title(&glib::markup_escape_text(
            notice.as_deref().unwrap_or_default(),
        ));
        self.update.set_visible(required.is_some());

        let mut rows = self.rows.borrow_mut();
        while rows.len() < hub.devices.len() {
            let row = PortRow::new(rows.len() as u8 + 1);
            let selecting = window.model.borrow().selecting;
            row.check.set_visible(selecting);
            if selecting {
                row.row.set_activatable_widget(Some(&row.check));
            }
            let weak = Rc::downgrade(window);
            let id = (self.serial.clone(), rows.len() as u8 + 1);
            let checked = id.clone();
            row.check.connect_toggled(move |check| {
                if let Some(window) = weak.upgrade() {
                    window.check(checked.clone(), check.is_active());
                }
            });
            connect_context_menu(window, &row.row, id);
            self.list.append(&row.row);
            rows.push(row);
        }
        while rows.len() > hub.devices.len() {
            if let Some(row) = rows.pop() {
                self.list.remove(&row.row);
            }
        }
        for (row, device) in rows.iter().zip(&hub.devices) {
            row.update(device);
            if !device.selectable() {
                row.check.set_active(false);
            }
        }
    }
}

fn device_menu() -> gtk::PopoverMenu {
    let menu = gio::Menu::new();
    for entries in [
        &[("Enter DFU", "win.dfu"), ("Restart", "win.restart")][..],
        &[
            ("Revive", "win.revive"),
            ("Restore…", "win.restore"),
            ("Advance Setup…", "device.advance-setup"),
        ],
    ] {
        let section = gio::Menu::new();
        for (label, action) in entries {
            let item = gio::MenuItem::new(Some(label), Some(action));
            item.set_attribute_value("hidden-when", Some(&"action-disabled".to_variant()));
            section.append_item(&item);
        }
        menu.append_section(None, &section);
    }
    gtk::PopoverMenu::builder()
        .menu_model(&menu)
        .has_arrow(false)
        .halign(gtk::Align::Start)
        .build()
}

fn connect_context_menu(window: &Rc<Window>, row: &adw::ActionRow, id: DeviceId) {
    let weak = Rc::downgrade(window);
    let row_ref = row.downgrade();
    let open = move |x: f64, y: f64| {
        if let (Some(window), Some(row)) = (weak.upgrade(), row_ref.upgrade())
            && row.is_selectable()
        {
            window.open_device_menu(&row, &id, x, y);
        }
    };
    let click = gtk::GestureClick::builder()
        .button(gdk::BUTTON_SECONDARY)
        .build();
    let on_click = open.clone();
    click.connect_pressed(move |_, _, x, y| on_click(x, y));
    row.add_controller(click);
    let press = gtk::GestureLongPress::builder().touch_only(true).build();
    press.connect_pressed(move |_, x, y| open(x, y));
    row.add_controller(press);
}
