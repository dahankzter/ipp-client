// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tests against the real mechanism.
//!
//! Read-only tests need only `--ignored`. Administrative tests additionally
//! require `COSMIC_PRINTERS_LIVE_ADMIN=1`, because they create and delete a
//! real print queue and will raise a polkit prompt.

use cups_pk_client::{CupsPk, PrinterSpec};

fn admin_opted_in() -> bool {
    std::env::var("COSMIC_PRINTERS_LIVE_ADMIN").as_deref() == Ok("1")
}

#[tokio::test]
#[ignore = "requires cups-pk-helper on the system bus"]
async fn the_mechanism_is_reachable() {
    assert!(
        CupsPk::is_available().await,
        "cups-pk-helper should be installed and D-Bus activatable"
    );
}

#[tokio::test]
#[ignore = "requires cups-pk-helper on the system bus"]
async fn discovery_returns_addressable_devices() {
    let client = CupsPk::connect().await.unwrap();
    let devices = client
        .devices_get(std::time::Duration::from_secs(5), 0)
        .await
        .unwrap();

    // Discovery may legitimately find nothing; what must hold is that anything
    // it does find is addressable.
    assert!(devices.iter().all(|d| !d.uri.is_empty()));
}

#[tokio::test]
#[ignore = "creates and deletes a real print queue"]
async fn a_scratch_queue_can_be_added_and_removed() {
    if !admin_opted_in() {
        eprintln!("skipping: set COSMIC_PRINTERS_LIVE_ADMIN=1 to run administrative tests");
        return;
    }

    let client = CupsPk::connect().await.unwrap();
    let name = format!("cups-pk-client-scratch-{}", std::process::id());
    let spec = PrinterSpec::driverless(&name, "ipp://localhost:631/ipp/print");

    client.printer_add(&spec).await.expect("add the scratch queue");
    let removed = client.printer_delete(&name).await;

    // Remove before asserting, so a failed assertion cannot leave the queue behind.
    removed.expect("remove the scratch queue");
}
