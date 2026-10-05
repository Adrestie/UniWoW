use std::net::TcpListener;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use uniwow_api::serde_json::json;
use uniwow_api::server_link::fake::FakeObserver;
use uniwow_api::server_link::protocol::{
    CYCLIC, Entity, FromObserver, Kind, MOVING, PathPoint, Spline, State, ToObserver, Zone,
};

use crate::link::{self, Connection, MOVE_EVERY, Settings, Shared, Surroundings, should_subscribe};
use crate::world::{Change, Tracked, World};
use crate::{entities, lock, state};

const WAIT: Duration = Duration::from_secs(5);

fn entity(guid: u64, kind: Kind, position: [f32; 3]) -> Entity {
    Entity {
        guid,
        kind,
        entry: 3296,
        spawn: guid as u32,
        flags: 0,
        pool: 0,
        event: 0,
        phase: 1,
        display: 4259,
        position,
        orientation: 0.0,
        scale: 1.0,
        rotation: (kind == Kind::GameObject).then_some([0.0, 0.0, 0.0, 1.0]),
        state: (kind == Kind::GameObject).then_some(1),
        spline: None,
        name: format!("entity {guid}"),
    }
}

/// A spline from `origin`: 10 yards along x in a second, then 20 along y in two.
fn spline(origin: [f32; 3], elapsed: u32, flags: u8) -> Spline {
    let at = |dx: f32, dy: f32| [origin[0] + dx, origin[1] + dy, origin[2]];
    Spline {
        id: 9,
        flags,
        elapsed,
        points: vec![
            PathPoint {
                position: at(0.0, 0.0),
                time: 0,
            },
            PathPoint {
                position: at(10.0, 0.0),
                time: 1000,
            },
            PathPoint {
                position: at(10.0, 20.0),
                time: 3000,
            },
        ],
    }
}

/// Whether `test` holds within `WAIT`.
fn until(test: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        if test() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    test()
}

#[derive(Default)]
struct Fake {
    camera: Mutex<Option<[f32; 3]>>,
    map: Mutex<Option<u32>>,
    changes: Mutex<Vec<Change>>,
}

