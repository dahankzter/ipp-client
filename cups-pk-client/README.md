# cups-pk-client

Async Rust client for the [`cups-pk-helper`] D-Bus mechanism.

CUPS administration — adding printers, enabling queues, setting the system
default — requires authorisation. `cups-pk-helper` performs those operations
behind polkit, so the desktop's own authentication agent prompts the user and no
password ever reaches your process. This crate is a typed, async binding to it.

```rust,no_run
use cups_pk_client::{CupsPk, PrinterSpec};

# async fn example() -> cups_pk_client::Result<()> {
let cups = CupsPk::connect().await?;

// Driverless: no PPD, no downloaded driver.
let spec = PrinterSpec::driverless("Office", "ipp://printer.local/ipp/print");
cups.printer_add(&spec).await?;
cups.printer_set_default("Office").await?;
# Ok(())
# }
```

## Why not HTTP Basic to `localhost:631`

Talking IPP directly means collecting the user's password yourself and holding it
in your process. Going through `cups-pk-helper` means polkit performs the check,
the desktop shows its native prompt, and the privileged code is a component the
distribution already ships and maintains.

## The error convention

The mechanism raises no D-Bus errors for operation failures. Every method returns
an `error` string which is empty on success. This crate translates that at its
boundary, so a failed operation is an `Err` rather than a silent success — the
single most important thing a binding to this interface has to get right.

Authorisation failure is a distinct variant. A user dismissing the polkit dialog
has made a decision, not hit a fault, and a UI needs to tell those apart:

```rust,no_run
# use cups_pk_client::{CupsPk, CupsPkError};
# async fn example(cups: &CupsPk) {
match cups.printer_set_default("Office").await {
    Ok(()) => {}
    Err(CupsPkError::AuthorizationFailed) => { /* the user declined; say nothing */ }
    Err(e) => eprintln!("{e}"),
}
# }
```

## Notes from the wire

Details that cost time to establish, recorded so you do not have to:

- The mechanism registers its object at the bus **root**, `/`. Its `main.c`
  defines `CPH_PATH "/"`. The conventional
  `/org/opensuse/CupsPkHelper/Mechanism` path does not exist.
- polkit refusals arrive as `org.opensuse.CupsPkHelper.Mechanism.NotPrivileged`,
  not the generic `AccessDenied`.
- `DevicesGet` returns a *flat* `a{ss}` whose keys carry an index suffix
  (`device-uri:0`, `device-class:0`, …). This crate groups them for you.
- Discovery can take considerably longer than the timeout you pass; treat that
  argument as a hint to CUPS, not a bound.
- `JobCancel` is annotated deprecated in the interface; use `job_cancel_purge`.

## Requirements

`cups-pk-helper` must be installed. It is packaged in over 100 distribution
repositories, and `gnome-control-center` depends on it. The service is D-Bus
activated, so there is nothing to start.

```sh
sudo pacman -S cups-pk-helper     # Arch, CachyOS
sudo apt install cups-pk-helper   # Debian, Ubuntu
sudo dnf install cups-pk-helper   # Fedora
```

## Testing

`cargo test` runs against an in-process fake serving the same interface, so it
needs a session bus but no printing system.

`cargo test -- --ignored` additionally exercises the real mechanism. Tests that
create or delete a print queue require `COSMIC_PRINTERS_LIVE_ADMIN=1` on top of
that, and operate only on a uniquely-named scratch queue they remove afterwards.

## Licence

MIT OR Apache-2.0.

[`cups-pk-helper`]: https://www.freedesktop.org/software/cups-pk-helper/releases/
