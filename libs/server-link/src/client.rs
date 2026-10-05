//! A blocking client of the observer whose every wait is bounded, for a thread of its own that
//! checks its cancellation between two reads.

use std::fmt;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use crate::protocol::{
    FromObserver, MAX_FROM_OBSERVER, ToObserver, VERSION, Welcome, decode_from_observer, encode_to_observer,
};

#[derive(Debug)]
pub enum Error {
    /// The observer refused the connection, for this reason.
    Refused(String),
    /// The connection was closed.
    Closed,
    /// The observer sent what the protocol does not allow.
    Protocol(String),
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Refused(reason) => write!(f, "refused by the observer: {reason}"),
            Error::Closed => write!(f, "the connection was closed"),
            Error::Protocol(problem) => write!(f, "the observer broke the protocol: {problem}"),
            Error::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Error::Io(error)
    }
}

/// A connection welcomed by the observer.
pub struct Client {
    stream: TcpStream,
    /// What was read and not yet a whole message.
    buffer: Vec<u8>,
    welcome: Welcome,
}

impl Client {
    /// Connects to `address`, gives `token`, and waits for WELCOME, each within `timeout`.
    pub fn connect(address: SocketAddr, token: &str, timeout: Duration) -> Result<Self, Error> {
        let stream = TcpStream::connect_timeout(&address, timeout)?;
        stream.set_nodelay(true)?;
        stream.set_write_timeout(Some(timeout))?;
        let mut client = Self {
            stream,
            buffer: Vec::new(),
            welcome: Welcome {
                version: 0,
                capabilities: 0,
                server: String::new(),
                commit: String::new(),
                max_radius: 0.0,
                max_entities: 0,
                rate: 0,
                heartbeat: 0,
            },
        };
        client.send(&ToObserver::Hello {
            version: VERSION,
            token: token.to_owned(),
        })?;
        match client.receive(timeout)? {
            Some(FromObserver::Welcome(welcome)) if welcome.version == VERSION => {
                client.welcome = welcome;
                Ok(client)
            }
            Some(FromObserver::Welcome(welcome)) => Err(Error::Protocol(format!(
                "version {} of the protocol, {VERSION} expected",
                welcome.version
            ))),
            Some(FromObserver::Refused(reason)) => Err(Error::Refused(reason)),
            Some(other) => Err(Error::Protocol(format!("{other:?} before WELCOME"))),
            None => Err(Error::Protocol(format!("no WELCOME within {timeout:?}"))),
        }
    }

    pub fn welcome(&self) -> &Welcome {
        &self.welcome
    }

    pub fn send(&mut self, message: &ToObserver) -> Result<(), Error> {
        self.stream.write_all(&encode_to_observer(message))?;
        Ok(())
    }

    /// The next message, or none when no whole one came within `timeout`.
    pub fn receive(&mut self, timeout: Duration) -> Result<Option<FromObserver>, Error> {
        let deadline = Instant::now() + timeout;
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            if let Some(message) = self.take()? {
                return Ok(Some(message));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            self.stream.set_read_timeout(Some(left))?;
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err(Error::Closed),
                Ok(read) => self.buffer.extend_from_slice(&chunk[..read]),
                Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                    return Ok(None);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(Error::Io(error)),
            }
        }
    }

    /// The message whole at the start of the buffer, taken out of it.
    fn take(&mut self) -> Result<Option<FromObserver>, Error> {
        let Some(header) = self.buffer.get(..4) else {
            return Ok(None);
        };
        let length = u32::from_le_bytes(header.try_into().expect("four bytes")) as usize;
        if length == 0 || length > MAX_FROM_OBSERVER {
            return Err(Error::Protocol(format!("a message of {length} bytes")));
        }
        if self.buffer.len() < 4 + length {
            return Ok(None);
        }
        let message = decode_from_observer(self.buffer[4], &self.buffer[5..4 + length]).map_err(Error::Protocol)?;
        self.buffer.drain(..4 + length);
        Ok(Some(message))
    }
}
