//! Small cancellable TCP layer beneath ureq's existing rustls TLS implementation.
//! ureq is pinned because this extension interface is explicitly unversioned.
use crate::CancellationToken;
use std::{
    io::{self, Read, Write},
    net::TcpStream,
    sync::Arc,
    time::{Duration, Instant},
};
use ureq::{
    Error,
    unversioned::{
        resolver::DefaultResolver,
        transport::{
            Buffers, ConnectionDetails, Connector, LazyBuffers, NextTimeout, RustlsConnector,
            Transport,
        },
    },
};

pub(crate) fn cancellable_agent(
    config: ureq::config::Config,
    token: CancellationToken,
) -> ureq::Agent {
    let connector = CancellableConnector(token).chain(RustlsConnector::default());
    ureq::Agent::with_parts(config, connector, DefaultResolver::default())
}
#[derive(Debug)]
struct CancellableConnector(CancellationToken);
impl Connector for CancellableConnector {
    type Out = CancellableTransport;
    fn connect(
        &self,
        details: &ConnectionDetails,
        _: Option<()>,
    ) -> Result<Option<Self::Out>, Error> {
        let start = Instant::now();
        let budget = details
            .timeout
            .not_zero()
            .map(|d| *d)
            .unwrap_or(Duration::from_secs(10));
        let count = details.addrs.len().max(1) as u32;
        for address in &details.addrs {
            check(&self.0)?;
            let remaining = budget.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return Err(Error::Timeout(details.timeout.reason));
            }
            let per_address = (budget / count)
                .min(remaining)
                .max(Duration::from_millis(1));
            match TcpStream::connect_timeout(address, per_address) {
                Ok(socket) => {
                    socket.set_nodelay(true)?;
                    let stream = Arc::new(socket);
                    self.0.register(&stream);
                    check(&self.0)?;
                    return Ok(Some(CancellableTransport {
                        stream,
                        token: self.0.clone(),
                        buffers: LazyBuffers::new(
                            details.config.input_buffer_size(),
                            details.config.output_buffer_size(),
                        ),
                    }));
                }
                Err(_) => continue,
            }
        }
        Err(Error::ConnectionFailed)
    }
}
struct CancellableTransport {
    stream: Arc<TcpStream>,
    token: CancellationToken,
    buffers: LazyBuffers,
}
impl std::fmt::Debug for CancellableTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CancellableTransport")
    }
}
fn check(token: &CancellationToken) -> Result<(), Error> {
    if token.is_cancelled() {
        Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled").into())
    } else {
        Ok(())
    }
}
fn map_io<T>(result: io::Result<T>, timeout: NextTimeout) -> Result<T, Error> {
    result.map_err(|error| {
        if matches!(
            error.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        ) {
            Error::Timeout(timeout.reason)
        } else {
            error.into()
        }
    })
}
impl Transport for CancellableTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }
    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), Error> {
        check(&self.token)?;
        self.stream
            .set_write_timeout(timeout.not_zero().map(|d| *d))?;
        map_io(
            self.stream
                .as_ref()
                .write_all(&self.buffers.output()[..amount]),
            timeout,
        )?;
        check(&self.token)
    }
    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, Error> {
        check(&self.token)?;
        self.stream
            .set_read_timeout(timeout.not_zero().map(|d| *d))?;
        let count = map_io(
            self.stream.as_ref().read(self.buffers.input_append_buf()),
            timeout,
        )?;
        check(&self.token)?;
        self.buffers.input_appended(count);
        Ok(count > 0)
    }
    fn is_open(&mut self) -> bool {
        // Each agent performs one request; pooling is deliberately disabled.
        !self.token.is_cancelled()
    }
}
