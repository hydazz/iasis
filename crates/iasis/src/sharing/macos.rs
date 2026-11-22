use std::net::Ipv4Addr;

use anyhow::{Context, Result};
use core_foundation::{
    array::CFArray,
    base::{CFType, TCFType},
    dictionary::CFDictionary,
    string::CFString,
};
use system_configuration::dynamic_store::{SCDynamicStore, SCDynamicStoreBuilder};

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