impl Surroundings for Fake {
    fn camera(&self) -> Option<[f32; 3]> {
        *lock(&self.camera)
    }
    fn map(&self) -> Option<u32> {
        *lock(&self.map)
    }
    fn changed(&self, change: &Change) {
        lock(&self.changes).push(change.clone());
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn subscriptions(observer: &FakeObserver) -> Vec<Zone> {
    observer
        .received()
        .into_iter()
        .filter_map(|message| match message {
            ToObserver::Subscribe(zone) => Some(zone),
            _ => None,
        })
        .collect()
}

/// Sets `stop` when dropped, a failed assertion included: the link's thread then ends and the
/// scope can join it.
struct Stopper<'a>(&'a AtomicBool);

impl Drop for Stopper<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

#[test]
fn a_world_is_replaced_by_a_snapshot_changed_by_changes_and_emptied_telling_each_guid() {
    let now = Instant::now();
    let (world, change) = World::default().snapshot(
        1,
        0,
        1,
        vec![
            entity(1, Kind::Creature, [0.0; 3]),
            entity(2, Kind::GameObject, [0.0; 3]),
        ],
        now,
    );
    assert_eq!(
        (change.appeared.len(), change.changed.len(), change.left.len()),
        (2, 0, 0)
    );
    let (world, change) = world.changes(
        2,
        vec![entity(2, Kind::GameObject, [1.0; 3]), entity(3, Kind::Player, [0.0; 3])],
        &[1, 99],
        now,
    );
    assert_eq!(change.appeared, vec![3]);
    assert_eq!(change.changed, vec![2]);
    assert_eq!(change.left, vec![1], "a guid not held is not said to leave");
    assert_eq!(
        (
            world.count(Kind::Creature),
            world.count(Kind::GameObject),
            world.count(Kind::Player)
        ),
        (0, 1, 1)
    );
    // A snapshot of the same map keeps the entities still there as changed; of another, all new.
    let (same, change) = world.snapshot(1, 0, 3, vec![entity(3, Kind::Player, [0.0; 3])], now);
    assert_eq!((change.changed, change.left), (vec![3], vec![2]));
    let (_, change) = same.snapshot(571, 0, 1, vec![entity(3, Kind::Player, [0.0; 3])], now);
    assert_eq!((change.appeared, change.left), (vec![3], vec![3]));
    let (empty, change) = same.emptied();
    assert!(empty.entities.is_empty());
    assert_eq!(change.left, vec![3]);
}

#[test]
fn an_entity_on_a_spline_stands_where_its_times_say_counting_on_from_its_reception() {
    let received = Instant::now();
    let mut moving = entity(1, Kind::Creature, [0.0; 3]);
    moving.spline = Some(spline([0.0; 3], 500, 0));
    let tracked = Tracked {
        entity: moving.clone(),
        received,
    };
    assert_eq!(
        tracked.position_at(received),
        [5.0, 0.0, 0.0],
        "half of its first segment"
    );
    assert_eq!(
        tracked.position_at(received + Duration::from_millis(1500)),
        [10.0, 10.0, 0.0]
    );
    assert_eq!(
        tracked.position_at(received + Duration::from_secs(60)),
        [10.0, 20.0, 0.0],
        "its end"
    );
    moving.spline = Some(spline([0.0; 3], 500, CYCLIC));
    let cyclic = Tracked {
        entity: moving,
        received,
    };
    assert_eq!(
        cyclic.position_at(received + Duration::from_millis(3000)),
        [5.0, 0.0, 0.0],
        "round again"
    );
    let still = Tracked {
        entity: entity(2, Kind::Creature, [7.0, 8.0, 9.0]),
        received,
    };
    assert_eq!(still.position_at(received + Duration::from_secs(9)), [7.0, 8.0, 9.0]);
}

#[test]
fn a_zone_is_subscribed_to_again_on_another_map_at_once_and_once_moved_an_eighth_of_its_radius() {
    let zone = Zone {
        map: 1,
        instance: 0,
        centre: [0.0, 0.0, 0.0],
        radius: 400.0,
    };
    let moved = |dx: f32| Zone {
        centre: [dx, 0.0, 0.0],
        ..zone
    };
    assert!(should_subscribe(None, &zone, Duration::ZERO));
    assert!(should_subscribe(
        Some(&zone),
        &Zone { map: 571, ..zone },
        Duration::ZERO
    ));
    assert!(should_subscribe(
        Some(&zone),
        &Zone { instance: 3, ..zone },
        Duration::ZERO
    ));
    assert!(
        !should_subscribe(Some(&zone), &moved(49.0), MOVE_EVERY),
        "less than 50 yards"
    );
    assert!(!should_subscribe(Some(&zone), &moved(51.0), MOVE_EVERY / 2), "too soon");
    assert!(should_subscribe(Some(&zone), &moved(51.0), MOVE_EVERY));
    assert!(should_subscribe(
        Some(&zone),
        &Zone { radius: 300.0, ..zone },
        MOVE_EVERY
    ));
}

#[test]
fn the_link_follows_the_camera_and_the_map_keeps_the_entities_and_connects_again() {
    let observer = FakeObserver::start("secret").unwrap();
    let settings = Settings {
        port: observer.address().port(),
        world_port: free_port(),
        token: "secret".to_owned(),
        radius: 300.0,
    };
    let shared = Shared::default();
    let fake = Fake::default();
    *lock(&fake.map) = Some(1);
    *lock(&fake.camera) = Some([1629.0, -4373.0, 30.0]);
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| link::run(&settings, &shared, &fake, &|| stop.load(Ordering::Acquire)));
        let _stopper = Stopper(&stop);

        assert!(until(|| subscriptions(&observer).len() == 1));
        assert_eq!(
            subscriptions(&observer)[0],
            Zone {
                map: 1,
                instance: 0,
                centre: [1629.0, -4373.0, 30.0],
                radius: 300.0
            }
        );
        assert!(until(|| shared.status().connection == Connection::Connected));
        assert_eq!(shared.status().server, "a fake observer");

        observer.send(&FromObserver::Status {
            state: State::Active,
            map: 1,
            instance: 0,
            radius: 300.0,
        });
        let mut walking = entity(0xF130_0000_0000_0001, Kind::Creature, [1630.0, -4373.0, 30.0]);
        walking.flags = MOVING;
        walking.spline = Some(spline([1630.0, -4373.0, 30.0], 0, 0));
        observer.send(&FromObserver::Snapshot {
            map: 1,
            instance: 0,
            sequence: 1,
            entities: vec![
                walking,
                entity(2, Kind::GameObject, [1700.0, -4373.0, 30.0]),
                entity(3, Kind::Player, [1629.0, -4300.0, 30.0]),
            ],
        });
        assert!(until(|| shared.world().entities.len() == 3));
        assert_eq!(shared.status().state, Some(State::Active));
        assert_eq!(lock(&fake.changes).last().unwrap().appeared.len(), 3);

