# cups-client

Rust libraries for talking to CUPS.

| Crate | What it is |
|---|---|
| [`cups-client`](cups-client) | Async, pure-Rust IPP client. Printers, jobs, supplies, drivers, printing. |
| [`cups-pk-client`](cups-pk-client) | Async client for the `cups-pk-helper` D-Bus mechanism, for administration through polkit. |

Neither links `libcups`. `cups-client` speaks IPP over HTTP directly and
`cups-pk-client` speaks D-Bus, so there is no `libcups` ABI to track, no
bindgen, and nothing to install before building. Both are `MIT OR Apache-2.0`.

## What "no C" means here, precisely

With `default-features = false`, `cups-client` builds with **no C at all**: no
`-sys` crate, no `cc`, no `cmake`. `libc` appears in the tree, but only as the
syscall declarations any Rust program doing I/O uses on Linux - nothing is
linked against a shared C library.

The default build adds one exception, and it is worth being exact about it.
TLS, which `ipps://` needs, comes from rustls, whose default cryptography
provider is `aws-lc-rs`. That vendors and compiles a BoringSSL fork, so `cc`
and `cmake` enter the build graph. Nothing is needed at runtime and there is no
`-dev` package to install, but a C compiler is needed to build.

So:

| Build | `ipps://` | C at build time | C at runtime |
|---|---|---|---|
| `default-features = false` | no | none | none |
| default (`tls`) | yes | `cc`, `cmake` for aws-lc-rs | none |

Pick the first if a fully C-free build matters more than TLS.

They were extracted from [COSMIC Printing](https://github.com/dahankzter/cosmic-printing),
which remains their main consumer, but neither depends on COSMIC or on
`libcosmic`.

## On speed

Measured on the development machine, against a live `cupsd`:

| | |
|---|---|
| `CUPS-Get-Printers` round trip | 20.7 ms |
| parsing that 15 KB response | 48.6 µs |
| decoding it into these types | 2.2 µs |

So this crate's own work is about a quarter of one percent of a call. Raw
parsing speed is not where a printing client is slow, and any claim that these
bindings are faster than another set at that would be noise.

Where the difference is real is in what an async program can do while a call is
in flight. `libcups` is blocking C, so an async caller has to hand every
operation to a blocking thread pool and hold a thread for the duration. This is
async to the socket, and documents are streamed rather than read into memory, so
printing a large file neither holds it whole nor blocks the executor while it is
read.

Not measured: any head-to-head against `libcups` bindings. That needs a harness
neither crate has, and the numbers above suggest it would mostly measure the
daemon.

`cargo bench -p cups-client` reproduces the two benchmarks; the round trip is
`cargo nextest run -p cups-client --run-ignored all -E 'test(round_trip_cost)'`.

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
