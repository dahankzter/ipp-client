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
