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

#[test]
fn at_the_start_the_password_kept_connects_unless_the_user_chose_otherwise() {
    use crate::{Start, start};
    assert_eq!(
        start(true, Ok(Some("kept".to_owned()))),
        Start::Connect("kept".to_owned())
    );
    assert_eq!(
        start(false, Ok(Some("kept".to_owned()))),
        Start::Ask,
        "not at the start"
    );
    assert_eq!(start(true, Ok(None)), Start::Ask, "none kept");
    assert_eq!(start(true, Ok(Some(String::new()))), Start::Ask);
    assert_eq!(start(true, Err("unreadable".to_owned())), Start::Ask);
}

#[test]
fn a_password_is_kept_once_connected_and_a_refusal_at_the_start_asks_again() {
    use uniwow_api::db::secret::{self, Memory, Vault};
    let vault = Arc::new(Memory::default());
    let mut module = crate::WorldDbModule {
        vault: vault.clone(),
        ..crate::WorldDbModule::default()
    };
    let target = secret::target(&Settings::default());
    // Asked by the window with Remember password: kept once connected, not before.
    module.pending = Some(crate::Pending {
        keep: Some((target.clone(), "acore".to_owned(), "typed".to_owned())),
        automatic: false,
    });
    module.shared.set(State::Connecting);
    module.settle();
    assert_eq!(vault.get(&target).unwrap(), None, "not before it connects");
    module.shared.set(State::Connected("8.4.8".to_owned()));
    module.settle();
    assert_eq!(vault.get(&target).unwrap().as_deref(), Some("typed"));
    assert!(module.pending.is_none());
    // Refused, typed: nothing kept, the window not opened by itself.
    vault.forget(&target).unwrap();
    module.pending = Some(crate::Pending {
        keep: Some((target.clone(), "acore".to_owned(), "wrong".to_owned())),
        automatic: false,
    });
    module.shared.set(State::Refused("Access denied".to_owned()));
    module.settle();
    assert_eq!(vault.get(&target).unwrap(), None);
    assert!(!module.asking);
    // Refused at the start, with the password kept: the window opened again.
    module.pending = Some(crate::Pending {
        keep: None,
        automatic: true,
    });
    module.settle();
    assert!(module.asking);
}
