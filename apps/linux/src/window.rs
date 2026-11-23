//! The main window: every port of every board in a boxed list per board,
//! the selected device in a utility pane beside them, and the actions the
//! agent allows.

use std::{
    cell::RefCell,
    rc::{Rc, Weak},
};

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use iasis_api::{Reply, Request, State};

use crate::{
    agent::{self, Event},
    device::{self, Actions, Board, Device, DeviceId},
    inspector::{Inspector, Shown},
    preferences::Preferences,
    row::PortRow,
};

pub struct Window {
    pub window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    split: adw::OverlaySplitView,
    banner: adw::Banner,
    stack: gtk::Stack,
    status: adw::StatusPage,
    boards: gtk::Box,
    groups: RefCell<Vec<BoardGroup>>,
    inspector: Inspector,
    select: gtk::ToggleButton,
    action_bar: gtk::ActionBar,
    title: adw::WindowTitle,
    preferences: RefCell<Option<Rc<Preferences>>>,
    model: RefCell<Model>,
}

#[derive(Default)]
struct Model {
    state: Option<State>,
    boards: Vec<Board>,
    selected: Vec<DeviceId>,
    selecting: bool,
    actions: Actions,
}

/// One board: a boxed list of its ports under its name, with a row for why
/// it cannot be read or what it needs.
struct BoardGroup {
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
            .build();
        let title = adw::WindowTitle::new("Iasis", "");
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        header.pack_start(&select);
        header.pack_end(&primary);
        header.pack_end(&show_inspector);

