use std::{
    io::{self, Cursor, Read, Write},
    os::unix::net::UnixStream,
    time::Duration,
};

use plist::{Dictionary, Value};

use super::{AppleDevice, AppleMode, Identity, IdentitySource};
use crate::topology::UsbDevice;

const SOCKET: &str = "/var/run/usbmuxd";
const TIMEOUT: Duration = Duration::from_secs(2);
const MAX_FRAME: usize = 1024 * 1024;

#[derive(Debug)]
pub(super) struct MuxDevice {
    id: u32,
    serial: String,
    product: u16,
}

pub(super) fn list() -> io::Result<Vec<MuxDevice>> {
    let mut socket = socket()?;
    let response = mux_request(
        &mut socket,
        dictionary([("MessageType", "ListDevices".into())]),
    )?;
    parse_list(&response)
}

pub(super) fn identify(device: &UsbDevice, devices: &[MuxDevice]) -> Result<AppleDevice, String> {
    let serial = device
        .serial_number
        .as_deref()
        .ok_or("USB serial is unavailable for usbmux identity matching")?;
    let matches: Vec<_> = devices
        .iter()
        .filter(|entry| {
            entry.product == device.product_id
                && normalize_udid(&entry.serial).eq_ignore_ascii_case(&normalize_udid(serial))
        })
        .collect();
    let [entry] = matches.as_slice() else {
        return Err(if matches.is_empty() {
            "USB target is not listed by the system usbmuxd service"
        } else {
            "multiple usbmuxd devices match the USB target"
        }
        .into());
    };
    query(entry.id).map_err(|error| error.to_string())
}

fn query(id: u32) -> io::Result<AppleDevice> {
    let mut socket = socket()?;
    let response = mux_request(
        &mut socket,
        dictionary([
            ("MessageType", "Connect".into()),
            ("DeviceID", id.into()),
            ("PortNumber", u32::from(62078_u16.to_be()).into()),
        ]),
    )?;
    if response
        .as_dictionary()
        .and_then(|v| v.get("Number"))
        .and_then(Value::as_unsigned_integer)
        != Some(0)
    {
        return Err(io::Error::other(
            "usbmuxd could not connect to the target identity service",
        ));
    }
    let response = service_request(&mut socket, dictionary([("Request", "QueryType".into())]))?;
    let service = response
        .as_dictionary()
        .and_then(|v| v.get("Type"))
        .and_then(Value::as_string)
        .ok_or_else(|| invalid("identity service did not return its type"))?;
    let (request, mode, source) = match service {
        "com.apple.mobile.restored" => (
            dictionary([
                ("Request", "QueryValue".into()),
                ("QueryKey", "HardwareInfo".into()),
            ]),
            AppleMode::Restored,
            IdentitySource::Restored,
        ),
        "com.apple.mobile.lockdown" => (
            dictionary([
                ("Request", "GetValue".into()),
                ("Key", "UniqueChipID".into()),
            ]),
            AppleMode::Normal,
            IdentitySource::Lockdown,
        ),
        _ => return Err(invalid("unrecognized Apple identity service")),
    };
    let response = service_request(&mut socket, request);
    if mode == AppleMode::Restored {
        // Close only this query session, as restored_client_free does.
        let _ = service_request(&mut socket, dictionary([("Request", "Goodbye".into())]));
    }
    let ecid = parse_identity_response(&response?, mode)?;
    Ok(AppleDevice {
        mode,
        identity: Identity::Known { ecid, source },
    })
}

fn parse_identity_response(response: &Value, mode: AppleMode) -> io::Result<u64> {
    let dict = response
        .as_dictionary()
        .ok_or_else(|| invalid("identity response is not a dictionary"))?;
    if dict.contains_key("Error") {
        return Err(io::Error::other(
            "Apple identity service refused the ECID query",
        ));
    }
    let value = match mode {
        AppleMode::Restored => dict
            .get("HardwareInfo")
            .and_then(Value::as_dictionary)
            .and_then(|v| v.get("UniqueChipID")),
        AppleMode::Normal => dict.get("Value"),
        _ => None,
    };
    value
        .and_then(Value::as_unsigned_integer)
        .filter(|ecid| *ecid != 0)
        .ok_or_else(|| invalid("identity response has no nonzero UniqueChipID integer"))
}

fn parse_list(response: &Value) -> io::Result<Vec<MuxDevice>> {
    let list = response
        .as_dictionary()
        .and_then(|dict| dict.get("DeviceList"))
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("usbmuxd response has no DeviceList"))?;
    let mut devices = Vec::new();
    for device in list {
        let props = device
            .as_dictionary()
            .and_then(|dict| dict.get("Properties"))
            .and_then(Value::as_dictionary)
            .ok_or_else(|| invalid("usbmuxd device has no Properties"))?;
        if props.get("ConnectionType").and_then(Value::as_string) != Some("USB") {
            continue;
        }
        let id = props
            .get("DeviceID")
            .and_then(Value::as_unsigned_integer)
            .and_then(|id| u32::try_from(id).ok())
            .ok_or_else(|| invalid("usbmuxd device has no DeviceID"))?;
        let serial = props
            .get("SerialNumber")
            .and_then(Value::as_string)
            .filter(|serial| !serial.is_empty())
            .ok_or_else(|| invalid("usbmuxd device has no SerialNumber"))?
            .to_owned();
        let product = props
            .get("ProductID")
            .and_then(Value::as_unsigned_integer)
            .and_then(|id| u16::try_from(id).ok())
            .ok_or_else(|| invalid("usbmuxd device has no ProductID"))?;
        devices.push(MuxDevice {
            id,
            serial,
            product,
        });
    }
    Ok(devices)
}

