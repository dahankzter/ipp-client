// SPDX-License-Identifier: MIT OR Apache-2.0
// Used only by the live tests, which are #[ignore]d by default.
#![allow(dead_code)]

use std::process::{Child, Command, Stdio};

/// A live `ippeveprinter` process on an ephemeral port, killed on drop.
pub struct IppEvePrinter {
    child: Child,
    port: u16,
    queue: String,
    spool: std::path::PathBuf,
}

impl IppEvePrinter {
    pub async fn start(queue: &str) -> IppEvePrinter {
        let port = free_port();
        // Spool to a directory of its own and keep the files, so a test can
        // check what actually arrived rather than trusting the job id.
        let spool = std::env::temp_dir().join(format!("cups-client-ippeve-{port}"));
        std::fs::create_dir_all(&spool).expect("can create a spool directory");

        let child = Command::new("ippeveprinter")
            .args([
                "-p",
                &port.to_string(),
                "-d",
                &spool.to_string_lossy(),
                "-k",
                queue,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("ippeveprinter must be installed to run this test");

        // ippeveprinter needs a moment before it accepts connections.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        IppEvePrinter {
            child,
            port,
            queue: queue.to_string(),
            spool,
        }
    }

    /// The largest document the printer has spooled, in bytes.
    ///
    /// `ippeveprinter` writes each received document into its spool directory,
    /// so this is what actually crossed the wire.
    pub fn largest_spooled_document(&self) -> Option<u64> {
        std::fs::read_dir(&self.spool)
            .ok()?
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.metadata().ok())
            .filter(|meta| meta.is_file())
            .map(|meta| meta.len())
            .max()
    }

    /// The HTTP endpoint to POST to. `ippeveprinter` ignores the request path.
    pub fn uri(&self) -> String {
        format!("http://localhost:{}", self.port)
    }

    /// The printer's own IPP URI. `ippeveprinter` serves exactly one resource,
    /// `/ipp/print`, and rejects any other value of `printer-uri`.
    pub fn printer_uri(&self) -> String {
        format!("ipp://localhost:{}/ipp/print", self.port)
    }

    pub fn queue_name(&self) -> &str {
        &self.queue
    }
}

impl Drop for IppEvePrinter {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.spool);
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("can bind an ephemeral port")
        .local_addr()
        .unwrap()
        .port()
}
