// SPDX-License-Identifier: MIT OR Apache-2.0

use futures::StreamExt;
use ipp_async::{IppClient, PrinterEvent};

#[tokio::test]
#[ignore = "requires a running cupsd"]
async fn the_event_stream_opens_with_a_resynchronisation() {
    let client = IppClient::local().unwrap();
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
    let client = IppClient::local().unwrap();
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
            ipp_async::MediaSize::parse(keyword).is_some(),
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
    use ipp_async::PpdFilter;

    let client = IppClient::local().unwrap();
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

#[tokio::test]
#[ignore = "requires a running cupsd"]
async fn classes_are_listed_without_error() {
    // Most systems have no classes configured, so this asserts the operation
    // succeeds and decodes rather than asserting a count: an unsupported
    // operation or a decode failure must not look like "no classes".
    let client = IppClient::local().unwrap();
    let classes = client.classes().await.expect("CUPS-Get-Classes succeeds");
    eprintln!("{} classes configured", classes.len());
    assert!(classes.iter().all(|c| !c.name.is_empty()));
}

#[cfg(feature = "tls")]
#[tokio::test]
#[ignore = "requires a running cupsd"]
async fn an_ipps_connection_needs_the_certificate_trusted() {
    // CUPS serves ipps on the same port and signs it with a self-signed
    // certificate, which is exactly what printers do. Verification must fail
    // by default, and the caller must be able to tell why.
    let strict = IppClient::builder("ipps://localhost:631").build().unwrap();
    let refused = strict.printers().await;

    match refused {
        Err(e) => assert!(
            e.is_certificate_error(),
            "a rejected certificate must be reported as one, got: {e}"
        ),
        // A machine whose CUPS certificate is in the trust store is not a
        // failure of this crate.
        Ok(_) => eprintln!("this daemon's certificate is already trusted"),
    }
}

#[cfg(feature = "tls")]
#[tokio::test]
#[ignore = "requires a running cupsd"]
async fn ipps_works_when_the_caller_accepts_the_certificate() {
    // The same connection succeeds once the caller opts out of verification,
    // which proves TLS itself works and that the only obstacle was trust.
    let client = IppClient::builder("ipps://localhost:631")
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();

    let printers = client.printers().await.expect("ipps works over TLS");
    eprintln!("{} printers over ipps://", printers.len());
}

#[tokio::test]
#[ignore = "requires a running cupsd"]
async fn a_queue_can_be_paused_and_resumed() {
    // Pause is administrative, so an unauthorised client is refused. Either
    // outcome proves the operation reached CUPS and was understood; what must
    // not happen is a silent success that changes nothing.
    let client = IppClient::local().unwrap();
    let printers = client.printers().await.unwrap();
    let Some(target) = printers.first() else {
        eprintln!("no queues configured");
        return;
    };
    let queue = client.queue(&target.name).unwrap();

    match queue.pause().await {
        Ok(()) => {
            let paused = client.printer(&target.name).await.unwrap();
            assert_eq!(
                paused.state,
                ipp_async::PrinterState::Stopped,
                "a paused queue reports itself stopped"
            );
            queue.resume().await.expect("resume");
        }
        Err(e) => eprintln!("pause refused, as an unauthorised caller should be: {e}"),
    }
}

#[tokio::test]
#[ignore = "requires a running cupsd; measures rather than asserts"]
async fn round_trip_cost_against_a_real_daemon() {
    // Context for the decode benchmarks: how much of a call is this crate's
    // work, and how much is the daemon and the socket. Printed rather than
    // asserted, because a timing threshold in a test suite is a flake waiting
    // to happen.
    let client = IppClient::local().unwrap();

    // Warm the connection so the first TCP setup is not counted.
    let _ = client.printers().await;

    let runs = 50;
    let start = std::time::Instant::now();
    for _ in 0..runs {
        client.printers().await.expect("printers");
    }
    let each = start.elapsed() / runs;

    eprintln!("CUPS-Get-Printers round trip: {each:?} each over {runs} runs");
    eprintln!("  of which parse+decode is about 51us, measured by benches/decode.rs");
}

#[tokio::test]
#[ignore = "requires a running cupsd"]
async fn a_subscription_can_be_created_collected_and_cancelled() {
    use ipp_async::NotifyEvent;

    let client = IppClient::local().unwrap();
    let printers = client.printers().await.unwrap();
    let Some(target) = printers.first() else {
        eprintln!("no queues configured");
        return;
    };
    let printer = client.queue(&target.name).unwrap();

    let sub = printer
        .subscribe(
            &[NotifyEvent::PrinterStateChanged, NotifyEvent::JobCreated],
            Some(std::time::Duration::from_secs(60)),
        )
        .await
        .expect("a subscription is created");
    eprintln!(
        "subscription {} events={:?} lease={:?}",
        sub.id, sub.events, sub.lease
    );
    assert!(sub.id > 0);

    // It must be findable among this user's subscriptions.
    let all = printer.subscriptions().await.expect("list");
    assert!(
        all.iter().any(|s| s.id == sub.id),
        "the new subscription is listed"
    );

    let one = printer.subscription(sub.id).await.expect("read one");
    assert_eq!(one.id, sub.id);

    // Nothing has happened yet, so this is about the call working rather than
    // the events. CUPS answers at once and says when to ask again.
    let notifications = printer.notifications(&[sub.id]).await.expect("collect");
    eprintln!(
        "  {} events waiting, poll again after {:?}",
        notifications.events.len(),
        notifications.poll_after
    );

    printer
        .renew_subscription(sub.id, Some(std::time::Duration::from_secs(120)))
        .await
        .expect("renew");
    printer.cancel_subscription(sub.id).await.expect("cancel");

    let after = printer.subscriptions().await.unwrap_or_default();
    assert!(
        !after.iter().any(|s| s.id == sub.id),
        "a cancelled subscription is gone"
    );
}

#[tokio::test]
#[ignore = "requires a running cupsd"]
async fn cups_understands_the_administrative_operations() {
    // Against CUPS these are authorised operations, so an unauthenticated
    // caller gets 401. That still proves the request reached the daemon and
    // was understood: an unknown operation or a malformed request comes back
    // differently.
    let client = IppClient::local().unwrap();
    let printers = client.printers().await.unwrap();
    let Some(target) = printers.first() else {
        eprintln!("no queues configured");
        return;
    };
    let printer = client.queue(&target.name).unwrap();

    for (name, result) in [
        ("disable", printer.disable().await),
        ("hold_new_jobs", printer.hold_new_jobs().await),
        ("cancel_all_jobs", printer.cancel_all_jobs().await),
    ] {
        match result {
            Ok(()) => eprintln!("  {name}: accepted (this client is authorised)"),
            Err(e) => {
                let text = e.to_string();
                assert!(
                    !text.contains("Invalid tag") && !text.contains("BadRequest"),
                    "{name} was malformed rather than refused: {text}"
                );
                eprintln!("  {name}: {text}");
            }
        }
    }

    // Cancelling one's own jobs needs no authorisation, so this one must work.
    printer
        .cancel_my_jobs()
        .await
        .expect("cancelling one's own jobs is always permitted");
}
