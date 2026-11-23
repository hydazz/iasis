use std::{net::Ipv4Addr, thread};

use anyhow::{Context, Result};
use core_foundation::{
    array::CFArray,
    base::{CFType, TCFType},
    dictionary::CFDictionary,
    runloop::{CFRunLoop, kCFRunLoopCommonModes},
    string::CFString,
};
use system_configuration::dynamic_store::{
    SCDynamicStore, SCDynamicStoreBuilder, SCDynamicStoreCallBackContext,
};

type Changed = Box<dyn Fn() + Send>;

/// Call `changed` whenever the host's interfaces, IPv4 addresses, default
/// route or resolvers change.
pub(super) fn follow_network(changed: impl Fn() + Send + 'static) -> Result<()> {
    fn notify(_: SCDynamicStore, _: CFArray<CFString>, changed: &mut Changed) {
        changed();
    }
    let changed: Changed = Box::new(changed);
    thread::Builder::new()
        .name("network-changes".into())
        .spawn(move || {
            let context = SCDynamicStoreCallBackContext {
                callout: notify,
                info: changed,
            };
            let Some(store) = SCDynamicStoreBuilder::new("Iasis network changes")
                .callback_context(context)
                .build()
            else {
                return;
            };
            let keys = CFArray::from_CFTypes(&[
                CFString::new("State:/Network/Global/DNS"),
                CFString::new("State:/Network/Global/IPv4"),
                CFString::new("State:/Network/Interface"),
            ]);
            let patterns =
                CFArray::from_CFTypes(&[CFString::new("State:/Network/Interface/[^/]+/IPv4")]);
            if !store.set_notification_keys(&keys, &patterns) {
                return;
            }
            let Some(source) = store.create_run_loop_source() else {
                return;
            };
            CFRunLoop::get_current().add_source(&source, unsafe { kCFRunLoopCommonModes });
            CFRunLoop::run_current();
        })?;
    Ok(())
}

pub(super) struct Resolver(Option<SCDynamicStore>);

impl Resolver {
    pub(super) fn new() -> Self {
        Self(None)
    }

    pub(super) fn server(&mut self, usable: impl Fn(&Ipv4Addr) -> bool) -> Result<Ipv4Addr> {
        if self.0.is_none() {
            self.0 = SCDynamicStoreBuilder::new("Iasis USB Internet Sharing").build();
        }
        let store = self.0.as_ref().context("opening SystemConfiguration")?;
        store
            .get("State:/Network/Global/DNS")
            .and_then(|value| value.downcast_into::<CFDictionary>())
            .and_then(|dict| {
                let key = CFString::new("ServerAddresses");
                let value = dict.find(key.as_CFTypeRef())?;
                // The dictionary owns a CFPropertyList containing the resolver addresses.
                unsafe { CFType::wrap_under_get_rule(*value) }.downcast_into::<CFArray>()
            })
            .and_then(|servers| {
                servers.iter().find_map(|value| {
                    let address: Ipv4Addr = unsafe { CFType::wrap_under_get_rule(*value) }
                        .downcast_into::<CFString>()?
                        .to_string()
                        .parse()
                        .ok()?;
                    usable(&address).then_some(address)
                })
            })
            .context("waiting for an IPv4 DNS server reachable from USB devices")
    }
}
