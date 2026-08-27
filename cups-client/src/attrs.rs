// SPDX-License-Identifier: GPL-3.0-only

use ipp::prelude::*;

use crate::{Error, Result};

/// Read-only view over one IPP attribute group.
pub(crate) struct Attrs<'a> {
    group: &'a IppAttributeGroup,
}

impl<'a> Attrs<'a> {
    pub(crate) fn new(group: &'a IppAttributeGroup) -> Self {
        Attrs { group }
    }

    fn value(&self, name: &str) -> Option<&IppValue> {
        self.group.get(name).map(|a| a.value())
    }

    fn as_text(value: &IppValue) -> Option<String> {
        match value {
            IppValue::TextWithoutLanguage(v) => Some(v.to_string()),
            IppValue::NameWithoutLanguage(v) => Some(v.to_string()),
            IppValue::TextWithLanguage { text, .. } => Some(text.to_string()),
            IppValue::NameWithLanguage { name, .. } => Some(name.to_string()),
            IppValue::Keyword(v) => Some(v.to_string()),
            IppValue::Uri(v) => Some(v.to_string()),
            IppValue::MimeMediaType(v) => Some(v.to_string()),
            _ => None,
        }
    }

    fn as_int(value: &IppValue) -> Option<i32> {
        match value {
            IppValue::Integer(v) | IppValue::Enum(v) => Some(*v),
            _ => None,
        }
    }

    pub(crate) fn text(&self, name: &str) -> Option<String> {
        self.value(name).and_then(Self::as_text)
    }

    pub(crate) fn int(&self, name: &str) -> Option<i32> {
        self.value(name).and_then(Self::as_int)
    }

    pub(crate) fn bool(&self, name: &str) -> Option<bool> {
        match self.value(name)? {
            IppValue::Boolean(v) => Some(*v),
            _ => None,
        }
    }

    /// Flattens `Array` values; a scalar reads as a one-element list.
    pub(crate) fn texts(&self, name: &str) -> Vec<String> {
        match self.value(name) {
            Some(IppValue::Array(items)) => items.iter().filter_map(Self::as_text).collect(),
            Some(other) => Self::as_text(other).into_iter().collect(),
            None => Vec::new(),
        }
    }

    pub(crate) fn ints(&self, name: &str) -> Vec<i32> {
        match self.value(name) {
            Some(IppValue::Array(items)) => items.iter().filter_map(Self::as_int).collect(),
            Some(other) => Self::as_int(other).into_iter().collect(),
            None => Vec::new(),
        }
    }

    pub(crate) fn require_text(&self, name: &str) -> Result<String> {
        self.text(name)
            .ok_or_else(|| Error::decode(name, "missing or not a text value"))
    }

    pub(crate) fn require_int(&self, name: &str) -> Result<i32> {
        self.int(name)
            .ok_or_else(|| Error::decode(name, "missing or not an integer value"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(attrs: Vec<(&str, IppValue)>) -> IppAttributeGroup {
        let mut g = IppAttributeGroup::new(DelimiterTag::PrinterAttributes);
        for (name, value) in attrs {
            g.attributes_mut()
                .push(IppAttribute::with_name(name, value).unwrap());
        }
        g
    }

    #[test]
    fn reads_single_text_and_int_values() {
        let g = group(vec![
            ("printer-name", IppValue::NameWithoutLanguage("HP".try_into().unwrap())),
            ("printer-state", IppValue::Enum(3)),
        ]);
        let a = Attrs::new(&g);
        assert_eq!(a.text("printer-name").as_deref(), Some("HP"));
        assert_eq!(a.int("printer-state"), Some(3));
        assert_eq!(a.text("missing"), None);
    }

    #[test]
    fn flattens_arrays_into_lists() {
        let g = group(vec![(
            "printer-state-reasons",
            IppValue::Array(vec![
                IppValue::Keyword("toner-low".try_into().unwrap()),
                IppValue::Keyword("cover-open".try_into().unwrap()),
            ]),
        )]);
        let a = Attrs::new(&g);
        assert_eq!(a.texts("printer-state-reasons"), vec!["toner-low", "cover-open"]);
    }

    #[test]
    fn a_single_value_reads_as_a_one_element_list() {
        let g = group(vec![(
            "printer-state-reasons",
            IppValue::Keyword("none".try_into().unwrap()),
        )]);
        let a = Attrs::new(&g);
        assert_eq!(a.texts("printer-state-reasons"), vec!["none"]);
    }

    #[test]
    fn require_names_the_missing_attribute() {
        let g = group(vec![]);
        let a = Attrs::new(&g);
        let err = a.require_text("printer-name").unwrap_err();
        assert!(err.to_string().contains("printer-name"));
    }
}
