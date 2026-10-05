//! A fake observer on 127.0.0.1, for the tests of the client and of the modules using it: it
//! welcomes the token it is given, keeps what it receives, and sends what the test queues.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::protocol::{
    CAPABILITY_READING, FromObserver, MAX_FROM_EDITOR, ToObserver, VERSION, Welcome, decode_to_observer,
    encode_from_observer,
};

const POLL: Duration = Duration::from_millis(5);

#[derive(Default)]
struct Shared {
    token: String,
    received: Mutex<Vec<ToObserver>>,
    outgoing: Mutex<VecDeque<Vec<u8>>>,
    accepted: AtomicUsize,
    drop_connection: AtomicBool,
    stop: AtomicBool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub struct FakeObserver {
    address: SocketAddr,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl FakeObserver {
    /// Listens on a free port of 127.0.0.1 and welcomes `token`; one connection at a time.
    pub fn start(token: &str) -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let shared = Arc::new(Shared {
            token: token.to_owned(),
            ..Shared::default()
        });
        let serving = shared.clone();
        let thread = std::thread::spawn(move || {
            while !serving.stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        serving.accepted.fetch_add(1, Ordering::AcqRel);
                        serving.drop_connection.store(false, Ordering::Release);
                        let _ = serve(&serving, stream);
                    }
                    Err(_) => std::thread::sleep(POLL),
                }
            }
        });
        Ok(Self {
            address,
            shared,
            thread: Some(thread),
        })
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// Queues `message` for the connection there is, or the next one.
    pub fn send(&self, message: &FromObserver) {
        self.send_raw(encode_from_observer(message));
    }

    /// Queues bytes as they are, whatever the protocol says of them.
    pub fn send_raw(&self, bytes: Vec<u8>) {
        lock(&self.shared.outgoing).push_back(bytes);
    }

    /// What every connection sent, in order, HELLO included.
    pub fn received(&self) -> Vec<ToObserver> {
        lock(&self.shared.received).clone()
    }

    /// Whether what was received satisfies `test` within `timeout`.
    pub fn wait_for(&self, timeout: Duration, test: impl Fn(&[ToObserver]) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if test(&lock(&self.shared.received)) {
                return true;
            }
            std::thread::sleep(POLL);
        }
        test(&lock(&self.shared.received))
    }

    /// The connections accepted so far.
    pub fn accepted(&self) -> usize {
        self.shared.accepted.load(Ordering::Acquire)
    }

    /// Closes the connection there is, as a server stopping would.
    pub fn drop_connection(&self) {
        self.shared.drop_connection.store(true, Ordering::Release);
    }
}

impl Drop for FakeObserver {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Serves one connection until it closes, is dropped or the observer stops.
fn serve(shared: &Shared, mut stream: TcpStream) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(POLL))?;
    let mut buffer = Vec::new();
    let mut welcomed = false;
    let mut chunk = [0u8; 4096];
    loop {
        if shared.stop.load(Ordering::Acquire) || shared.drop_connection.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        while welcomed && let Some(bytes) = lock(&shared.outgoing).pop_front() {
            stream.write_all(&bytes)?;
        }
        match stream.read(&mut chunk) {
            Ok(0) => return Ok(()),
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
            Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => continue,
            Err(error) => return Err(error),
        }
        while buffer.len() >= 4 {
            let length = u32::from_le_bytes(buffer[..4].try_into().expect("four bytes")) as usize;
            if length == 0 || length > MAX_FROM_EDITOR {
                return Ok(());
            }
            if buffer.len() < 4 + length {
                break;
            }
            let Ok(message) = decode_to_observer(buffer[4], &buffer[5..4 + length]) else {
                return Ok(());
            };
            buffer.drain(..4 + length);
            lock(&shared.received).push(message.clone());
            if let ToObserver::Hello { version, token } = message {
                if welcomed {
                    return Ok(());
                }
                if version != VERSION || token != shared.token {
                    stream.write_all(&encode_from_observer(&FromObserver::Refused("wrong token".to_owned())))?;
                    return Ok(());
                }
                welcomed = true;
                stream.write_all(&encode_from_observer(&FromObserver::Welcome(Welcome {
                    version: VERSION,
                    capabilities: CAPABILITY_READING,
                    server: "a fake observer".to_owned(),
                    commit: "0000000".to_owned(),
                    max_radius: 533.0,
                    max_entities: 2000,
                    rate: 10,
                    heartbeat: 10,
                })))?;
            }
        }
    }
}
