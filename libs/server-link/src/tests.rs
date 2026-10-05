use std::net::TcpListener;
use std::time::Duration;

use crate::fake::FakeObserver;
use crate::protocol::*;
use crate::{Client, Error};

const WAIT: Duration = Duration::from_secs(5);

fn creature() -> Entity {
    Entity {
        guid: 0xF130_0000_0B8D_0001,
        kind: Kind::Creature,
        entry: 3296,
        spawn: 10467,
        flags: MOVING | WALKING,
        pool: 0,
        event: -12,
        phase: 1,
        display: 4259,
        position: [1629.5, -4373.25, 31.0],
        orientation: 1.5,
        scale: 1.0,
        rotation: None,
        state: None,
        spline: Some(Spline {
            id: 77,
            flags: CATMULL_ROM | CYCLIC,
            elapsed: 1250,
            points: vec![
                PathPoint {
                    position: [1629.5, -4373.25, 31.0],
                    time: 0,
                },
                PathPoint {
                    position: [1640.0, -4380.0, 31.5],
                    time: 2400,
                },
            ],
        }),
        name: "Orgrimmar Grunt".to_owned(),
    }
}

fn game_object() -> Entity {
    Entity {
        guid: 0xF110_0000_0000_0002,
        kind: Kind::GameObject,
        entry: 19551,
        spawn: 0,
        flags: TEMPORARY,
        pool: 3,
        event: 0,
        phase: 1,
        display: 3601,
        position: [1.0, 2.0, 3.0],
        orientation: 0.0,
        scale: 2.0,
        rotation: Some([0.0, 0.0, 0.6, 0.8]),
        state: Some(1),
        spline: None,
        name: "Brazier — chaud".to_owned(),
    }
}

/// The kind and the body of a framed message.
fn unframe(message: &[u8]) -> (u8, &[u8]) {
    let length = u32::from_le_bytes(message[..4].try_into().unwrap()) as usize;
    assert_eq!(length + 4, message.len(), "the length counts the kind and the body");
    (message[4], &message[5..])
}

#[test]
fn every_message_both_ways_is_read_back_as_written() {
    for message in [
        ToObserver::Hello {
            version: VERSION,
            token: "secret".to_owned(),
        },
        ToObserver::Subscribe(Zone {
            map: 1,
            instance: 0,
            centre: [1629.0, -4373.0, 30.0],
            radius: 300.0,
        }),
        ToObserver::Unsubscribe,
        ToObserver::Heartbeat,
    ] {
        let bytes = encode_to_observer(&message);
        let (kind, body) = unframe(&bytes);
        assert_eq!(decode_to_observer(kind, body), Ok(message));
    }
    for message in [
        FromObserver::Welcome(Welcome {
            version: VERSION,
            capabilities: CAPABILITY_READING,
            server: "AzerothCore rev. bc9198ce7111".to_owned(),
            commit: "bc9198ce7111".to_owned(),
            max_radius: 533.0,
            max_entities: 2000,
            rate: 10,
            heartbeat: 10,
        }),
        FromObserver::Refused("wrong token".to_owned()),
        FromObserver::Status {
            state: State::NotFound,
            map: 9999,
            instance: 0,
            radius: 100.0,
        },
        FromObserver::Snapshot {
            map: 1,
            instance: 0,
            sequence: 2,
            entities: vec![creature(), game_object()],
        },
        FromObserver::Changes {
            map: 1,
            instance: 0,
            sequence: 3,
            entities: vec![game_object()],
            left: vec![5, 6],
        },
    ] {
        let bytes = encode_from_observer(&message);
        let (kind, body) = unframe(&bytes);
        assert_eq!(decode_from_observer(kind, body), Ok(message));
    }
}

