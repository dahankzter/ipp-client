// SPDX-License-Identifier: MIT OR Apache-2.0

use cups_client::{CupsClient, PrinterEvent};
use futures::StreamExt;

#[tokio::test]
#[ignore = "requires a running cupsd"]
async fn the_event_stream_opens_with_a_resynchronisation() {
    let client = CupsClient::local().unwrap();
    let mut stream = Box::pin(client.events());

    let first = stream
        .next()
        .await
        .expect("stream yields")
        .expect("no error");
    match first {
        PrinterEvent::Resynchronised { printers, .. } => {
            assert!(printers.iter().all(|p| !p.name.is_empty()));
        }
        other => panic!("expected Resynchronised, got {other:?}"),
    }
}

#[tokio::test]
#[ignore = "requires a running cupsd with at least one queue"]
async fn a_real_queue_advertises_its_job_options() {
    // Guards the assumption the option decoding rests on: CUPS-Get-Printers
    // returns the *-supported and *-default attributes for every queue, so no
    // per-printer Get-Printer-Attributes is needed.
    let client = CupsClient::local().unwrap();
    let printers = client.printers().await.expect("printers");
    let Some(printer) = printers.first() else {
        eprintln!("no queues configured, nothing to assert");
        return;
    };

    let o = &printer.options;
    eprintln!(
        "{}: {} media, {} sides, {} colour, {} quality, {} bins",
        printer.name,
        o.media.supported.len(),
        o.sides.supported.len(),
        o.color_mode.supported.len(),
        o.quality.supported.len(),
        o.output_bin.supported.len(),
    );

    // Any IPP printer worth the name offers a page size and a default for it.
    assert!(!o.media.supported.is_empty(), "no media-supported");
    assert!(o.media.default.is_some(), "no media-default");
    // Every advertised size must be a parseable PWG name, since the window
    // pairs borderless variants by the dimensions parsed out of it.
    for keyword in &o.media.supported {
        assert!(
            cups_client::MediaSize::parse(keyword).is_some(),
            "unparseable media name: {keyword}"
        );
    }
}

#[tokio::test]
#[ignore = "requires a running cupsd with drivers installed"]
async fn a_device_id_narrows_the_driver_list() {
    // The whole add-printer design rests on cupsd doing this filtering: an
    // unfiltered list is ~2300 drivers, which is not choosable. Note lpinfo's
    // --device-id flag does NOT filter, only the IPP operation does.
    use cups_client::PpdFilter;

    let client = CupsClient::local().unwrap();
    let all = client.ppds(None).await.expect("unfiltered");
    if all.is_empty() {
        eprintln!("no drivers installed, nothing to compare against");
        return;
    }

    let matched = client
        .ppds(Some(PpdFilter::DeviceId("MFG:HP;MDL:OfficeJet Pro 8210;")))
        .await
        .expect("filtered");

    eprintln!(
        "{} drivers total, {} matching the device id",
        all.len(),
        matched.len()
    );
    assert!(
        matched.len() < all.len(),
        "the device id filter did not narrow anything: {} of {}",
        matched.len(),
        all.len()
    );
    // Every driver must carry the name a printer-add needs.
    assert!(all.iter().all(|p| !p.name.is_empty()));
}
