//! Apple Configurator evidence for a Debug USB enumeration on macOS.

use std::{
    collections::{HashMap, HashSet},
    io::{Read, Seek},
    path::Path,
    process::{Command, Stdio},
    sync::{LazyLock, Mutex},
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};

use super::{AppleMode, Identity, IdentitySource};
use crate::topology::{MacAttachment, Snapshot};

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Reply {
    command: String,
    #[serde(rename = "Type")]
    kind: String,
    devices: Vec<String>,
    output: Output,
}

#[derive(Deserialize)]
struct Output {
    #[serde(rename = "Errors", alias = "Error", default)]
    errors: Option<HashMap<String, Map<String, Value>>>,
    #[serde(flatten)]
    devices: HashMap<String, Record>,
}

#[derive(Deserialize)]
struct Record {
    #[serde(rename = "ECID", deserialize_with = "read_ecid")]
    ecid: u64,
    #[serde(rename = "locationID")]
    location: u32,
    #[serde(rename = "deviceType")]
    model: String,
    #[serde(rename = "bootedState")]
    booted: Option<String>,
    #[serde(rename = "isRestorable")]
    restorable: Option<bool>,
}

fn ecid(value: &str) -> Option<u64> {
    let hex = value.strip_prefix("0x")?;
    if hex.is_empty() || hex.len() > 16 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(hex, 16).ok().filter(|id| *id != 0)
}

fn read_ecid<'de, D: Deserializer<'de>>(source: D) -> Result<u64, D::Error> {
    ecid(&String::deserialize(source)?).ok_or_else(|| serde::de::Error::custom("invalid ECID"))
}

impl Reply {
    fn matches(&self, command: &str) -> bool {
        let mut seen = HashSet::new();
        self.kind == "CommandOutput"
            && self.command == command
            && self.output.devices.len() == self.devices.len()
            && self.devices.iter().all(|key| {
                self.output
                    .devices
                    .get(key)
                    .is_some_and(|row| ecid(key) == Some(row.ecid) && seen.insert(row.ecid))
            })
            && self.output.errors.as_ref().is_none_or(|errors| {
                errors
                    .keys()
                    .all(|key| self.output.devices.contains_key(key))
            })
    }

    fn at(&self, location: u32) -> Option<&Record> {
        let mut matching = self
            .output
            .devices
            .iter()
            .filter(|(_, row)| row.location == location);
        let (key, row) = matching.next()?;
        let error = self
            .output
            .errors
            .as_ref()
            .and_then(|errors| errors.get(key));
        (matching.next().is_none() && error.is_none_or(Map::is_empty)).then_some(row)
    }
}

// Limit the time spent waiting for Configurator. Its stdout can exceed a pipe
// buffer, including when listing many devices.
fn run(command: &mut Command, deadline: Instant) -> Option<Reply> {
    if Instant::now() >= deadline {
        return None;
    }
    let mut file = tempfile::tempfile().ok()?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(file.try_clone().ok()?)
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    if file.metadata().ok()?.len() > 1024 * 1024 {
        return None;
    }
    file.rewind().ok()?;
    let reply: Reply = serde_json::from_reader(file.take(1024 * 1024)).ok()?;
    // Configurator can exit 1 for a successful get with empty per-device Errors.
    if !status.success()
        && !(status.code() == Some(1)
            && reply.command == "get"
            && reply
                .output
                .errors
                .as_ref()
                .is_some_and(|errors| reply.devices.iter().all(|id| errors.contains_key(id))))
    {
        return None;
    }
    Some(reply)
}

