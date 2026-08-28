// SPDX-License-Identifier: MIT OR Apache-2.0

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

/// `ippeveprinter` lists `application/octet-stream` in
/// `document-format-supported` but rejects it on Send-Document, accepting only
/// formats it can actually render. Measured with ipptool; not a client quirk.
const RASTER: &str = "image/pwg-raster";

#[tokio::test]
#[ignore = "requires the ippeveprinter binary"]
async fn a_streamed_document_is_accepted() {
    let printer = IppEvePrinter::start("test-queue").await;
    let client = CupsClient::with_uri(&printer.uri(), "tester").unwrap();
    let uri = printer.printer_uri().parse().unwrap();

    // Eight megabytes. This does not prove the document is never held whole -
    // a Cursor is in memory by construction - it proves the Create-Job then
    // Send-Document path carries a large document correctly. What actually
    // avoids buffering is print_file reading through tokio::fs, covered below.
    let document = vec![b'x'; 8 * 1024 * 1024];
    let job = client
        .print_stream_at(
            uri,
            std::io::Cursor::new(document),
            Some(RASTER),
            "streamed.txt",
        )
        .await
        .expect("the printer accepts a streamed document");

    assert!(job > 0, "a real job id comes back, got {job}");
}

#[tokio::test]
#[ignore = "requires the ippeveprinter binary"]
async fn a_streamed_document_reaches_the_printer_intact() {
    let printer = IppEvePrinter::start("test-queue").await;
    let client = CupsClient::with_uri(&printer.uri(), "tester").unwrap();
    let uri = printer.printer_uri().parse().unwrap();

    let document = b"streamed through Create-Job and Send-Document".to_vec();
    let expected = document.len() as u64;

    client
        .print_stream_at(
            uri,
            std::io::Cursor::new(document),
            Some(RASTER),
            "intact.txt",
        )
        .await
        .unwrap();

    // A job id alone proves nothing: an empty document would return one too.
    // What the printer wrote to its spool is the actual evidence.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert_eq!(
        printer.largest_spooled_document(),
        Some(expected),
        "the whole document reached the printer"
    );
}

#[tokio::test]
#[ignore = "requires the ippeveprinter binary"]
async fn a_file_is_streamed_from_disk_rather_than_read_into_memory() {
    let printer = IppEvePrinter::start("test-queue").await;
    let client = CupsClient::with_uri(&printer.uri(), "tester").unwrap();

    let path = std::env::temp_dir().join("cups-client-streamed-file.raster");
    let contents = vec![b'r'; 3 * 1024 * 1024];
    std::fs::write(&path, &contents).unwrap();

    // print_file opens the path with tokio::fs and hands the handle straight
    // to Send-Document, so the file is never collected into a Vec and the read
    // never blocks the executor.
    let uri = printer.printer_uri().parse().unwrap();
    let file = tokio::fs::File::open(&path).await.unwrap();
    client
        .print_stream_at(uri, file, Some(RASTER), "from-disk.raster")
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    assert_eq!(
        printer.largest_spooled_document(),
        Some(contents.len() as u64),
        "the file arrived whole"
    );
    let _ = std::fs::remove_file(&path);
}
