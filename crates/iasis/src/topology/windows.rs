//! Windows gives no location path to a device on a host controller that is
//! not on PCI, such as the one Apple's KIS driver creates for what a target
//! sends through Debug USB. Such a device takes the place one tier below the
//! nearest located USB device among its ancestors, where macOS numbers the
//! devices its own Debug USB host presents.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::{OsStrExt, OsStringExt};

use nusb::DeviceInfo;
use windows_sys::Win32::Devices::{
    DeviceAndDriverInstallation::{
        CM_Get_DevNode_PropertyW, CM_Get_Device_IDW, CM_Get_Parent, CM_LOCATE_DEVNODE_NORMAL,
        CM_Locate_DevNodeW, CR_SUCCESS, MAX_DEVICE_ID_LEN,
    },
    Properties::{DEVPKEY_Device_ContainerId, DEVPROP_TYPE_GUID},
};

use super::UsbDevice;
use crate::Topology;

/// How far up the device tree a located ancestor is looked for.
const DEPTH: usize = 8;

pub(super) fn devices(devices: Vec<DeviceInfo>) -> Vec<UsbDevice> {
    let located: HashMap<String, Topology> = devices
        .iter()
        .filter(|device| !device.port_chain().is_empty())
        .map(|device| {
            (
                key(device.instance_id()),
                Topology {
                    bus: device.bus_id().to_owned(),
                    port_chain: device.port_chain().to_vec(),
                },
            )
        })
        .collect();
    devices
        .into_iter()
        .map(|info| {
            let presented = info
                .port_chain()
                .is_empty()
                .then(|| below_ancestor(&info, &located))
                .flatten();
            let mut device = UsbDevice::from(info);
            if let Some(topology) = presented {
                device.topology = topology;
            }
            device
        })
        .collect()
}

/// The Container ID of each USB hub. Windows takes a hub's from its BOS
/// descriptor, which both functions of a USB 3 hub report alike, and gives a
/// hub without one its parent's or a container of its own.
pub(super) fn containers(devices: &[DeviceInfo]) -> Vec<(Topology, [u8; 16])> {
    devices
        .iter()
        .filter(|device| device.class() == 0x09 && !device.port_chain().is_empty())
        .filter_map(|device| {
            let id = container_id(locate(device.instance_id())?)?;
            let location = Topology {
                bus: device.bus_id().to_owned(),
                port_chain: device.port_chain().to_vec(),
            };
            Some((location, id))
        })
        .collect()
}

/// The device's Container ID, other than the one Windows gives devices that
/// belong to no container.
fn container_id(node: u32) -> Option<[u8; 16]> {
    const NONE: [u8; 16] = [
        0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    ];
    let mut kind = 0;
    let mut id = [0u8; 16];
    let mut size = id.len() as u32;
    // SAFETY: the buffer's length is passed with it.
    let result = unsafe {
        CM_Get_DevNode_PropertyW(
            node,
            &DEVPKEY_Device_ContainerId,
            &mut kind,
            id.as_mut_ptr(),
            &mut size,
            0,
        )
    };
    (result == CR_SUCCESS && kind == DEVPROP_TYPE_GUID && size == 16 && id != NONE).then_some(id)
}

fn below_ancestor(info: &DeviceInfo, located: &HashMap<String, Topology>) -> Option<Topology> {
    let port = u8::try_from(info.port_number()).ok()?;
    let mut node = locate(info.parent_instance_id())?;
    for _ in 0..DEPTH {
        if let Some(found) = located.get(&key(&device_id(node)?)) {
            let mut topology = found.clone();
            topology.port_chain.push(port);
            return Some(topology);
        }
        node = parent(node)?;
    }
    None
}

/// Instance IDs compare without case; Windows reports them in both.
fn key(id: &OsStr) -> String {
    id.to_string_lossy().to_uppercase()
}

fn locate(id: &OsStr) -> Option<u32> {
    let id: Vec<u16> = id.encode_wide().chain([0]).collect();
    let mut node = 0;
    // SAFETY: `id` is NUL-terminated and outlives the call.
    let result = unsafe { CM_Locate_DevNodeW(&mut node, id.as_ptr(), CM_LOCATE_DEVNODE_NORMAL) };
    (result == CR_SUCCESS).then_some(node)
}

fn parent(node: u32) -> Option<u32> {
    let mut parent = 0;
    // SAFETY: `parent` is a valid out pointer.
    let result = unsafe { CM_Get_Parent(&mut parent, node, 0) };
    (result == CR_SUCCESS).then_some(parent)
}

fn device_id(node: u32) -> Option<OsString> {
    let mut id = [0u16; MAX_DEVICE_ID_LEN as usize + 1];
    // SAFETY: the buffer's length is passed with it.
    let result = unsafe { CM_Get_Device_IDW(node, id.as_mut_ptr(), id.len() as u32, 0) };
    if result != CR_SUCCESS {
        return None;
    }
    let length = id.iter().position(|&c| c == 0).unwrap_or(id.len());
    Some(OsString::from_wide(&id[..length]))
}