fn confirm(
    targets: &[MacAttachment],
    present: &impl Fn(MacAttachment) -> bool,
    mut execute: impl FnMut(&[String]) -> Option<Reply>,
) -> HashMap<MacAttachment, (u64, String)> {
    let mut confirmed = HashMap::new();
    let Some(list) = execute(&["list".into()]) else {
        return confirmed;
    };
    if !list.matches("list") {
        return confirmed;
    }
    let selected: Vec<_> = targets
        .iter()
        .filter_map(|key| {
            let row = list.at(key.location_id)?;
            present(*key).then_some((*key, row))
        })
        .collect();
    if selected.is_empty() {
        return confirmed;
    }
    let mut args = Vec::new();
    for (_, row) in &selected {
        args.extend(["--ecid".into(), format!("0x{:X}", row.ecid)]);
    }
    args.extend(
        [
            "get",
            "bootedState",
            "isRestorable",
            "deviceType",
            "locationID",
            "ECID",
        ]
        .map(String::from),
    );
    let Some(get) = execute(&args) else {
        return confirmed;
    };
    if !get.matches("get")
        || get.output.devices.values().any(|row| {
            !selected
                .iter()
                .any(|(_, requested)| requested.ecid == row.ecid)
        })
    {
        return confirmed;
    }
    for (key, expected) in selected {
        if let Some(row) = get.at(key.location_id)
            && row.booted.as_deref() == Some("DFU")
            && row.restorable == Some(true)
            && row.ecid == expected.ecid
            && row.model == expected.model
            && row.model.split_once(',').is_some_and(|(family, revision)| {
                family.starts_with(|c: char| c.is_ascii_alphabetic())
                    && family.ends_with(|c: char| c.is_ascii_digit())
                    && family.bytes().all(|b| b.is_ascii_alphanumeric())
                    && !revision.is_empty()
                    && revision.bytes().all(|b| b.is_ascii_digit())
            })
        {
            confirmed.insert(key, (row.ecid, row.model.clone()));
        }
    }
    confirmed
}

type Cache = HashMap<MacAttachment, (Instant, Option<(u64, String)>)>;
static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(Mutex::default);

pub(super) fn query(snapshot: &mut Snapshot, mask: u8) {
    let Ok(mut cache) = CACHE.lock() else { return };
    enrich(snapshot, mask, &mut cache, MacAttachment::present, |args| {
        let bundle = Path::new("/Applications/Apple Configurator.app/Contents/MacOS/cfgutil");
        let executable = if bundle.is_file() {
            bundle
        } else {
            Path::new("cfgutil")
        };
        run(
            Command::new(executable)
                .args(["--format", "JSON"])
                .args(args),
            Instant::now() + Duration::from_secs(3),
        )
    });
}

