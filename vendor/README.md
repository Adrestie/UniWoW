# Crates of other authors changed for UniWoW

Each is the crate as published, with its licence, and the changes below, marked `UniWoW:` in its
code. The workspace excludes this folder and takes these copies through `[patch.crates-io]`.

## mysql 28.0.4 (MIT or Apache 2.0)

`sha256_password` without TLS, which the crate does not have: MySQL 8.4 switches an account of that
plugin to it, the client asks the server's public key (0x01), then sends the password, ended by a
zero and XORed with the scramble, encrypted with it (RSA OAEP), as it does for the full
authentication of `caching_sha2_password`; over TLS or a socket, the password itself
(`src/conn/mod.rs`: `perform_auth_switch`, `continue_auth`, `continue_sha256_password_auth`).
Tested against a server the tests of `libs/db` run (`libs/db/src/fake_server.rs`).
