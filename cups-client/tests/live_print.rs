// SPDX-License-Identifier: GPL-3.0-only

//! Submits a real print job.
//!
//! Gated twice — `#[ignore]` and `COSMIC_PRINTERS_LIVE_PRINT=1` — because this
//! consumes paper on whatever printer is default.

use cups_client::CupsClient;

#[tokio::test]
#[ignore = "prints a real page"]
async fn the_cups_test_page_can_be_submitted() {
    if std::env::var("COSMIC_PRINTERS_LIVE_PRINT").as_deref() != Ok("1") {
        eprintln!("skipping: set COSMIC_PRINTERS_LIVE_PRINT=1 to print a real page");
        return;
    }

    let client = CupsClient::local().unwrap();
    let printers = client.printers().await.unwrap();
    let target = printers
        .iter()
        .find(|p| p.is_default)
        .or_else(|| printers.first())
        .expect("a configured printer");

    let job = client
        .print_file(
            &target.name,
            std::path::Path::new("/usr/share/cups/data/testprint"),
        )
        .await
        .expect("submit the test page");

    assert!(job > 0, "CUPS should assign a positive job id");
}
