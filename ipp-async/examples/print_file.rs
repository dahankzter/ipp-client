// SPDX-License-Identifier: MIT OR Apache-2.0

//! Sends a file to a printer, streaming it rather than reading it into memory.
//!
//! ```sh
//! cargo run --example print_file -- report.pdf                        # default queue
//! cargo run --example print_file -- report.pdf Office-Laser           # a named queue
//! cargo run --example print_file -- report.pdf ipp://printer.local/ipp/print
//! ```

#[tokio::main]
async fn main() -> ipp_async::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .expect("usage: print_file <file> [queue-or-uri]");
    let target = args.next();

    let client = ipp_async::IppClient::local()?;

    // A URI addresses any IPP printer; a bare name is a queue on the daemon.
    let printer = match target.as_deref() {
        Some(t) if t.contains("://") => client.at(t)?,
        Some(name) => client.queue(name)?,
        None => {
            let default = client
                .default_printer()
                .await
                .expect("no default printer configured");
            client.queue(&default)?
        }
    };

    let job = printer.print_file(std::path::Path::new(&path)).await?;
    println!("submitted as job {job}");
    Ok(())
}
