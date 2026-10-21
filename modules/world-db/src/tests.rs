//! Tests of the connection's thread without a server: a connection refused said without its
//! password, the queries refused while not connected, and those that write refused before they go.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use uniwow_api::db::Settings;
use uniwow_api::serde_json::json;
use uniwow_api::world_db::{Pending, Reload, State, WorldDb};

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
    module.pending = Some(crate::Asking {
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
    module.pending = Some(crate::Asking {
        keep: Some((target.clone(), "acore".to_owned(), "wrong".to_owned())),
        automatic: false,
    });
    module.shared.set(State::Refused("Access denied".to_owned()));
    module.settle();
    assert_eq!(vault.get(&target).unwrap(), None);
    assert!(!module.asking);
    // Refused at the start, with the password kept: the window opened again.
    module.pending = Some(crate::Asking {
        keep: None,
        automatic: true,
    });
    module.settle();
    assert!(module.asking);
}

#[test]
fn an_application_is_dated_named_and_kept_in_the_project_as_it_was() {
    assert_eq!(crate::when(0), "1970-01-01 00:00:00");
    assert_eq!(crate::when(1_792_568_880), "2026-10-21 07:48:00");
    assert_eq!(crate::when(951_782_400), "2000-02-29 00:00:00");
    assert_eq!(
        crate::script_name("2026-10-21 07:48:00", false),
        "uniwow_2026_10_21_074800.sql"
    );
    assert_eq!(
        crate::script_name("2026-10-21 07:48:00", true),
        "uniwow_2026_10_21_074800_undone.sql"
    );
    let application = crate::Application {
        when: "2026-10-21 07:48:00".to_owned(),
        database: "acore_world_test on 127.0.0.1:3306".to_owned(),
        owners: vec!["spawns".to_owned()],
        entries: vec![uniwow_api::db::write::Entry {
            table: "creature".to_owned(),
            key: uniwow_api::db::write::row([("guid", json!(1))]),
            before: vec![uniwow_api::db::write::row([("guid", json!(1)), ("map", json!(0))])],
            after: vec![uniwow_api::db::write::row([("guid", json!(1)), ("map", json!(1))])],
        }],
        reloads: vec![
            Reload::Quests,
            Reload::Path(790),
            Reload::Formation(79),
            Reload::Creature(79),
            Reload::GameObject(5),
        ],
    };
    assert_eq!(application.rows(), 1);
    assert_eq!(crate::Application::of_json(&application.to_json()), Some(application));
}

#[test]
fn the_changes_of_each_module_replace_those_it_gave_before_and_go_once_it_holds_none() {
    let service = Service(Arc::default());
    let held = |owner: &str, rows: usize| Pending {
        owner: owner.to_owned(),
        label: owner.to_owned(),
        changes: (0..rows)
            .map(|guid| uniwow_api::db::write::RowChange::Delete {
                table: "creature".to_owned(),
                key: uniwow_api::db::write::row([("guid", json!(guid))]),
            })
            .collect(),
        ..Pending::default()
    };
    service.set_pending(held("spawns", 2));
    service.set_pending(held("paths", 1));
    service.set_pending(held("spawns", 3));
    let pending = service.pending();
    assert_eq!(
        pending
            .iter()
            .map(|held| (held.owner.as_str(), held.changes.len()))
            .collect::<Vec<_>>(),
        [("paths", 1), ("spawns", 3)]
    );
    service.set_pending(held("paths", 0));
    assert_eq!(service.pending().len(), 1);
    // Not connected: nothing written, and said.
    assert!(service.apply(&pending[1].changes).is_err());
    let outside = uniwow_api::db::write::RowChange::Delete {
        table: "account".to_owned(),
        key: uniwow_api::db::write::row([("id", json!(1))]),
    };
    assert!(service.apply(&[outside]).unwrap_err().contains("account"));
}
