// SPDX-License-Identifier: MIT OR Apache-2.0

//! What the crate itself costs, as opposed to what the network costs.
//!
//! A real `CUPS-Get-Printers` response, captured from a live daemon, parsed
//! and decoded into the crate's own types. This is the part of a call that is
//! this crate's responsibility; the round trip dominates everything measured
//! here and is not this crate's to make faster.

use criterion::{Criterion, criterion_group, criterion_main};
use ipp::parser::IppParser;
use ipp::prelude::*;

const CAPTURE: &[u8] = include_bytes!("../testdata/cups-get-printers.bin");

fn parse() -> IppRequestResponse {
    IppParser::new(std::io::Cursor::new(CAPTURE.to_vec()))
        .parse()
        .expect("the captured response parses")
}

fn benches(c: &mut Criterion) {
    c.bench_function("parse a captured CUPS-Get-Printers response", |b| {
        b.iter(|| std::hint::black_box(parse()))
    });

    let response = parse();
    c.bench_function("decode every printer in that response", |b| {
        b.iter(|| {
            let decoded: Vec<_> = response
                .attributes()
                .groups_of(DelimiterTag::PrinterAttributes)
                .filter_map(|g| cups_client::Printer::decode(g).ok())
                .collect();
            std::hint::black_box(decoded)
        })
    });
}

criterion_group!(decode, benches);
criterion_main!(decode);