        // The commands read the snapshot: the nearest first, by kind, within a radius, limited.
        let found = entities(&shared, &json!({ "x": 1629.0, "y": -4373.0 })).unwrap();
        let guids: Vec<&str> = found["entities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["guid"].as_str().unwrap())
            .collect();
        assert_eq!(guids.len(), 3);
        assert_eq!(guids[0], "0xF130000000000001", "a GUID as text, all 64 bits");
        assert_eq!(found["entities"][0]["moving"], true);
        let players = entities(&shared, &json!({ "kinds": ["player"] })).unwrap();
        assert_eq!(
            players["entities"].as_array().unwrap().len(),
            1,
            "around the zone by default"
        );
        let near = entities(&shared, &json!({ "x": 1629.0, "y": -4373.0, "radius": 50.0 })).unwrap();
        assert_eq!(near["entities"].as_array().unwrap().len(), 1, "within 50 yards");
        let two = entities(
            &shared,
            &json!({ "x": 1629.0, "y": -4373.0, "radius": 1000.0, "limit": 2 }),
        )
        .unwrap();
        assert_eq!(two["entities"].as_array().unwrap().len(), 2, "at most the limit");
        assert!(entities(&shared, &json!({ "kinds": "player" })).is_err());
        let now = state(&shared, &settings);
        assert_eq!(now["connection"], "connected");
        assert_eq!(now["subscription"], "active");
        assert_eq!(
            now["entities"],
            json!({ "creatures": 1, "game_objects": 1, "players": 1 })
        );

        observer.send(&FromObserver::Changes {
            map: 1,
            instance: 0,
            sequence: 2,
            entities: vec![entity(2, Kind::GameObject, [1701.0, -4373.0, 30.0])],
            left: vec![3],
        });
        assert!(until(|| shared.world().entities.len() == 2));
        let change = lock(&fake.changes).last().unwrap().clone();
        assert_eq!((change.changed, change.left), (vec![2], vec![3]));

        // A move of 20 yards is not worth a subscription; one of 100 is, once half a second passed.
        *lock(&fake.camera) = Some([1649.0, -4373.0, 30.0]);
        std::thread::sleep(MOVE_EVERY + Duration::from_millis(200));
        assert_eq!(subscriptions(&observer).len(), 1);
        *lock(&fake.camera) = Some([1729.0, -4373.0, 30.0]);
        assert!(until(|| subscriptions(&observer).len() == 2));
        assert_eq!(subscriptions(&observer)[1].centre, [1729.0, -4373.0, 30.0]);
        assert_eq!(subscriptions(&observer)[1].map, 1);

        // Another map at once; no map, no zone.
        *lock(&fake.map) = Some(571);
        assert!(until(|| subscriptions(&observer).len() == 3));
        assert_eq!(subscriptions(&observer)[2].map, 571);
        *lock(&fake.map) = None;
        assert!(until(|| observer.received().contains(&ToObserver::Unsubscribe)));
        assert!(until(|| shared.world().entities.is_empty()));
        assert!(until(|| shared.status().zone.is_none()));

        // The server stops: the entities go, the link connects again and subscribes again.
        *lock(&fake.map) = Some(1);
        assert!(until(|| subscriptions(&observer).len() == 4));
        observer.send(&FromObserver::Snapshot {
            map: 1,
            instance: 0,
            sequence: 1,
            entities: vec![entity(5, Kind::Creature, [0.0; 3])],
        });
        assert!(until(|| shared.world().entities.len() == 1));
        observer.drop_connection();
        assert!(until(|| shared.world().entities.is_empty()));
        assert!(until(|| observer.accepted() == 2 && subscriptions(&observer).len() == 5));
    });
    // Each change said once, none empty.
    assert!(lock(&fake.changes).iter().all(|change| !change.is_empty()));
}

/// The connection of the link with `settings`, once it is no longer connecting.
fn standing(settings: Settings) -> Connection {
    let shared = Shared::default();
    let fake = Fake::default();
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| link::run(&settings, &shared, &fake, &|| stop.load(Ordering::Acquire)));
        let _stopper = Stopper(&stop);
        until(|| shared.status().connection != Connection::Connecting);
    });
    shared.status().connection
}

#[test]
fn no_token_a_wrong_one_a_server_stopped_and_an_observer_missing_are_told_apart() {
    let observer = FakeObserver::start("secret").unwrap();
    let port = observer.address().port();
    let closed = free_port();
    let settings = |port: u16, world_port: u16, token: &str| Settings {
        port,
        world_port,
        token: token.to_owned(),
        radius: 300.0,
    };
    assert_eq!(standing(settings(port, closed, "")), Connection::NoToken);
    assert_eq!(
        standing(settings(port, closed, "not it")),
        Connection::Refused("wrong token".to_owned())
    );
    assert_eq!(standing(settings(closed, closed, "secret")), Connection::ServerStopped);
    let world = TcpListener::bind("127.0.0.1:0").unwrap();
    let world_port = world.local_addr().unwrap().port();
    assert_eq!(
        standing(settings(closed, world_port, "secret")),
        Connection::ObserverMissing
    );
    let text = crate::connection_text(
        &link::Status {
            connection: Connection::ObserverMissing,
            ..link::Status::default()
        },
        &settings(8087, 8085, "secret"),
    );
    assert!(text.1 && text.0.contains("8085") && text.0.contains("8087"), "{text:?}");
}
