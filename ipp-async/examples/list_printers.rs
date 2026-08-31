// SPDX-License-Identifier: MIT OR Apache-2.0

//! What the local print daemon knows about.
//!
//! ```sh
//! cargo run --example list_printers
//! ```

#[tokio::main]
async fn main() -> ipp_async::Result<()> {
    let client = ipp_async::IppClient::local()?;

    let default = client.default_printer().await;
    for printer in client.printers().await? {
        let marker = if Some(&printer.name) == default.as_ref() {
            " (default)"
        } else {
            ""
        };
        println!("{}{marker}", printer.name);
        println!("  state: {:?}", printer.state);

        for reason in &printer.reasons {
            println!("  {:?}: {}", reason.severity, reason.keyword);
        }
        for supply in &printer.supplies {
            println!("  {}: {:?}", supply.name, supply.level);
        }
    }
    Ok(())
}
