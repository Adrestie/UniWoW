//! Tests of the connection's thread without a server: a connection refused said without its
//! password, the queries refused while not connected, and those that write refused before they go.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use uniwow_api::db::Settings;
use uniwow_api::world_db::{State, WorldDb};

use crate::{Asked, Service, Shared, serve};

/// The thread of a connection, serving until the test ends.
fn running() -> (Arc<Shared>, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let shared = Arc::new(Shared::default());
    let (asked, receiver) = std::sync::mpsc::channel();
    *crate::lock(&shared.asked) = Some(asked);
    let stop = Arc::new(AtomicBool::new(false));
    let (thread_shared, thread_stop) = (shared.clone(), stop.clone());
    let thread = std::thread::spawn(move || serve(thread_shared, receiver, || thread_stop.load(Ordering::Relaxed)));
    (shared, stop, thread)
}

#[test]
fn a_connection_refused_is_said_without_its_password_and_the_queries_wait_for_none() {
    let (shared, stop, thread) = running();
    let service = Service(shared.clone());
    assert_eq!(service.state(), State::Waiting);
    // Nothing listens on the port 1 of this machine.
    let settings = Settings {
        port: 1,
        ..Settings::default()
    };
    let password = "the-password-of-the-test";
    crate::lock(&shared.asked)
        .as_ref()
        .unwrap()
        .send(Asked::Connect(settings, password.to_owned()))
        .unwrap();
    let started = Instant::now();
    let refused = loop {
        if let State::Refused(reason) = service.state() {
            break reason;
        }
        assert!(started.elapsed() < Duration::from_secs(20), "never refused");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(!refused.contains(password), "{refused}");
    // The password is nowhere in what the module keeps but the thread, which forgot it once refused.
    assert!(!format!("{:?}", service.state()).contains(password));
    assert!(service.query("SELECT 1", &[]).unwrap_err().contains("not connected"));
    assert!(
        service
            .query("DELETE FROM creature", &[])
            .unwrap_err()
            .contains("read only")
    );
    stop.store(true, Ordering::Relaxed);
    thread.join().unwrap();
}
