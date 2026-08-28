# cups-client

Rust libraries for talking to CUPS.

| Crate | What it is |
|---|---|
| [`cups-client`](cups-client) | Async, pure-Rust IPP client. Printers, jobs, supplies, drivers, printing. |
| [`cups-pk-client`](cups-pk-client) | Async client for the `cups-pk-helper` D-Bus mechanism, for administration through polkit. |

Neither links `libcups`. `cups-client` speaks IPP over HTTP directly, and
`cups-pk-client` speaks D-Bus, so there is no C toolchain, no bindgen and no
`libcups` ABI to track. Both are `MIT OR Apache-2.0`.

They were extracted from [COSMIC Printing](https://github.com/dahankzter/cosmic-printing),
which remains their main consumer, but neither depends on COSMIC or on
`libcosmic`.

## Tests

```sh
just test      # unit tests, no daemon needed
just test-all  # adds the live tests, which need a running cupsd
just check     # everything CI checks
```

The live tests are `#[ignore]`d because they need a real `cupsd`. The one that
submits a page needs a second opt-in through `COSMIC_PRINTERS_LIVE_PRINT=1`.

## Licence

MIT OR Apache-2.0, at your option.
