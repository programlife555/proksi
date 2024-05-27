use std::io;

use async_trait::async_trait;
use pingora::{
    server::{ListenFds, ShutdownWatch},
    services::Service,
};

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tracing_subscriber::fmt::MakeWriter;

/// A `io::Write` implementation that sends logs to a background service
#[derive(Debug, Clone)]
pub struct StdoutWriter(UnboundedSender<Vec<u8>>);

impl io::Write for StdoutWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let buf_copy = buf.to_owned();
        if let Ok(()) = self.0.send(buf_copy) {
            return Ok(buf.len());
        }

        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A naive implementation of a logger that delegate sending logs to a background channel
#[derive(Debug)]
pub struct ProxyLog {
    stdout: StdoutWriter,
}

impl ProxyLog {
    pub fn new(sender: &UnboundedSender<Vec<u8>>) -> Self {
        ProxyLog {
            // level,
            stdout: StdoutWriter(sender.clone()),
        }
    }
}

/// impl from `tracing_subscriber::fmt::MakeWriter`
impl<'a> MakeWriter<'a> for ProxyLog {
    type Writer = StdoutWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.stdout.clone()
    }
}

/// A background service that receives logs from the main thread and writes them to stdout
/// TODO: implement log rotation/write to disk (or use an existing lightweight crate)
pub struct ProxyLoggerReceiver {
    receiver: UnboundedReceiver<Vec<u8>>,
}

impl ProxyLoggerReceiver {
    pub fn new(receiver: UnboundedReceiver<Vec<u8>>) -> Self {
        ProxyLoggerReceiver { receiver }
    }
}

#[async_trait]
impl Service for ProxyLoggerReceiver {
    async fn start_service(&mut self, _fds: Option<ListenFds>, _shutdown: ShutdownWatch) {
        loop {
            if let Some(buf) = self.receiver.recv().await {
                let buf = std::str::from_utf8(&buf).unwrap();
                // TODO: flush/rotate logs to disk
                print!("{buf}");
            }
        }
    }

    fn name(&self) -> &str {
        "ProxyLogger"
    }

    fn threads(&self) -> Option<usize> {
        Some(1)
    }
}