fn enrich(
    snapshot: &mut Snapshot,
    mask: u8,
    cache: &mut Cache,
    present: impl Fn(MacAttachment) -> bool,
    execute: impl FnMut(&[String]) -> Option<Reply>,
) {
    let mut candidates = Vec::new();
    let mut counts = HashMap::new();
    for entry in &mut snapshot.ports {
        for device in &entry.devices {
            if let Some(key) = device.macos_attachment {
                *counts.entry(key.location_id).or_insert(0) += 1;
            }
        }
        if mask & (1 << entry.port.index()) == 0 {
            continue;
        }
        for device in &mut entry.devices {
            if let Some(apple) = &mut device.apple
                && apple.mode == AppleMode::KisDfu
            {
                apple.mode = AppleMode::Kis;
                apple.identity = Identity::Missing;
                apple.model = None;
            }
        }
        let mut apple = entry.devices.iter_mut().filter(|d| d.apple.is_some());
        let Some(device) = apple.next() else { continue };
        if apple.next().is_some() || (device.vendor_id, device.product_id) != (0x05ac, 0x1881) {
            continue;
        }
        let apple = device.apple.as_mut().unwrap();
        if let Some(key) = device.macos_attachment
            && key.location_id != 0
            && key.registry_entry_id != 0
        {
            candidates.push((key, apple));
        }
    }
    candidates.retain(|(key, _)| counts[&key.location_id] == 1);
    if candidates.is_empty() {
        return;
    }
    let now = Instant::now();
    cache.retain(|_, (expires, _)| *expires > now);
    let missing: Vec<_> = candidates
        .iter()
        .map(|(k, _)| *k)
        .filter(|k| !cache.contains_key(k))
        .collect();
    if !missing.is_empty() {
        let hits = confirm(&missing, &present, execute);
        for key in missing {
            let hit = hits.get(&key).cloned();
            let expires = Instant::now() + Duration::from_secs(if hit.is_some() { 5 } else { 2 });
            cache.insert(key, (expires, hit));
        }
    }
    for (key, apple) in candidates {
        if let Some((_, Some((id, model)))) = cache.get(&key)
            && present(key)
        {
            apple.mode = AppleMode::KisDfu;
            apple.model = Some(model.clone());
            apple.identity = Identity::Known {
                ecid: *id,
                source: IdentitySource::Configurator,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::tests::{device, snapshot};
    use serde_json::json;
    use std::cell::Cell;

    const KEY: MacAttachment = MacAttachment {
        location_id: 0x0012_0000,
        registry_entry_id: 42,
    };

    fn responses() -> (Value, Value) {
        let list = json!({"Command":"list","Type":"CommandOutput","Devices":["0x11","0x22"],
            "Output":{"0x11":{"ECID":"0x11","locationID":1179648,"deviceType":"Mac17,3"},
                      "0x22":{"ECID":"0x22","locationID":1245184,"deviceType":"Mac17,5"}}});
        let get = json!({"Command":"get","Type":"CommandOutput","Devices":["0x11"],
            "Output":{"0x11":{"ECID":"0x11","locationID":1179648,"deviceType":"Mac17,3",
                             "bootedState":"DFU","isRestorable":true},"Errors":{"0x11":{}}}});
        (list, get)
    }

    #[test]
    fn handles_configurators_exit_one_and_bounds_a_stalled_probe() {
        let (_, get) = responses();
        let run_reply = |value: &Value, code: &str| {
            run(
                Command::new("/bin/sh").args([
                    "-c",
                    r#"printf '%s' "$1"; exit "$2""#,
                    "fixture",
                    &value.to_string(),
                    code,
                ]),
                Instant::now() + Duration::from_secs(2),
            )
        };
        assert!(run_reply(&get, "1").unwrap().matches("get"));
        let mut bad = get.clone();
        bad["Output"].as_object_mut().unwrap().remove("Errors");
        assert!(run_reply(&bad, "1").is_none());
        assert!(run_reply(&get, "2").is_none());
        let start = Instant::now();
        assert!(
            run(
                Command::new("/bin/sleep").arg("5"),
                start + Duration::from_millis(50)
            )
            .is_none()
        );
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    fn transport(list: Value, get: Value) -> impl FnMut(&[String]) -> Option<Reply> {
        move |args| {
            if args == ["list"] {
                serde_json::from_value(list.clone()).ok()
            } else {
                assert_eq!(
                    args,
                    [
                        "--ecid",
                        "0x11",
                        "get",
                        "bootedState",
                        "isRestorable",
                        "deviceType",
                        "locationID",
                        "ECID"
                    ]
                );
                serde_json::from_value(get.clone()).ok()
            }
        }
    }

    #[test]
    fn scopes_get_to_the_enumerated_location_and_expires_evidence_on_replacement() {
        let mut snapshot = snapshot();
        let mut kis = device(0x1881, None);
        kis.macos_attachment = Some(KEY);
        snapshot.ports[1].devices.push(kis.clone());
        let other = MacAttachment {
            location_id: 0x0013_0000,
            registry_entry_id: 46,
        };
        kis.macos_attachment = Some(other);
        snapshot.ports[2].devices.push(kis);
        let port = 1 << snapshot.ports[1].port.index();
        let (list, get) = responses();
        let mut cache = Cache::new();
        enrich(
            &mut snapshot,
            port,
            &mut cache,
            |_| true,
            transport(list, get),
        );
        let apple = snapshot.ports[1].devices[0].apple.as_ref().unwrap();
        assert_eq!(apple.mode, AppleMode::KisDfu);
        assert_eq!(
            apple.identity,
            Identity::Known {
                ecid: 0x11,
                source: IdentitySource::Configurator
            }
        );
        assert_eq!(
            snapshot.ports[2].devices[0].apple.as_ref().unwrap().mode,
            AppleMode::Kis
        );
        enrich(
            &mut snapshot,
            port,
            &mut cache,
            |_| true,
            |_| panic!("cached enumeration re-probed"),
        );
        enrich(
            &mut snapshot,
            port,
            &mut cache,
            |_| false,
            |_| panic!("cached enumeration re-probed"),
        );
        assert_eq!(
            snapshot.ports[1].devices[0].apple.as_ref().unwrap().mode,
            AppleMode::Kis
        );
        snapshot.ports[1].devices[0]
            .macos_attachment
            .as_mut()
            .unwrap()
            .registry_entry_id += 1;
        let queried = Cell::new(false);
        enrich(
            &mut snapshot,
            port,
            &mut cache,
            |_| true,
            |_| {
                queried.set(true);
                None
            },
        );
        assert!(queried.get());
        assert_eq!(
            snapshot.ports[1].devices[0]
                .apple
                .as_ref()
                .unwrap()
                .identity,
            Identity::Missing
        );
    }

    #[test]
    fn refuses_partial_or_conflicting_configurator_results() {
        let (list, get) = responses();
        for (field, value) in [
            ("bootedState", json!("Recovery")),
            ("isRestorable", json!(false)),
            ("deviceType", json!("Unknown")),
            ("ECID", json!("0x12")),
            ("locationID", json!(1245184)),
        ] {
            let mut bad = get.clone();
            bad["Output"]["0x11"][field] = value;
            assert!(
                confirm(&[KEY], &|_| true, transport(list.clone(), bad)).is_empty(),
                "{field}"
            );
        }
        for field in [
            "bootedState",
            "isRestorable",
            "ECID",
            "locationID",
            "deviceType",
        ] {
            let mut bad = get.clone();
            bad["Output"]["0x11"].as_object_mut().unwrap().remove(field);
            assert!(
                confirm(&[KEY], &|_| true, transport(list.clone(), bad)).is_empty(),
                "{field}"
            );
        }
        let mut bad = get.clone();
        bad["Output"]["Errors"]["0x11"] = json!({"reason":"busy"});
        assert!(confirm(&[KEY], &|_| true, transport(list.clone(), bad)).is_empty());
        let mut bad = get.clone();
        bad["Type"] = json!("Error");
        assert!(confirm(&[KEY], &|_| true, transport(list.clone(), bad)).is_empty());
        let mut bad = list.clone();
        bad["Output"]["0x22"]["locationID"] = json!(KEY.location_id);
        assert!(confirm(&[KEY], &|_| true, transport(bad, get.clone())).is_empty());
        let mut bad = get.clone();
        bad["Devices"] = json!(["0x11", "0x11"]);
        assert!(confirm(&[KEY], &|_| true, transport(list.clone(), bad)).is_empty());
        assert!(
            confirm(&[KEY], &|_| false, |args| {
                assert_eq!(args, ["list"]);
                serde_json::from_value(list.clone()).ok()
            })
            .is_empty()
        );
    }

    #[test]
    fn multi_device_slot_and_reenumeration_during_the_probe_stay_unverified() {
        let mut snapshot = snapshot();
        let mut kis = device(0x1881, None);
        kis.macos_attachment = Some(KEY);
        snapshot.ports[0].devices.push(kis.clone());
        let present = Cell::new(true);
        let (list, get) = responses();
        enrich(
            &mut snapshot,
            u8::MAX,
            &mut Cache::new(),
            |_| present.replace(false),
            transport(list.clone(), get.clone()),
        );
        assert_eq!(
            snapshot.ports[0].devices[0].apple.as_ref().unwrap().mode,
            AppleMode::Kis
        );
        snapshot.ports[0].devices.push(device(0x1227, Some(88)));
        enrich(
            &mut snapshot,
            u8::MAX,
            &mut Cache::new(),
            |_| true,
            |_| panic!("ambiguous slot probed"),
        );
        snapshot.ports[0].devices.pop();
        snapshot.ports[1].devices.push(kis);
        enrich(
            &mut snapshot,
            u8::MAX,
            &mut Cache::new(),
            |_| true,
            |_| panic!("duplicate USB location probed"),
        );
    }
}
