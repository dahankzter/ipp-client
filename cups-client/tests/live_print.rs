// SPDX-License-Identifier: MIT OR Apache-2.0

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

#[tokio::test]
#[ignore = "prints a real page"]
async fn several_documents_print_as_one_job() {
    // Gated like the other printing test: CUPS does support multi-document
    // jobs, unlike ippeveprinter, so this is where that path can actually be
    // exercised - at the cost of paper.
    if std::env::var("COSMIC_PRINTERS_LIVE_PRINT").as_deref() != Ok("1") {
        eprintln!("skipping: set COSMIC_PRINTERS_LIVE_PRINT=1 to print real pages");
        return;
    }

    let client = CupsClient::local().unwrap();
    let printers = client.printers().await.unwrap();
    let target = printers
        .iter()
        .find(|p| p.is_default)
        .or_else(|| printers.first())
        .expect("a configured printer");
    let printer = client.queue(&target.name).unwrap();

    let page = std::fs::read("/usr/share/cups/data/testprint").unwrap();
    let mut job = printer.create_job("two-part").await.expect("create");
    job.add_document(
        std::io::Cursor::new(page.clone()),
        Some("text/plain"),
        false,
    )
    .await
    .expect("first document");
    job.add_document(std::io::Cursor::new(page), Some("text/plain"), true)
        .await
        .expect("second document");

    let id = job.close().await.expect("close");
    assert!(id > 0);
}
