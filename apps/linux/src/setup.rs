//! Choose and check the setup steps for one selection of iPhones and iPads.

use std::{cell::Cell, rc::Rc};

use adw::prelude::*;
use gtk::glib;
use iasis_api::{PostRestore, Reply, Request, Selection, SetupLanguage};

use crate::{agent, preferences::desktop_setup_language};

pub struct SetupDialog {
    pub dialog: adw::AlertDialog,
    pub hubs: Vec<Selection>,
    settings: SetupLanguage,
    language: gtk::CheckButton,
    enrollment: gtk::CheckButton,
    status: gtk::Label,
    revision: Cell<u64>,
}

impl SetupDialog {
    pub fn new(hubs: Vec<Selection>, saved: &PostRestore, subject: &str) -> Rc<Self> {
        let dialog = adw::AlertDialog::new(Some("Advance Setup"), Some(subject));
        dialog.add_response("cancel", "_Cancel");
        dialog.add_response("apply", "_Advance Setup");
        dialog.set_response_appearance("apply", adw::ResponseAppearance::Suggested);
        dialog.set_close_response("cancel");
        dialog.set_response_enabled("apply", false);
        dialog.set_default_response(Some("apply"));

        let settings = saved
            .setup_language
            .clone()
            .unwrap_or_else(desktop_setup_language);
        let language = gtk::CheckButton::with_label(&format!(
            "Set language and region ({}, {})",
            settings.language, settings.locale
        ));
        language.set_active(saved.setup_language.is_some() || !saved.try_enrollment);
        let enrollment = gtk::CheckButton::with_label("Try enrollment");
        enrollment.set_active(saved.try_enrollment);
        let fields = gtk::Box::new(gtk::Orientation::Vertical, 6);
        fields.append(&language);
        fields.append(&enrollment);
        fields.append(
            &gtk::Label::builder()
                .label("Request the assigned management profile. If sign-in or “Partially Set Up” appears, continue on the device.")
                .xalign(0.0)
                .wrap(true)
                .css_classes(["dim-label"])
                .build(),
        );
        let status = gtk::Label::builder().xalign(0.0).wrap(true).build();
        fields.append(&status);
        dialog.set_extra_child(Some(&fields));

        let this = Rc::new(Self {
            dialog,
            hubs,
            settings,
            language,
            enrollment,
            status,
            revision: Cell::new(0),
        });
        for toggle in [&this.language, &this.enrollment] {
            let weak = Rc::downgrade(&this);
            toggle.connect_toggled(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.check();
                }
            });
        }
        this
    }

    pub fn present(self: &Rc<Self>, parent: &impl IsA<gtk::Widget>) {
        self.check();
        self.dialog.present(Some(parent));
    }

    fn request(&self, check_only: bool) -> Request {
        Request::AdvanceSetup {
            hubs: self.hubs.clone(),
            settings: self.language.is_active().then(|| self.settings.clone()),
            dep: self.enrollment.is_active(),
            check_only,
        }
    }

    fn check(self: &Rc<Self>) {
        self.dialog.set_response_enabled("apply", false);
        let revision = self.revision.get() + 1;
        self.revision.set(revision);
        self.status.set_visible(true);
        if !self.language.is_active() && !self.enrollment.is_active() {
            self.status.set_label("Select at least one setup step.");
            return;
        }
        self.status.set_label("Checking selected devices…");
        let request = self.request(true);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = agent::ask(request).await;
            let Some(this) = weak.upgrade() else { return };
            if this.revision.get() != revision {
                return;
            }
            match result {
                Ok(Reply::Ok) => {
                    this.status.set_visible(false);
                    this.dialog.set_response_enabled("apply", true);
                }
                Ok(Reply::Error(message)) | Err(message) => this.status.set_label(&message),
                Ok(_) => this
                    .status
                    .set_label("The agent returned an unexpected reply."),
            }
        });
    }

    pub fn invalidate(&self) {
        if self.language.get_sensitive() {
            self.revision.set(self.revision.get() + 1);
            self.language.set_sensitive(false);
            self.enrollment.set_sensitive(false);
            self.dialog.set_response_enabled("apply", false);
            self.status.set_label(
                "The selected devices changed. Close this dialog and select them again.",
            );
            self.status.set_visible(true);
        }
    }

    pub fn approved_request(&self) -> Option<Request> {
        self.dialog
            .is_response_enabled("apply")
            .then(|| self.request(false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gtk::test]
    fn the_form_uses_saved_steps_and_falls_back_to_the_desktop_language() {
        adw::init().unwrap();
        let language = SetupLanguage {
            language: "fr-CA".into(),
            locale: "fr_CA".into(),
        };
        for (saved_language, enrollment) in [
            (None, false),
            (None, true),
            (Some(language.clone()), true),
            (Some(language), false),
        ] {
            let saved = PostRestore {
                setup_language: saved_language.clone(),
                try_enrollment: enrollment,
                ..PostRestore::default()
            };
            let form = SetupDialog::new(Vec::new(), &saved, "iPad");
            assert_eq!(
                form.request(true),
                Request::AdvanceSetup {
                    hubs: Vec::new(),
                    settings: saved_language.or_else(|| (!enrollment).then(desktop_setup_language)),
                    dep: enrollment,
                    check_only: true,
                }
            );
            assert!(form.approved_request().is_none());
        }
    }
}