        let banner = adw::Banner::new("");
        let status = adw::StatusPage::new();
        let boards = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(24)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(12)
            .margin_end(12)
            .build();
        let clamp = adw::Clamp::builder()
            .maximum_size(720)
            .child(&boards)
            .build();
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&clamp)
            .build();
        let stack = gtk::Stack::new();
        stack.add_named(&status, Some("status"));
        stack.add_named(&scrolled, Some("boards"));

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
            .build();
        split
            .bind_property("show-sidebar", &show_inspector, "active")
            .bidirectional()
            .sync_create()
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
            boards,
            groups: RefCell::default(),
            inspector,
            select,
            action_bar,
            title,
            preferences: RefCell::default(),
            model: RefCell::default(),
        });
        this.add_actions();
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
            this.perform_all(|actions| actions.dfu.clone())
        });
        simple("restart", |this| {
            this.perform_all(|actions| actions.restart.clone());
        });
        simple("revive", |this| {
            this.perform_all(|actions| actions.revive.clone().into_iter().collect());
        });
        simple("restore", |this| this.restore());

        let update = gio::SimpleAction::new("update-board", Some(glib::VariantTy::STRING));
        let weak = Rc::downgrade(self);
        update.connect_activate(move |_, serial| {
            if let (Some(this), Some(board)) = (weak.upgrade(), serial.and_then(|s| s.str())) {
                this.update_board(board.to_owned());
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
        self.select_one(id);
        self.split.set_show_sidebar(true);
    }

    pub fn toast(&self, message: &str) {
        self.toasts.add_toast(adw::Toast::new(message));
    }

    /// Show what the agent says.
    pub fn show(self: &Rc<Self>, event: Event) {
        match event {
            Event::State(state) => {
                let boards = device::boards(&state);
                if boards.is_empty() {
                    self.status.set_icon_name(Some("media-removable-symbolic"));
                    self.status.set_title("No Boards Connected");
                    self.status
                        .set_description(Some("Connect an Iasis board to this computer."));
                    self.stack.set_visible_child_name("status");
                } else {
                    self.stack.set_visible_child_name("boards");
                }
                self.show_banner(&state);
                self.show_boards(&boards);
                if let Some(preferences) = &*self.preferences.borrow() {
                    preferences.update(&state);
                }
                let mut model = self.model.borrow_mut();
                model.boards = boards;
                model.state = Some(*state);
            }
            Event::Absent | Event::Refused(_) => {
                let (icon, title, description) = match &event {
                    Event::Refused(reason) => (
                        "changes-prevent-symbolic",
                        "No Access to Iasis",
                        format!("{reason}."),
                    ),
                    _ => (
                        "computer-fail-symbolic",
                        "Iasis Service Not Running",
                        "The iasisd service attends the boards. Start it with “sudo systemctl start iasisd”.".to_owned(),
                    ),
                };
                self.status.set_icon_name(Some(icon));
                self.status.set_title(title);
                self.status.set_description(Some(&description));
                self.stack.set_visible_child_name("status");
                self.banner.set_revealed(false);
                self.show_boards(&[]);
                let mut model = self.model.borrow_mut();
                model.boards.clear();
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
            format!(
                "Downloading {} ({}), {percent}%",
                fetch.version, fetch.build
            )
        });
        if let Some(message) = &message {
            self.banner.set_title(&glib::markup_escape_text(message));
        }
        self.banner.set_revealed(message.is_some());
    }

    fn show_boards(self: &Rc<Self>, boards: &[Board]) {
        let mut groups = self.groups.borrow_mut();
        groups.retain(|group| {
            let keep = boards.iter().any(|board| board.view.serial == group.serial);
            if !keep {
                self.boards.remove(&group.group);
            }
            keep
        });
        for (index, board) in boards.iter().enumerate() {
            let position = groups
                .iter()
                .position(|group| group.serial == board.view.serial);
            let group = match position {
                Some(position) => groups.remove(position),
                None => BoardGroup::new(self, &board.view.serial),
            };
            group.show(self, board);
            // Keep the agent's board order.
            let previous = index
                .checked_sub(1)
                .and_then(|index| groups.get(index))
                .map(|previous| previous.group.clone().upcast::<gtk::Widget>());
            if group.group.parent().is_none() {
                self.boards.append(&group.group);
            }
            self.boards
                .reorder_child_after(&group.group, previous.as_ref());
            groups.insert(index, group);
        }
    }

    /// The devices the model holds selected, as the latest state shows them.
    fn selected(&self) -> Vec<Device> {
        let model = self.model.borrow();
        model
            .boards
            .iter()
            .flat_map(|board| &board.devices)
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
        enable("revive", actions.revive.is_some());
        enable("restore", actions.restore.is_some());
        let selecting = self.model.borrow().selecting;
        // Selection mode names what is selected where the app's name was.
        self.title.set_title(&match (selecting, selected.len()) {
            (false, _) => "Iasis".to_owned(),
            (true, 0) => "No Devices Selected".to_owned(),
            (true, 1) => "1 Device Selected".to_owned(),
            (true, count) => format!("{count} Devices Selected"),
        });
        self.inspector.show(match (selecting, selected.as_slice()) {
            (_, []) => Shown::Nothing,
            (_, [device]) => Shown::Device(Box::new(device.clone())),
            (_, devices) => Shown::Many(devices.len()),
        });
        self.model.borrow_mut().actions = actions;
    }

    fn set_selecting(self: &Rc<Self>, selecting: bool) {
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
        self.model.borrow_mut().selected = vec![id.clone()];
        for group in self.groups.borrow().iter() {
            if group.serial != id.0 {
                group.list.unselect_all();
            } else if let Some(row) = group.rows.borrow().get(usize::from(id.1) - 1) {
                group.list.select_row(Some(&row.row));
            }
        }
        self.refresh();
    }

    fn check(self: &Rc<Self>, id: DeviceId, on: bool) {
        {
            let mut model = self.model.borrow_mut();
            model.selected.retain(|selected| *selected != id);
            if on {
                model.selected.push(id);
            }
        }
        self.refresh();
    }

    /// Send each request the actions make, and say why any was refused.
    fn perform_all(self: &Rc<Self>, requests: impl Fn(&Actions) -> Vec<Request>) {
        let requests = requests(&self.model.borrow().actions);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            for request in requests {
                if let Err(message) = agent::perform(request).await
                    && let Some(this) = weak.upgrade()
                {
                    this.toast(&message);
                }
            }
        });
    }

    fn update_board(self: &Rc<Self>, serial: String) {
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = agent::perform(Request::Update {
                board: serial,
                bytes: None,
            })
            .await;
            if let (Err(message), Some(this)) = (result, weak.upgrade()) {
                this.toast(&message);
            }
        });
    }

    /// Ask what each device would get, then whether to erase them.
    fn restore(self: &Rc<Self>) {
        let Some(mut confirmation) = self.model.borrow().actions.restore.clone() else {
            return;
        };
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let planned = agent::ask(Request::Plan {
                boards: confirmation.boards.clone(),
            })
            .await;
            let Some(this) = weak.upgrade() else { return };
            match planned {
                Ok(Reply::Plan(planned)) => confirmation.planned = planned,
                Ok(Reply::Error(message)) | Err(message) => return this.toast(&message),
                Ok(_) => {}
            }
            let dialog =
                adw::AlertDialog::new(Some(&confirmation.heading()), Some(&confirmation.body()));
            dialog.add_response("cancel", "_Cancel");
            let choices = confirmation.choices();
            for (index, choice) in choices.iter().enumerate() {
                let id = index.to_string();
                dialog.add_response(&id, &choice.label);
                dialog.set_response_appearance(&id, adw::ResponseAppearance::Destructive);
            }
            dialog.set_default_response(Some("cancel"));
            dialog.set_close_response("cancel");
            let response = dialog.choose_future(Some(&this.window)).await;
            let Some(choice) = response
                .parse::<usize>()
                .ok()
                .and_then(|index| choices.into_iter().nth(index))
            else {
                return;
            };
            if let Err(message) = agent::perform(choice.request).await {
                this.toast(&message);
            }
        });
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

