// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tests against an in-process fake serving the same interface.
//!
//! The real mechanism runs as root and performs destructive operations, so the
//! contract — especially the empty-string error convention — is verified here
//! rather than against a live system.

use std::collections::HashMap;

pub struct FakeMechanism;

#[zbus::interface(name = "org.opensuse.CupsPkHelper.Mechanism")]
impl FakeMechanism {
    async fn printer_set_default(&self, name: &str) -> String {
        if name == "good" {
            String::new()
        } else {
            format!("\"{name}\" is not a valid printer name.")
        }
    }

    async fn printer_set_enabled(&self, name: &str, _enabled: bool) -> String {
        if name == "good" { String::new() } else { format!("\"{name}\" is not a valid printer name.") }
    }

    async fn printer_set_accept_jobs(&self, name: &str, _enabled: bool, _reason: &str) -> String {
        if name == "good" { String::new() } else { format!("\"{name}\" is not a valid printer name.") }
    }

    async fn printer_set_info(&self, name: &str, _info: &str) -> String {
        if name == "good" { String::new() } else { format!("\"{name}\" is not a valid printer name.") }
    }

    async fn printer_set_location(&self, name: &str, _location: &str) -> String {
        if name == "good" { String::new() } else { format!("\"{name}\" is not a valid printer name.") }
    }

    async fn printer_add(&self, name: &str, _uri: &str, _ppd: &str, _info: &str, _location: &str) -> String {
        if name == "good" { String::new() } else { format!("\"{name}\" is not a valid printer name.") }
    }

    async fn printer_delete(&self, name: &str) -> String {
        if name == "good" { String::new() } else { format!("\"{name}\" is not a valid printer name.") }
    }

    async fn printer_rename(&self, old_name: &str, _new_name: &str) -> String {
        if old_name == "good" { String::new() } else { format!("\"{old_name}\" is not a valid printer name.") }
    }

    async fn printer_add_option_default(&self, name: &str, _option: &str, _values: Vec<String>) -> String {
        if name == "good" { String::new() } else { format!("\"{name}\" is not a valid printer name.") }
    }

    async fn job_cancel_purge(&self, jobid: i32, _purge: bool) -> String {
        if jobid > 0 { String::new() } else { format!("\"{jobid}\" is not a valid job id.") }
    }

    async fn devices_get(
        &self,
        _timeout: i32,
        _limit: i32,
        _include: Vec<String>,
        _exclude: Vec<String>,
    ) -> (String, HashMap<String, String>) {
        let mut devices = HashMap::new();
        for (k, v) in [
            ("device-uri:0", "ipp://printer.local/ipp/print"),
            ("device-class:0", "network"),
            ("device-info:0", "Office Printer"),
            ("device-uri:1", "usb://Brother/HL-2030"),
            ("device-class:1", "direct"),
        ] {
            devices.insert(k.to_string(), v.to_string());
        }
        (String::new(), devices)
    }
}

/// Serves the fake under a unique bus name and returns it with that name.
/// The connection must be held for the lifetime of the test.
async fn serve(tag: &str) -> (zbus::Connection, String) {
    let name = format!("org.example.FakeCupsPk{tag}");
    let conn = zbus::connection::Builder::session()
        .expect("a session bus is required for these tests")
        .name(name.clone())
        .unwrap()
        .serve_at("/", FakeMechanism)
        .unwrap()
        .build()
        .await
        .unwrap();
    (conn, name)
}

#[tokio::test]
async fn a_successful_call_returns_ok() {
    let (_held, name) = serve("Ok").await;
    let client = cups_pk_client::CupsPk::connect_to(&name).await.unwrap();
    assert!(client.printer_set_default("good").await.is_ok());
}

#[tokio::test]
async fn a_failing_call_surfaces_the_mechanisms_message() {
    let (_held, name) = serve("Err").await;
    let client = cups_pk_client::CupsPk::connect_to(&name).await.unwrap();

    let err = client.printer_set_default("nope").await.unwrap_err();
    assert!(err.to_string().contains("not a valid printer name"));
}

#[tokio::test]
async fn discovery_decodes_the_indexed_reply() {
    let (_held, name) = serve("Devices").await;
    let client = cups_pk_client::CupsPk::connect_to(&name).await.unwrap();

    let devices = client
        .devices_get(std::time::Duration::from_secs(5), 0)
        .await
        .unwrap();

    assert_eq!(devices.len(), 2);
    assert!(devices.iter().any(|d| d.is_network()));
    assert!(devices.iter().any(|d| !d.is_network()));
}

#[tokio::test]
async fn every_state_setter_translates_success() {
    let (_held, name) = serve("StateOk").await;
    let c = cups_pk_client::CupsPk::connect_to(&name).await.unwrap();

    assert!(c.printer_set_enabled("good", true).await.is_ok());
    assert!(c.printer_set_accept_jobs("good", true, "").await.is_ok());
    assert!(c.printer_set_info("good", "Front desk").await.is_ok());
    assert!(c.printer_set_location("good", "Level 2").await.is_ok());
}

#[tokio::test]
async fn every_state_setter_translates_failure() {
    // The convention is per-method: one forgotten translate() is a silent
    // success on a failed operation, so each is checked individually.
    let (_held, name) = serve("StateErr").await;
    let c = cups_pk_client::CupsPk::connect_to(&name).await.unwrap();

    assert!(c.printer_set_enabled("nope", true).await.is_err());
    assert!(c.printer_set_accept_jobs("nope", true, "").await.is_err());
    assert!(c.printer_set_info("nope", "x").await.is_err());
    assert!(c.printer_set_location("nope", "x").await.is_err());
}

#[tokio::test]
async fn every_remaining_method_translates_success() {
    let (_held, name) = serve("RestOk").await;
    let c = cups_pk_client::CupsPk::connect_to(&name).await.unwrap();
    let spec = cups_pk_client::PrinterSpec::driverless("good", "ipp://x/ipp/print");

    assert!(c.printer_add(&spec).await.is_ok());
    assert!(c.printer_delete("good").await.is_ok());
    assert!(c.printer_rename("good", "better").await.is_ok());
    assert!(c.printer_add_option_default("good", "sides", &["two-sided-long-edge".into()]).await.is_ok());
    assert!(c.job_cancel_purge(42, false).await.is_ok());
}

#[tokio::test]
async fn every_remaining_method_translates_failure() {
    let (_held, name) = serve("RestErr").await;
    let c = cups_pk_client::CupsPk::connect_to(&name).await.unwrap();
    let spec = cups_pk_client::PrinterSpec::driverless("nope", "ipp://x/ipp/print");

    assert!(c.printer_add(&spec).await.is_err());
    assert!(c.printer_delete("nope").await.is_err());
    assert!(c.printer_rename("nope", "x").await.is_err());
    assert!(c.printer_add_option_default("nope", "sides", &["one-sided".into()]).await.is_err());
    assert!(c.job_cancel_purge(0, false).await.is_err());
}
