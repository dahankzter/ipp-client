# cups-client

An async, pure-Rust IPP client for CUPS.

No `libcups`, no bindgen, no C toolchain — it speaks the IPP protocol over HTTP
directly, so it cross-compiles and packages like any other Rust crate.

```rust,no_run
# async fn example() -> cups_client::Result<()> {
let client = cups_client::CupsClient::local()?;

for printer in client.printers().await? {
    println!("{} — {:?}", printer.name, printer.state);
}
# Ok(())
# }
```

## What it covers

- Printers: state, state reasons, supply levels, and the job option defaults a
  queue advertises (media, sides, colour mode, quality, output bin)
- Jobs: list, inspect, cancel, hold, release, and what became of finished ones
- Printing: submit a file or bytes to a queue
- Drivers: the PPDs CUPS offers, filtered server-side by device id or model
- An event stream that diffs successive polls into printer and job changes

## What it does not

Administrative operations — adding, removing or reconfiguring a queue — need
authorisation. Those live in [`cups-pk-client`](../cups-pk-client), which talks
to `cups-pk-helper` over D-Bus so polkit does the privilege check and no
privileged code ships here.

## Licence

MIT OR Apache-2.0.
