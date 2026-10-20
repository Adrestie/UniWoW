//! The client of MySQL of the editor (step 15.11a), on the crate `mysql` without TLS, for the world
//! database of AzerothCore on this machine: a session set read only, one statement at a time
//! beginning with `SELECT`, `SHOW`, `DESCRIBE` or `EXPLAIN`, its rows given as JSON. The password is
//! given at each connection and never kept here, nor said in an error.

use std::time::Duration;

use mysql::prelude::Queryable;
use serde_json::Value;

/// Where the database is, and as whom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub database: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_owned(),
            port: 3306,
            user: "acore".to_owned(),
            database: "acore_world".to_owned(),
        }
    }
}

/// The rows of a query: its columns, then each row's values in their order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

/// Whether `sql` may run: one statement, beginning with `SELECT`, `SHOW`, `DESCRIBE` or `EXPLAIN`.
pub fn allowed(sql: &str) -> Result<(), String> {
    let trimmed = sql.trim().trim_end_matches(';').trim();
    let first = trimmed
        .split(|c: char| c.is_whitespace() || c == '(')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if !matches!(first.as_str(), "SELECT" | "SHOW" | "DESCRIBE" | "DESC" | "EXPLAIN") {
        return Err(format!(
            "only SELECT, SHOW, DESCRIBE and EXPLAIN run here, the database being read only: not '{first}'"
        ));
    }
    // A second statement after a semicolon, outside the quotes.
    let mut quote: Option<char> = None;
    for c in trimmed.chars() {
        match (quote, c) {
            (None, '\'' | '"' | '`') => quote = Some(c),
            (Some(open), _) if c == open => quote = None,
            (None, ';') => return Err("one statement at a time".to_owned()),
            _ => {}
        }
    }
    Ok(())
}

/// `error` said without `password`, whatever the error is.
fn said(error: impl std::fmt::Display, password: &str) -> String {
    let mut text = error.to_string();
    if !password.is_empty() {
        text = text.replace(password, "…");
    }
    // A plugin of authentication this client has not: what the account can be given instead.
    if text.contains("Unknown authentication protocol") {
        text.push_str(
            ". The account's plugin is not supported: caching_sha2_password, mysql_native_password and              sha256_password are (ALTER USER 'user'@'host' IDENTIFIED WITH caching_sha2_password BY '…')",
        );
    }
    text
}

/// A connection to the database, its session read only.
pub struct Connection {
    conn: mysql::Conn,
}

impl Connection {
    /// Connects as `settings` say with `password`, within `timeout`, and sets the session read only.
    /// An error never holds the password.
    pub fn open(settings: &Settings, password: &str, timeout: Duration) -> Result<Self, String> {
        let options = mysql::OptsBuilder::new()
            .ip_or_hostname(Some(settings.host.clone()))
            .tcp_port(settings.port)
            .user(Some(settings.user.clone()))
            .pass(Some(password.to_owned()))
            .db_name(Some(settings.database.clone()))
            .prefer_socket(false)
            .tcp_connect_timeout(Some(timeout))
            .read_timeout(Some(timeout * 3))
            .write_timeout(Some(timeout));
        let mut conn = mysql::Conn::new(options).map_err(|error| said(error, password))?;
        conn.query_drop("SET SESSION TRANSACTION READ ONLY")
            .map_err(|error| said(error, password))?;
        Ok(Self { conn })
    }

    /// The version of the server.
    pub fn version(&mut self) -> Result<String, String> {
        let version: Option<String> = self.conn.query_first("SELECT VERSION()").map_err(|e| e.to_string())?;
        Ok(version.unwrap_or_default())
    }