impl BoardGroup {
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
            .tooltip_text("Board")
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
            .action_name("win.update-board")
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

    fn show(&self, window: &Rc<Window>, board: &Board) {
        let view = &board.view;
        self.group
            .set_title(&glib::markup_escape_text(&board.title));
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
                Some("win.update-board"),
                Some(&view.serial.to_variant()),
            );
            self.menu.append_item(&item);
        }
        self.menu
            .append(Some("Board Preferences"), Some("app.preferences"));

        let required = view.update.as_ref().filter(|update| update.required);
        let notice = match (&view.notice, required) {
            (_, Some(update)) => Some(format!(
                "IasisOS {} is from another build. Update the board to {} to use it.",
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
        while rows.len() < board.devices.len() {
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
            context_menu(window, &row.row, id);
            self.list.append(&row.row);
            rows.push(row);
        }
        while rows.len() > board.devices.len() {
            if let Some(row) = rows.pop() {
                self.list.remove(&row.row);
            }
        }
        for (row, device) in rows.iter().zip(&board.devices) {
            row.update(device);
        }
    }
}

/// The device actions on a secondary click or a long press, for the row
/// under the pointer.
fn context_menu(window: &Rc<Window>, row: &adw::ActionRow, id: DeviceId) {
    let menu = gio::Menu::new();
    let signals = gio::Menu::new();
    signals.append(Some("Enter DFU"), Some("win.dfu"));
    signals.append(Some("Restart"), Some("win.restart"));
    menu.append_section(None, &signals);
    let operations = gio::Menu::new();
    operations.append(Some("Revive"), Some("win.revive"));
    operations.append(Some("Restore…"), Some("win.restore"));
    menu.append_section(None, &operations);
    let popover = gtk::PopoverMenu::builder()
        .menu_model(&menu)
        .has_arrow(false)
        .halign(gtk::Align::Start)
        .build();
    popover.set_parent(row);

    let open = {
        let weak = Rc::downgrade(window);
        let popover = popover.clone();
        move |x: f64, y: f64| {
            let Some(window) = weak.upgrade() else { return };
            if !window.model.borrow().selecting {
                window.select_one(id.clone());
            }
            popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            popover.popup();
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