#[test]
fn a_body_malformed_is_an_error_never_a_panic_nor_a_huge_allocation() {
    let snapshot = encode_from_observer(&FromObserver::Snapshot {
        map: 1,
        instance: 0,
        sequence: 1,
        entities: vec![creature()],
    });
    let (kind, body) = unframe(&snapshot);
    for cut in 0..body.len() {
        assert!(decode_from_observer(kind, &body[..cut]).is_err(), "cut at {cut}");
    }
    let mut longer = body.to_vec();
    longer.push(0);
    assert!(decode_from_observer(kind, &longer).unwrap_err().contains("more than"));
    assert!(decode_from_observer(77, &[]).unwrap_err().contains("unknown kind"));
    // A count of four billion entities in a body of 24 bytes.
    let mut lying = vec![1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0];
    lying.extend_from_slice(&u32::MAX.to_le_bytes());
    lying.extend_from_slice(&[0; 4]);
    assert!(decode_from_observer(kind, &lying).is_err());
    let mut entity_kind = body.to_vec();
    entity_kind[28] = 9;
    assert!(
        decode_from_observer(kind, &entity_kind)
            .unwrap_err()
            .contains("entity of an unknown kind")
    );
    let refused = [2, 0, 0xFF, 0xFE];
    assert!(decode_from_observer(102, &refused).unwrap_err().contains("UTF-8"));

    // From the editor too: a body longer or shorter than its kind says.
    let subscribe = encode_to_observer(&ToObserver::Subscribe(Zone {
        map: 1,
        instance: 0,
        centre: [0.0; 3],
        radius: 10.0,
    }));
    let (kind, body) = unframe(&subscribe);
    let mut longer = body.to_vec();
    longer.push(0);
    assert!(decode_to_observer(kind, &longer).unwrap_err().contains("more than"));
    assert!(decode_to_observer(kind, &body[..body.len() - 1]).is_err());
    assert!(decode_to_observer(4, &[0]).is_err(), "a HEARTBEAT with a body");
}

#[test]
fn the_client_is_welcomed_subscribes_and_reads_messages_cut_anywhere() {
    let observer = FakeObserver::start("secret").unwrap();
    let mut client = Client::connect(observer.address(), "secret", WAIT).unwrap();
    assert_eq!(client.welcome().version, VERSION);
    assert_eq!(client.welcome().max_radius, 533.0);
    let zone = Zone {
        map: 571,
        instance: 0,
        centre: [5800.0, 640.0, 650.0],
        radius: 400.0,
    };
    client.send(&ToObserver::Subscribe(zone)).unwrap();
    assert!(observer.wait_for(WAIT, |received| received.contains(&ToObserver::Subscribe(zone))));

    // Nothing comes: none, within the time given.
    assert!(client.receive(Duration::from_millis(30)).unwrap().is_none());

    // A snapshot cut in pieces of 7 bytes arrives whole.
    let snapshot = FromObserver::Snapshot {
        map: 571,
        instance: 0,
        sequence: 1,
        entities: vec![creature(), game_object()],
    };
    for piece in encode_from_observer(&snapshot).chunks(7) {
        observer.send_raw(piece.to_vec());
    }
    let mut got = None;
    for _ in 0..100 {
        got = client.receive(Duration::from_millis(50)).unwrap();
        if got.is_some() {
            break;
        }
    }
    assert_eq!(got, Some(snapshot));

    // Two messages in one write arrive one after the other.
    let mut two = encode_from_observer(&FromObserver::Status {
        state: State::Active,
        map: 571,
        instance: 0,
        radius: 400.0,
    });
    two.extend(encode_from_observer(&FromObserver::Changes {
        map: 571,
        instance: 0,
        sequence: 2,
        entities: vec![],
        left: vec![1],
    }));
    observer.send_raw(two);
    assert!(matches!(
        client.receive(WAIT).unwrap(),
        Some(FromObserver::Status { .. })
    ));
    assert!(matches!(
        client.receive(WAIT).unwrap(),
        Some(FromObserver::Changes { .. })
    ));
}

#[test]
fn a_wrong_token_a_closed_connection_and_a_message_too_long_are_told_apart() {
    let observer = FakeObserver::start("secret").unwrap();
    match Client::connect(observer.address(), "not it", WAIT) {
        Err(Error::Refused(reason)) => assert_eq!(reason, "wrong token"),
        other => panic!("{:?}", other.map(|_| ())),
    }

    let mut client = Client::connect(observer.address(), "secret", WAIT).unwrap();
    observer.drop_connection();
    let mut closed = None;
    for _ in 0..100 {
        match client.receive(Duration::from_millis(50)) {
            Ok(None) => {}
            other => {
                closed = Some(other);
                break;
            }
        }
    }
    assert!(matches!(closed, Some(Err(Error::Closed | Error::Io(_)))), "{closed:?}");

    let mut client = Client::connect(observer.address(), "secret", WAIT).unwrap();
    let mut huge = ((MAX_FROM_OBSERVER + 1) as u32).to_le_bytes().to_vec();
    huge.push(104);
    observer.send_raw(huge);
    let mut error = None;
    for _ in 0..100 {
        if let Err(problem) = client.receive(Duration::from_millis(50)) {
            error = Some(problem);
            break;
        }
    }
    assert!(
        matches!(error, Some(Error::Protocol(ref problem)) if problem.contains("bytes")),
        "{error:?}"
    );
    assert_eq!(observer.accepted(), 3);
}

#[test]
fn nothing_listening_is_an_error_of_the_connection() {
    let free = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    assert!(matches!(
        Client::connect(free, "secret", Duration::from_millis(500)),
        Err(Error::Io(_))
    ));
}
