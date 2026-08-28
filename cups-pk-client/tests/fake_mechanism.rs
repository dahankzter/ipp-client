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

    async fn devices_get(
        &self,
        _timeout: i32,
        _limit: i32,
        _include: Vec<String>,
        _exclude: Vec<String>,
    ) -> (String, HashMap<String, String>) {
        (String::new(), HashMap::new())
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
        .serve_at("/org/opensuse/CupsPkHelper/Mechanism", FakeMechanism)
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
