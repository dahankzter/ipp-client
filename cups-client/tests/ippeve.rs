// SPDX-License-Identifier: GPL-3.0-only

mod common;

use common::IppEvePrinter;
use cups_client::CupsClient;

#[tokio::test]
#[ignore = "requires the ippeveprinter binary"]
async fn get_printer_attributes_succeeds_against_a_plain_ipp_printer() {
    let printer = IppEvePrinter::start("test-queue").await;
    let client = CupsClient::with_uri(&printer.uri(), "tester").unwrap();

    let p = client.printer_at(&printer.printer_uri()).await.unwrap();
    assert!(!p.name.is_empty());
}

#[tokio::test]
#[ignore = "requires the ippeveprinter binary"]
async fn cups_get_printers_is_unsupported_on_a_plain_ipp_printer() {
    let printer = IppEvePrinter::start("test-queue").await;
    let client = CupsClient::with_uri(&printer.uri(), "tester").unwrap();

    // ippeveprinter has no CUPS extensions. This must surface as an error,
    // not a silent empty list.
    assert!(client.printers().await.is_err());
}
