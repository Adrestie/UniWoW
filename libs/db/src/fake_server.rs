//! A server of MySQL the tests run on this machine: its handshake asks `caching_sha2_password`,
//! then switches the client to `sha256_password`, gives its public key when asked, takes the
//! password encrypted and answers every command after with OK, as MySQL 8.4 does for an account of
//! that plugin over a connection without TLS.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::JoinHandle;

/// A public key of RSA of 2048 bits, of the tests alone.
pub const PUBLIC_KEY: &str = "-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA6zRhlXZu+ox4TWPVvG32
bllWMBqUU6B3UuOX/CULSaG56Pfey5VxfBrFjh9DHeSE5/yPpeLPNu1xC7RbcIqP
SLMErMizK3AlmKexEjikIkGy3kH7dKbxAwJ6nl4icVo5orAguhVfQCDXA1xfacEw
ELlvvuq91VXp3guilBtIfRPVmv0ljH7f3rbpUj4bvDTp00HUUqeNOqfubHnzYmdw
g8P4HOtUq4QtOJf9SVDOJDWvp19o6ihsjCE5SDo49mn0CjddT2gVhHK360GaWPPe
rYALyTzBQYnTeRtZ2+lA0LoyNh4EjXE8x9Wv0sSR7TrAdMNv8dsrqePGwEWjsVYz
pQIDAQAB
-----END PUBLIC KEY-----
";

/// What the server saw of the client: what it sent when switched to `sha256_password`, the
/// encrypted password, and the commands after.
#[derive(Debug, Default)]
pub struct Seen {
    pub asked_key: Vec<u8>,
    pub encrypted: Vec<u8>,
    pub commands: Vec<Vec<u8>>,
}

fn read(stream: &mut TcpStream) -> std::io::Result<(u8, Vec<u8>)> {
    let mut header = [0u8; 4];
    stream.read_exact(&mut header)?;
    let length = u32::from_le_bytes([header[0], header[1], header[2], 0]) as usize;
    let mut payload = vec![0u8; length];
    stream.read_exact(&mut payload)?;
    Ok((header[3], payload))
}

fn write(stream: &mut TcpStream, sequence: u8, payload: &[u8]) -> std::io::Result<()> {
    let length = (payload.len() as u32).to_le_bytes();
    stream.write_all(&[length[0], length[1], length[2], sequence])?;
    stream.write_all(payload)
}

/// An OK packet: no rows changed, the session in autocommit.
const OK: [u8; 7] = [0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00];

fn serve(mut stream: TcpStream) -> std::io::Result<Seen> {
    let mut seen = Seen::default();
    let nonce: Vec<u8> = (1..=20).collect();
    // The capabilities of MySQL 8.4 without TLS: protocol 4.1, plugins, attributes; the results
    // ended by EOF packets.
    let capabilities: u32 = 0x003F_A20F;
    let mut handshake = vec![0x0a];
    handshake.extend(b"8.4.8\0");
    handshake.extend(7u32.to_le_bytes());
    handshake.extend(&nonce[..8]);
    handshake.push(0);
    handshake.extend((capabilities as u16).to_le_bytes());
    handshake.push(0x21);
    handshake.extend(2u16.to_le_bytes());
    handshake.extend(((capabilities >> 16) as u16).to_le_bytes());
    handshake.push(21);
    handshake.extend([0u8; 10]);
    handshake.extend(&nonce[8..]);
    handshake.push(0);
    handshake.extend(b"caching_sha2_password\0");
    write(&mut stream, 0, &handshake)?;
    let (sequence, _response) = read(&mut stream)?;
    let mut switch = vec![0xfe];
    switch.extend(b"sha256_password\0");
    switch.extend(&nonce);
    switch.push(0);
    write(&mut stream, sequence + 1, &switch)?;
    let (sequence, asked) = read(&mut stream)?;
    seen.asked_key = asked;
    let mut key = vec![0x01];
    key.extend(PUBLIC_KEY.as_bytes());
    write(&mut stream, sequence + 1, &key)?;
    let (sequence, encrypted) = read(&mut stream)?;
    seen.encrypted = encrypted;
    write(&mut stream, sequence + 1, &OK)?;
    // Every command answered OK, until the client quits or leaves.
    while let Ok((sequence, command)) = read(&mut stream) {
        if command.first() == Some(&0x01) {
            break;
        }
        // A system variable asked for: one row, its value.
        if let Some(name) = command.strip_prefix(b"SELECT @@") {
            let value = match name {
                b"max_allowed_packet" => "67108864",
                _ => "0",
            };
            let mut column = Vec::new();
            for text in [&b"def"[..], b"", b"", b"", &command[8..], b""] {
                column.push(text.len() as u8);
                column.extend(text);
            }
            column.push(0x0c);
            column.extend(0x3fu16.to_le_bytes());
            column.extend(20u32.to_le_bytes());
            column.push(0x08);
            column.extend([0u8; 5]);
            let mut row = vec![value.len() as u8];
            row.extend(value.as_bytes());
            let end = [0xfe, 0x00, 0x00, 0x02, 0x00];
            for (at, packet) in [&[1u8][..], &column, &end, &row, &end].into_iter().enumerate() {
                write(&mut stream, sequence + 1 + at as u8, packet)?;
            }
            seen.commands.push(command);
            continue;
        }
        seen.commands.push(command);
        write(&mut stream, sequence + 1, &OK)?;
    }
    Ok(seen)
}

/// A server listening on a port of this machine for one connection: its port, and what it saw
/// once the client is gone.
pub fn start() -> (u16, JoinHandle<std::io::Result<Seen>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a port of this machine");
    let port = listener.local_addr().expect("its address").port();
    let thread = std::thread::spawn(move || {
        let (stream, _) = listener.accept()?;
        serve(stream)
    });
    (port, thread)
}
