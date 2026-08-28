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

/// Proves the whole authorised path end to end, without depending on what
/// hardware is present.
///
/// CUPS rejects the bogus device, which is the point: reaching a *mechanism*
/// error means polkit authorised the call, the mechanism ran it, CUPS answered,
/// and the reply was translated. An `AuthorizationFailed` or `Transport` here
/// would mean the path is broken rather than the device.
#[tokio::test]
#[ignore = "requires cups-pk-helper and raises a polkit prompt"]
async fn an_authorised_call_reaches_cups_and_its_error_is_translated() {
    if !admin_opted_in() {
        eprintln!("skipping: set COSMIC_PRINTERS_LIVE_ADMIN=1 to run administrative tests");
        return;
    }

    let client = CupsPk::connect().await.unwrap();
    let name = format!("cups-pk-client-bogus-{}", std::process::id());
    let spec = PrinterSpec::driverless(&name, "ipp://localhost:1/nonexistent");

    match client.printer_add(&spec).await {
        Err(cups_pk_client::CupsPkError::Mechanism(msg)) => {
            assert!(!msg.is_empty(), "CUPS should say why it refused");
        }
        Err(other) => panic!("the authorised path is broken, not the device: {other}"),
        Ok(()) => {
            // Astonishing, but tidy up rather than leave a queue behind.
            let _ = client.printer_delete(&name).await;
            panic!("CUPS accepted a printer at ipp://localhost:1/nonexistent");
        }
    }
}

/// Adds and removes a real queue, using a device discovery actually found.
///
/// `PrinterSpec::driverless` sets `ppd` to `"everywhere"`, which only works for
/// a printer that speaks IPP Everywhere — a raw `socket://` device fails with
/// `server-error-internal-error`. So this test uses a discovered IPP device and
/// skips when there is none, rather than inventing a URI.
#[tokio::test]
#[ignore = "creates and deletes a real print queue"]
async fn a_scratch_queue_can_be_added_and_removed() {
    if !admin_opted_in() {
        eprintln!("skipping: set COSMIC_PRINTERS_LIVE_ADMIN=1 to run administrative tests");
        return;
    }

    let client = CupsPk::connect().await.unwrap();
    let devices = client
        .devices_get(std::time::Duration::from_secs(10), 0)
        .await
        .expect("discovery");

    let Some(device) = devices
        .iter()
        .find(|d| d.uri.starts_with("ipp://") || d.uri.starts_with("ipps://"))
    else {
        eprintln!("skipping: discovery found no IPP device to add driverlessly");
        return;
    };

    let name = format!("cups-pk-client-scratch-{}", std::process::id());
    let spec = PrinterSpec::driverless(&name, &device.uri);

    client
        .printer_add(&spec)
        .await
        .expect("add the scratch queue");
    let removed = client.printer_delete(&name).await;

    // Remove before asserting, so a failed assertion cannot leave the queue behind.
    removed.expect("remove the scratch queue");
}
