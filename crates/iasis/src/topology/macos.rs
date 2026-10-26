use std::{ffi::CStr, io};

use core_foundation::{
    base::{CFType, TCFType},
    number::CFNumber,
    string::CFString,
};
use io_kit_sys::{
    IOIteratorNext, IOObjectRelease, IORegistryEntryCreateCFProperty, IORegistryEntryGetPath,
    IOServiceGetMatchingServices, IOServiceMatching, types::io_object_t,
};

use super::HostPort;
use crate::Topology;

struct Object(io_object_t);

impl Drop for Object {
    fn drop(&mut self) {
        // IOKit matching/iterator APIs transfer one object reference.
        unsafe {
            IOObjectRelease(self.0);
        }
    }
}

pub(super) fn ports() -> io::Result<Vec<HostPort>> {
    let mut iterator = 0;
    // IOServiceGetMatchingServices consumes the matching dictionary.
    let result = unsafe {
        IOServiceGetMatchingServices(
            0,
            IOServiceMatching(c"AppleUSBHostPort".as_ptr()),
            &mut iterator,
        )
    };
    if result != 0 {
        return Err(io::Error::other(format!(
            "IOKit port enumeration failed: {result:#x}"
        )));
    }
    let iterator = Object(iterator);
    let mut ports = Vec::new();
    loop {
        let entry = unsafe { IOIteratorNext(iterator.0) };
        if entry == 0 {
            break;
        }
        let entry = Object(entry);
        let Some(location) = property(&entry, "locationID")
            .and_then(|v| v.downcast_into::<CFNumber>())
            .and_then(|n| n.to_i64())
            .and_then(|n| u32::try_from(n).ok())
        else {
            continue;
        };
        let Some(topology) = location_id(location) else {
            continue;
        };
        let peer = string_property(&entry, "UsbProtocolCompanion (3.x)")
            .or_else(|| string_property(&entry, "UsbProtocolCompanion (2.0)"));
        let mut path = [0; 512];
        let result =
            unsafe { IORegistryEntryGetPath(entry.0, c"IOService".as_ptr(), path.as_mut_ptr()) };
        if result != 0 {
            return Err(io::Error::other(format!(
                "IOKit port path failed: {result:#x}"
            )));
        }
        let path = unsafe { CStr::from_ptr(path.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        ports.push(HostPort {
            path,
            topology,
            peer,
        });
    }
    Ok(ports)
}

fn property(entry: &Object, key: &str) -> Option<CFType> {
    let key = CFString::new(key);
    // Read only the port's own property. Inherited parent metadata could name
    // a different connector, so IORegistryEntrySearchCFProperty is unsuitable.
    let value = unsafe {
        IORegistryEntryCreateCFProperty(entry.0, key.as_concrete_TypeRef(), std::ptr::null(), 0)
    };
    if value.is_null() {
        None
    } else {
        Some(unsafe { CFType::wrap_under_create_rule(value) })
    }
}

fn string_property(entry: &Object, key: &str) -> Option<String> {
    property(entry, key)?
        .downcast_into::<CFString>()
        .map(|s| s.to_string())
}

fn location_id(id: u32) -> Option<Topology> {
    let mut path = id << 8;
    let mut port_chain = Vec::new();
    while path != 0 {
        let port = (path >> 28) as u8;
        if port == 0 {
            return None;
        }
        port_chain.push(port);
        path <<= 4;
    }
    if port_chain.is_empty() {
        return None;
    }
    Some(Topology {
        bus: format!("{:02x}", id >> 24),
        port_chain,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_native_host_port_registry() {
        let ports = ports().unwrap();
        for port in &ports {
            assert!(port.path.starts_with("IOService:/"));
            assert!(!port.topology.port_chain.is_empty());
        }
    }

    #[test]
    fn parses_location_ids_from_macos_port_registry() {
        // Captured AppleUSB20/30XHCIARMPort locations on Apple silicon.
        assert_eq!(
            location_id(0x01100000),
            Some(Topology {
                bus: "01".into(),
                port_chain: vec![1]
            })
        );
        assert_eq!(
            location_id(0x01200000),
            Some(Topology {
                bus: "01".into(),
                port_chain: vec![2]
            })
        );
        assert_eq!(location_id(0x01423000).unwrap().port_chain, [4, 2, 3]);
        assert_eq!(location_id(0x01040100), None);
    }
}
