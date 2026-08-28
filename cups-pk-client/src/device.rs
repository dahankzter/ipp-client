// SPDX-License-Identifier: MIT OR Apache-2.0

use std::collections::{BTreeMap, HashMap};

/// A printer discovered by `DevicesGet`.
///
/// Fields the mechanism did not report are empty strings rather than options:
/// every one of them is descriptive text destined for a UI, and an absent
/// description and an empty one call for the same treatment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Device {
    /// The device URI, e.g. `ipp://printer.local/ipp/print`. Never empty: a
    /// device without one is dropped during decoding.
    pub uri: String,
    /// `network`, `direct`, `file`, or a backend-specific value.
    pub class: String,
    pub info: String,
    pub make_and_model: String,
    pub device_id: String,
    pub location: String,
}

impl Device {
    /// Whether this is a network device, for grouping in an add-printer flow.
    pub fn is_network(&self) -> bool {
        self.class == "network"
    }
}

/// Decodes `DevicesGet`'s flat, index-suffixed reply.
///
/// The mechanism emits keys as `device-uri:0`, `device-class:0`, `device-uri:1`
/// and so on — built in its `cups.c` with `g_strdup_printf("device-uri:%d", i)`
/// — so records are recovered by grouping on the suffix after the final colon.
/// A `BTreeMap` keyed by index gives the caller a stable order the wire format
/// does not itself provide.
pub(crate) fn decode_devices(raw: HashMap<String, String>) -> Vec<Device> {
    let mut by_index: BTreeMap<u32, Device> = BTreeMap::new();

    for (key, value) in raw {
        let Some((field, index)) = key.rsplit_once(':') else {
            continue;
        };
        let Ok(index) = index.parse::<u32>() else {
            continue;
        };

        let device = by_index.entry(index).or_default();
        match field {
            "device-uri" => device.uri = value,
            "device-class" => device.class = value,
            "device-info" => device.info = value,
            "device-make-and-model" => device.make_and_model = value,
            "device-id" => device.device_id = value,
            "device-location" => device.location = value,
            other => tracing::debug!("ignoring unknown device field {other}"),
        }
    }

    by_index
        .into_values()
        .filter(|d| !d.uri.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn groups_a_flat_dictionary_by_its_index_suffix() {
        let devices = decode_devices(raw(&[
            ("device-uri:0", "ipp://printer.local/ipp/print"),
            ("device-class:0", "network"),
            ("device-info:0", "Office Printer"),
            ("device-make-and-model:0", "HP OfficeJet Pro 8210"),
            ("device-uri:1", "usb://Brother/HL-2030"),
            ("device-class:1", "direct"),
        ]));

        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].uri, "ipp://printer.local/ipp/print");
        assert_eq!(devices[0].make_and_model, "HP OfficeJet Pro 8210");
        assert_eq!(devices[1].uri, "usb://Brother/HL-2030");
    }

    #[test]
    fn devices_come_back_in_index_order() {
        // The dictionary has no ordering of its own.
        let devices = decode_devices(raw(&[
            ("device-uri:2", "c"),
            ("device-uri:0", "a"),
            ("device-uri:1", "b"),
        ]));
        assert_eq!(
            devices.iter().map(|d| d.uri.as_str()).collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
    }

    #[test]
    fn a_missing_field_becomes_empty_not_a_dropped_device() {
        let devices = decode_devices(raw(&[("device-uri:0", "usb://x/y")]));
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].uri, "usb://x/y");
        assert_eq!(devices[0].info, "");
        assert_eq!(devices[0].location, "");
    }

    #[test]
    fn an_entry_with_no_uri_is_dropped() {
        // A device we cannot address is useless to the caller.
        let devices = decode_devices(raw(&[("device-info:0", "Ghost")]));
        assert!(devices.is_empty());
    }

    #[test]
    fn a_malformed_key_is_ignored_rather_than_panicking() {
        let devices = decode_devices(raw(&[
            ("device-uri", "no-index"),
            ("device-uri:notanumber", "bad-index"),
            ("device-uri:0", "good"),
        ]));
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].uri, "good");
    }

    #[test]
    fn an_empty_reply_yields_no_devices() {
        assert!(decode_devices(HashMap::new()).is_empty());
    }

    #[test]
    fn network_devices_are_distinguished_from_local_ones() {
        let network = Device {
            class: "network".into(),
            uri: "ipp://x".into(),
            ..Device::default()
        };
        let direct = Device {
            class: "direct".into(),
            uri: "usb://x".into(),
            ..Device::default()
        };
        assert!(network.is_network());
        assert!(!direct.is_network());
    }
}