    /// The rows of `sql`, its `?` given `parameters` in their order; refused unless `allowed`.
    pub fn query(&mut self, sql: &str, parameters: &[Value]) -> Result<Rows, String> {
        allowed(sql)?;
        let parameters: Vec<mysql::Value> = parameters.iter().map(to_mysql).collect();
        let parameters = if parameters.is_empty() {
            mysql::Params::Empty
        } else {
            mysql::Params::Positional(parameters)
        };
        let result: Vec<mysql::Row> = self.conn.exec(sql, parameters).map_err(|e| e.to_string())?;
        let columns = result
            .first()
            .map(|row| {
                row.columns_ref()
                    .iter()
                    .map(|column| column.name_str().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        let rows = result
            .into_iter()
            .map(|row| row.unwrap().into_iter().map(from_mysql).collect())
            .collect();
        Ok(Rows { columns, rows })
    }
}

fn to_mysql(value: &Value) -> mysql::Value {
    match value {
        Value::Null => mysql::Value::NULL,
        Value::Bool(value) => mysql::Value::Int(i64::from(*value)),
        Value::Number(number) => match (number.as_i64(), number.as_u64(), number.as_f64()) {
            (Some(value), _, _) => mysql::Value::Int(value),
            (None, Some(value), _) => mysql::Value::UInt(value),
            (_, _, Some(value)) => mysql::Value::Double(value),
            _ => mysql::Value::NULL,
        },
        Value::String(text) => mysql::Value::Bytes(text.clone().into_bytes()),
        other => mysql::Value::Bytes(other.to_string().into_bytes()),
    }
}

fn from_mysql(value: mysql::Value) -> Value {
    match value {
        mysql::Value::NULL => Value::Null,
        mysql::Value::Int(value) => Value::from(value),
        mysql::Value::UInt(value) => Value::from(value),
        mysql::Value::Float(value) => Value::from(value),
        mysql::Value::Double(value) => Value::from(value),
        mysql::Value::Bytes(bytes) => Value::from(String::from_utf8_lossy(&bytes).into_owned()),
        mysql::Value::Date(year, month, day, hour, minute, second, _) => Value::from(format!(
            "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}"
        )),
        mysql::Value::Time(negative, days, hours, minutes, seconds, _) => Value::from(format!(
            "{}{}:{minutes:02}:{seconds:02}",
            if negative { "-" } else { "" },
            u64::from(days) * 24 + u64::from(hours)
        )),
    }
}

pub mod secret;

#[cfg(test)]
mod fake_server;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_statement_that_reads_is_allowed() {
        for sql in [
            "SELECT guid FROM creature WHERE map = ?",
            "  select 1;",
            "SHOW TABLES",
            "DESCRIBE creature",
            "EXPLAIN SELECT 1",
            "SELECT ';' AS semicolon",
            "SELECT(1)",
        ] {
            assert!(allowed(sql).is_ok(), "{sql}");
        }
        for sql in [
            "UPDATE creature SET map = 1",
            "DELETE FROM creature",
            "SELECT 1; DROP TABLE creature",
            "INSERT INTO x VALUES (1)",
            "",
            "CALL something()",
        ] {
            assert!(allowed(sql).is_err(), "{sql}");
        }
    }

    #[test]
    fn a_connection_refused_never_says_the_password() {
        // A port nothing listens on, on this machine.
        let settings = Settings {
            port: 1,
            ..Settings::default()
        };
        let password = "s3cr3t-of-the-test";
        let error = Connection::open(&settings, password, Duration::from_millis(500))
            .err()
            .expect("refused");
        assert!(!error.contains(password), "{error}");
        assert_eq!(
            said(format!("Access denied for {password}"), password),
            "Access denied for …"
        );
        let unknown = said("Unknown authentication protocol: `authentication_ldap_sasl`", password);
        assert!(unknown.contains("ALTER USER"), "{unknown}");
    }

    #[test]
    fn an_account_of_sha256_password_connects_without_tls_its_password_encrypted() {
        let (port, server) = fake_server::start();
        let settings = Settings {
            port,
            ..Settings::default()
        };
        let password = "s3cr3t-of-the-test";
        let connection = Connection::open(&settings, password, Duration::from_secs(5));
        let failed = connection.as_ref().err().cloned();
        drop(connection);
        let seen = server.join().expect("the server").expect("served");
        assert!(failed.is_none(), "{failed:?}");
        assert_eq!(seen.asked_key, [0x01], "the public key asked for, without TLS");
        assert_eq!(seen.encrypted.len(), 256, "encrypted with the key of 2048 bits");
        let sent = String::from_utf8_lossy(&seen.encrypted);
        assert!(!sent.contains(password), "never in clear");
        assert!(
            seen.commands
                .iter()
                .any(|command| command.ends_with(b"SET SESSION TRANSACTION READ ONLY")),
            "the session read only"
        );
    }

    #[test]
    fn values_cross_both_ways() {
        assert_eq!(
            from_mysql(mysql::Value::Bytes(b"Marshal Dughan".to_vec())),
            Value::from("Marshal Dughan")
        );
        assert_eq!(from_mysql(mysql::Value::Int(-3)), Value::from(-3));
        assert_eq!(from_mysql(mysql::Value::NULL), Value::Null);
        assert_eq!(to_mysql(&Value::from(571)), mysql::Value::Int(571));
        assert_eq!(to_mysql(&Value::from("x")), mysql::Value::Bytes(b"x".to_vec()));
    }

    /// Against the user's server when `UNIWOW_MYSQL` names it, `user:password@host:port/database`;
    /// skipped otherwise.
    #[test]
    fn the_world_database_is_read_when_one_is_named() {
        let Ok(named) = std::env::var("UNIWOW_MYSQL") else {
            eprintln!("skipped: UNIWOW_MYSQL names no database");
            return;
        };
        let (account, place) = named.split_once('@').expect("user:password@host:port/database");
        let (user, password) = account.split_once(':').expect("user:password");
        let (address, database) = place.split_once('/').expect("host:port/database");
        let (host, port) = address.split_once(':').unwrap_or((address, "3306"));
        let settings = Settings {
            host: host.to_owned(),
            port: port.parse().expect("a port"),
            user: user.to_owned(),
            database: database.to_owned(),
        };
        let mut connection = Connection::open(&settings, password, Duration::from_secs(5)).expect("connected");
        eprintln!("MySQL {}", connection.version().unwrap());
        let started = std::time::Instant::now();
        let rows = connection
            .query(
                "SELECT guid, id1, map, position_x, position_y, position_z FROM creature WHERE map = ?",
                &[Value::from(571)],
            )
            .unwrap();
        eprintln!(
            "{} creatures of Northrend read in {:?}",
            rows.rows.len(),
            started.elapsed()
        );
        assert!(connection.query("UPDATE creature SET map = map", &[]).is_err());
    }
}
