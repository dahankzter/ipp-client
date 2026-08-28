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
