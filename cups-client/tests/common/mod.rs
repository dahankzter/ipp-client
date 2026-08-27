// SPDX-License-Identifier: GPL-3.0-only

use std::process::{Child, Command, Stdio};

/// A live `ippeveprinter` process on an ephemeral port, killed on drop.
pub struct IppEvePrinter {
    child: Child,
    port: u16,
    queue: String,
}

impl IppEvePrinter {
    pub async fn start(queue: &str) -> IppEvePrinter {
        let port = free_port();
        let child = Command::new("ippeveprinter")
            .args(["-p", &port.to_string(), queue])
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
        }
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
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("can bind an ephemeral port")
        .local_addr()
        .unwrap()
        .port()
}