fn normalize_udid(serial: &str) -> String {
    // usbmuxd inserts this separator in the 24-character USB serial format.
    if serial.len() == 24 && serial.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        format!("{}-{}", &serial[..8], &serial[8..])
    } else {
        serial.to_owned()
    }
}

fn socket() -> io::Result<UnixStream> {
    let socket = UnixStream::connect(SOCKET)?;
    socket.set_read_timeout(Some(TIMEOUT))?;
    socket.set_write_timeout(Some(TIMEOUT))?;
    Ok(socket)
}

fn dictionary<const N: usize>(pairs: [(&str, Value); N]) -> Value {
    Value::Dictionary(
        pairs
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect::<Dictionary>(),
    )
}

fn xml(mut request: Value) -> io::Result<Vec<u8>> {
    request
        .as_dictionary_mut()
        .unwrap()
        .insert("Label".into(), "iasis".into());
    let mut payload = Vec::new();
    request
        .to_writer_xml(&mut payload)
        .map_err(|error| invalid(error.to_string()))?;
    Ok(payload)
}

fn mux_request(socket: &mut (impl Read + Write), request: Value) -> io::Result<Value> {
    let payload = xml(request)?;
    for word in [payload.len() as u32 + 16, 1, 8, 1] {
        socket.write_all(&word.to_le_bytes())?;
    }
    socket.write_all(&payload)?;
    read_mux(socket)
}

fn read_mux(socket: &mut impl Read) -> io::Result<Value> {
    let mut header = [0; 16];
    socket.read_exact(&mut header)?;
    let word = |offset| u32::from_le_bytes(header[offset..offset + 4].try_into().unwrap());
    let length = word(0) as usize;
    if !(16..=MAX_FRAME).contains(&length) || word(4) != 1 || word(8) != 8 || word(12) != 1 {
        return Err(invalid("invalid usbmuxd response header"));
    }
    read_plist(socket, length - 16)
}

fn service_request(socket: &mut (impl Read + Write), request: Value) -> io::Result<Value> {
    let payload = xml(request)?;
    socket.write_all(&(payload.len() as u32).to_be_bytes())?;
    socket.write_all(&payload)?;
    read_service(socket)
}

fn read_service(socket: &mut impl Read) -> io::Result<Value> {
    let mut header = [0; 4];
    socket.read_exact(&mut header)?;
    let length = u32::from_be_bytes(header) as usize;
    if !(1..=MAX_FRAME).contains(&length) {
        return Err(invalid("invalid identity service frame length"));
    }
    read_plist(socket, length)
}

fn read_plist(socket: &mut impl Read, length: usize) -> io::Result<Value> {
    let mut payload = vec![0; length];
    socket.read_exact(&mut payload)?;
    Value::from_reader(Cursor::new(payload)).map_err(|error| invalid(error.to_string()))
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_usbmux_device_list_and_ignores_network_targets() {
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>DeviceList</key><array><dict><key>DeviceID</key><integer>7</integer><key>Properties</key><dict><key>ConnectionType</key><string>USB</string><key>DeviceID</key><integer>7</integer><key>SerialNumber</key><string>00008103-001234567890ABCD</string><key>ProductID</key><integer>6401</integer></dict></dict><dict><key>Properties</key><dict><key>ConnectionType</key><string>Network</string></dict></dict></array></dict></plist>"#;
        let payload = Value::from_reader(Cursor::new(xml)).unwrap();
        let devices = parse_list(&payload).unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, 7);
        assert_eq!(devices[0].product, 0x1901);
        assert_eq!(
            normalize_udid("00008103001234567890ABCD"),
            devices[0].serial
        );
        assert_eq!(normalize_udid("ECID:123"), "ECID:123");
    }

    #[test]
    fn reads_restored_and_lockdown_unique_chip_id_without_string_guessing() {
        let restored = dictionary([(
            "HardwareInfo",
            dictionary([("UniqueChipID", 0x12345678_u64.into())]),
        )]);
        assert_eq!(
            parse_identity_response(&restored, AppleMode::Restored).unwrap(),
            0x12345678
        );
        assert_eq!(
            parse_identity_response(&dictionary([("Value", 42_u64.into())]), AppleMode::Normal)
                .unwrap(),
            42
        );
        for value in ["42".into(), 0_u64.into(), Value::Boolean(true)] {
            assert!(
                parse_identity_response(&dictionary([("Value", value)]), AppleMode::Normal)
                    .is_err()
            );
        }
        assert!(
            parse_identity_response(
                &dictionary([("Error", "GetProhibited".into())]),
                AppleMode::Normal
            )
            .is_err()
        );
    }

    #[test]
    fn validates_distinct_mux_and_service_framing_and_truncation() {
        let payload = xml(dictionary([("Value", 42_u64.into())])).unwrap();
        let mut mux = Vec::new();
        for word in [payload.len() as u32 + 16, 1, 8, 1] {
            mux.extend(word.to_le_bytes());
        }
        mux.extend(&payload);
        assert!(read_mux(&mut Cursor::new(&mux)).is_ok());
        assert!(read_mux(&mut Cursor::new(&mux[..mux.len() - 1])).is_err());
        mux[12] = 2;
        assert!(read_mux(&mut Cursor::new(mux)).is_err());
        let mut service = (payload.len() as u32).to_be_bytes().to_vec();
        service.extend(payload);
        assert!(read_service(&mut Cursor::new(service)).is_ok());
        assert!(read_service(&mut Cursor::new(u32::MAX.to_be_bytes())).is_err());
    }
}
