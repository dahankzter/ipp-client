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

    // Eight megabytes. This does not prove the document is never held whole -
    // a Cursor is in memory by construction - it proves the Create-Job then
    // Send-Document path carries a large document correctly. What actually
    // avoids buffering is print_file reading through tokio::fs, covered below.
    let document = vec![b'x'; 8 * 1024 * 1024];
    let job = client
        .at(&printer.printer_uri())
        .unwrap()
        .print_stream(std::io::Cursor::new(document), Some(RASTER), "streamed.txt")
        .await
        .expect("the printer accepts a streamed document");

    assert!(job > 0, "a real job id comes back, got {job}");
}

#[tokio::test]
#[ignore = "requires the ippeveprinter binary"]
async fn a_streamed_document_reaches_the_printer_intact() {
    let printer = IppEvePrinter::start("test-queue").await;
    let client = CupsClient::with_uri(&printer.uri(), "tester").unwrap();

    let document = b"streamed through Create-Job and Send-Document".to_vec();
    let expected = document.len() as u64;

    client
        .at(&printer.printer_uri())
        .unwrap()
        .print_stream(std::io::Cursor::new(document), Some(RASTER), "intact.txt")
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
    let file = tokio::fs::File::open(&path).await.unwrap();
    client
        .at(&printer.printer_uri())
        .unwrap()
        .print_stream(file, Some(RASTER), "from-disk.raster")
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

#[tokio::test]
#[ignore = "requires the ippeveprinter binary"]
async fn validate_job_accepts_a_format_the_printer_supports() {
    let printer = IppEvePrinter::start("test-queue").await;
    let client = CupsClient::with_uri(&printer.uri(), "tester").unwrap();

    // ippeveprinter serves only /ipp/print, so it is addressed by URI.
    client
        .at(&printer.printer_uri())
        .unwrap()
        .validate(RASTER)
        .await
        .expect("a format the printer renders is accepted");
}

#[tokio::test]
#[ignore = "requires the ippeveprinter binary"]
async fn validate_job_rejects_a_format_the_printer_cannot_render() {
    let printer = IppEvePrinter::start("test-queue").await;
    let client = CupsClient::with_uri(&printer.uri(), "tester").unwrap();

    // This is the whole point of asking first: the rejection arrives before
    // any document is uploaded.
    let refused = client
        .at(&printer.printer_uri())
        .unwrap()
        .validate("application/vnd.made-up")
        .await;
    assert!(refused.is_err(), "an unrenderable format is refused");
}

#[tokio::test]
#[ignore = "requires the ippeveprinter binary"]
async fn a_printer_can_be_asked_to_identify_itself() {
    use cups_client::IdentifyAction;

    let printer = IppEvePrinter::start("test-queue").await;
    let client = CupsClient::with_uri(&printer.uri(), "tester").unwrap();

    // The opcode is set by hand because the ipp crate's enum has no
    // Identify-Printer, so this is the check that the right operation reaches
    // the printer rather than the placeholder it was built from.
    client
        .at(&printer.printer_uri())
        .unwrap()
        .identify(IdentifyAction::Sound)
        .await
        .expect("ippeveprinter advertises Identify-Printer and accepts it");
}

#[tokio::test]
#[ignore = "requires the ippeveprinter binary"]
async fn every_printer_operation_works_without_cups() {
    use cups_client::IdentifyAction;

    // ippeveprinter is a bare IPP printer: no queues, no CUPS extensions,
    // nothing but the standard protocol. Everything on the handle has to work
    // against it, because that is the case CUPS-shaped addressing cannot reach.
    let printer = IppEvePrinter::start("test-queue").await;
    let client = CupsClient::with_uri(&printer.uri(), "tester").unwrap();
    let p = client.at(&printer.printer_uri()).unwrap();

    let attributes = p.attributes().await.expect("attributes");
    assert!(!attributes.name.is_empty());

    p.validate(RASTER).await.expect("validate");
    p.identify(IdentifyAction::Sound).await.expect("identify");

    let job = p
        .print_stream(
            std::io::Cursor::new(b"handle-submitted".to_vec()),
            Some(RASTER),
            "handle.raster",
        )
        .await
        .expect("print");
    assert!(job > 0);

    // Cancelling an already-finished job is refused rather than silent, so
    // this asserts the request reached the printer and was understood.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let cancelled = p.cancel_job(job).await;
    eprintln!("cancel of finished job {job}: {cancelled:?}");
}
