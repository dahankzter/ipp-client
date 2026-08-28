// SPDX-License-Identifier: MIT OR Apache-2.0

//! Raw wire signatures. Every method returns the mechanism's `error` string
//! unchanged; [`crate::client`] is where it becomes a `Result`.
//!
//! `zbus` maps these snake_case names to their CamelCase D-Bus counterparts
//! automatically, so `printer_set_default` reaches `PrinterSetDefault`.

use std::collections::HashMap;

// The mechanism registers its object at the bus root. Its own main.c defines
// `CPH_PATH "/"`; the conventional /org/opensuse/CupsPkHelper/Mechanism path
// does not exist and every call against it fails with UnknownMethod.
#[zbus::proxy(interface = "org.opensuse.CupsPkHelper.Mechanism", default_path = "/")]
pub(crate) trait Mechanism {
    async fn printer_add(
        &self,
        name: &str,
        uri: &str,
        ppd: &str,
        info: &str,
        location: &str,
    ) -> zbus::Result<String>;

    async fn printer_delete(&self, name: &str) -> zbus::Result<String>;
    async fn printer_rename(&self, old_name: &str, new_name: &str) -> zbus::Result<String>;
    async fn printer_set_default(&self, name: &str) -> zbus::Result<String>;
    async fn printer_set_enabled(&self, name: &str, enabled: bool) -> zbus::Result<String>;
    async fn printer_set_accept_jobs(
        &self,
        name: &str,
        enabled: bool,
        reason: &str,
    ) -> zbus::Result<String>;
    async fn printer_set_info(&self, name: &str, info: &str) -> zbus::Result<String>;
    async fn printer_set_location(&self, name: &str, location: &str) -> zbus::Result<String>;
    async fn printer_add_option_default(
        &self,
        name: &str,
        option: &str,
        values: Vec<String>,
    ) -> zbus::Result<String>;

    async fn devices_get(
        &self,
        timeout: i32,
        limit: i32,
        include_schemes: Vec<String>,
        exclude_schemes: Vec<String>,
    ) -> zbus::Result<(String, HashMap<String, String>)>;

    async fn job_cancel_purge(&self, jobid: i32, purge: bool) -> zbus::Result<String>;
}
