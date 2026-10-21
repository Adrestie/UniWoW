//! The servers the user declares (step 16.7): each its world database, its observer and its bridge,
//! kept in the module's settings (`servers`, the one chosen `server`); the password of each in the
//! Credential Manager of Windows. One is chosen at a time, from the panel or the toolbar of the 3D
//! view; the modules that read the database or follow the world read the new one.

use uniwow_api::db::Settings;
use uniwow_api::serde_json::{Value, json};
use uniwow_api::world_db::Server;

/// The keys of the settings: the servers declared, the one chosen.
pub const SERVERS: &str = "servers";
pub const CHOSEN: &str = "server";

pub fn to_json(server: &Server) -> Value {
    json!({
        "name": server.name,
        "host": server.host,
        "port": server.port,
        "user": server.user,
        "database": server.database,
        "observer_port": server.observer_port,
        "observer_token": server.observer_token,
        "world_port": server.world_port,
        "bridge_port": server.bridge_port,
        "bridge_token": server.bridge_token,
        "sql_folder": server.sql_folder,
    })
}

pub fn of_json(value: &Value) -> Option<Server> {
    let text = |key: &str, default: &str| value[key].as_str().unwrap_or(default).to_owned();
    let port = |key: &str, default: u16| {
        value[key]
            .as_u64()
            .and_then(|port| u16::try_from(port).ok())
            .unwrap_or(default)
    };
    let name = value["name"]
        .as_str()
        .filter(|name| !name.trim().is_empty())?
        .to_owned();
    Some(Server {
        name,
        host: text("host", "127.0.0.1"),
        port: port("port", 3306),
        user: text("user", "acore"),
        database: text("database", "acore_world"),
        observer_port: port("observer_port", 8087),
        observer_token: text("observer_token", ""),
        world_port: port("world_port", 8085),
        bridge_port: port("bridge_port", 8092),
        bridge_token: text("bridge_token", ""),
        sql_folder: text("sql_folder", ""),
    })
}

/// The servers of the settings `servers`; none declared yet, one made of the settings of the module
/// before servers were (`old`, a key's value), named after its database.
pub fn read(servers: Option<&Value>, old: impl Fn(&str) -> Option<Value>) -> Vec<Server> {
    let declared: Vec<Server> = servers
        .and_then(Value::as_array)
        .map(|servers| servers.iter().filter_map(of_json).collect())
        .unwrap_or_default();
    if !declared.is_empty() {
        return declared;
    }
    let mut first = json!({});
    for key in [
        "host",
        "port",
        "user",
        "database",
        "bridge_port",
        "bridge_token",
        "sql_folder",
    ] {
        if let Some(value) = old(key) {
            first[key] = value;
        }
    }
    let database = first["database"].as_str().unwrap_or("acore_world").to_owned();
    first["name"] = json!(if database.contains("test") {
        "Test".to_owned()
    } else {
        database
    });
    of_json(&first).into_iter().collect()
}

/// The connection to the database of `server`.
pub fn settings(server: &Server) -> Settings {
    Settings {
        host: server.host.clone(),
        port: server.port,
        user: server.user.clone(),
        database: server.database.clone(),
    }
}

/// A name for a server added, not one of `servers`'.
pub fn new_name(servers: &[Server], wanted: &str) -> String {
    let taken = |name: &str| servers.iter().any(|server| server.name == name);
    if !taken(wanted) {
        return wanted.to_owned();
    }
    (2..)
        .map(|at| format!("{wanted} {at}"))
        .find(|name| !taken(name))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_servers_are_kept_in_the_settings_and_made_of_the_old_ones_at_first() {
        let test = Server {
            name: "Test".to_owned(),
            host: "127.0.0.1".to_owned(),
            port: 3306,
            user: "root".to_owned(),
            database: "acore_world_test".to_owned(),
            observer_port: 8088,
            observer_token: "secret".to_owned(),
            world_port: 8086,
            bridge_port: 8092,
            bridge_token: "secret".to_owned(),
            sql_folder: String::new(),
        };
        assert_eq!(of_json(&to_json(&test)), Some(test.clone()));
        assert_eq!(
            read(Some(&json!([to_json(&test)])), |_| None),
            std::slice::from_ref(&test)
        );
        // None declared: the old settings of the module, one server.
        let old = |key: &str| match key {
            "database" => Some(json!("acore_world_test")),
            "user" => Some(json!("root")),
            "bridge_port" => Some(json!(8092)),
            _ => None,
        };
        let made = read(None, old);
        assert_eq!(made.len(), 1);
        assert_eq!(
            (made[0].name.as_str(), made[0].user.as_str(), made[0].bridge_port),
            ("Test", "root", 8092)
        );
        assert_eq!(settings(&made[0]).database, "acore_world_test");
        assert!(of_json(&json!({ "name": " " })).is_none(), "a server has a name");
        assert_eq!(new_name(std::slice::from_ref(&test), "Test"), "Test 2");
        assert_eq!(new_name(&[test], "Nibelung"), "Nibelung");
    }
}
