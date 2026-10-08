# cordial-protocol

The wire protocol between the [Cordial](https://github.com/luohoa97/cordial)
launcher and a **runtime**: the program that turns Play into a running Roblox
client. It is `cordial.runtime/1`, written down in
[`docs/runtime-spec.md`](https://github.com/luohoa97/cordial/blob/main/docs/runtime-spec.md),
and this crate is the Rust implementation of its pieces, so a launcher and a
runtime written by different people agree on the bytes.

It is plain JSON lines over a Unix socket: one object per line, UTF-8, at most
64 KiB a line. A line is a request (`m`), a reply (`ok`) or an event (`ev`).
The protocol carries events and effects. It never carries channels or code.

What is in the crate:

- **Framing**: `Frame`, `decode_line`, `encode_line`, and `LineReader`, a line
  reader with a ceiling, so a peer sending noise costs a bounded buffer.
- **Handshake**: `negotiate`, which takes the lower of the two minors and the
  lower version of each capability both sides have.
- **Messages**: typed payloads for every verb and event in version 1, in `msg`,
  with `msg::check` to validate one by name.
- **Manifest**: `Manifest` for `runtime.json`, and the expansion of its
  `{socket}`-style placeholders into single arguments.
- **Event queue**: `EventQueue`, the bounded drop-and-count queue a runtime puts
  between its engine threads and the socket.
- **Settings**: `settings`, the closed key set `settings.set` carries and the
  types behind it.
- **Socket**: `socket`, where `ctl.sock` lives and how both sides reach it when
  the path is too long for `sun_path`.
- **Conformance** (feature `conformance`): shared accept and reject line vectors
  and a harness either side can run against a peer over any `Read` and `Write`.

It depends on `serde` and `serde_json` and nothing else, links no native code,
and needs no async runtime.

## A runtime

Read lines, answer `hello`, refuse what you do not offer:

```rust
use cordial_protocol::msg::{self, names, caps, ClientIdent, HelloReply, RuntimeIdent};
use cordial_protocol::{Code, Frame, LineReader, Next, Protocol, Reply, encode_line, decode_line};
use std::io::{Read, Write};

fn serve(input: impl Read, mut output: impl Write) -> std::io::Result<()> {
    let mut lines = LineReader::new(input);
    loop {
        let line = match lines.next_line()? {
            Next::Line(line) => line,
            Next::Eof => return Ok(()),
            // A line that is not a frame closes this connection, never the runtime.
            _ => return Ok(()),
        };
        let Ok(Frame::Request(req)) = decode_line(&line) else { return Ok(()) };
        let reply = match req.m.as_str() {
            names::HELLO => msg::reply_ok(req.id, &HelloReply {
                protocol: Protocol::CURRENT,
                runtime: RuntimeIdent { id: "org.example.runtime".into(), version: "0.1".into() },
                client: ClientIdent { name: "Example".into(), version: "1".into(), build: "1".into() },
                caps: [(caps::LIFECYCLE.to_string(), 1)].into(),
            }),
            // An unknown request gets `unsupported`, never a silent success.
            other => Reply::err(req.id, Code::Unsupported, format!("{other} is not offered")),
        };
        output.write_all(encode_line(&Frame::Reply(reply)).as_bytes())?;
    }
}

let hello = r#"{"id":1,"m":"hello","p":{"protocol":{"major":1,"minor":0},"cordial":"0.25.0","session":"a1b2c3d4","caps":{"lifecycle":1}}}"#;
let mut out = Vec::new();
serve(format!("{hello}\n{{\"id\":2,\"m\":\"exec\"}}\n").as_bytes(), &mut out).unwrap();
let text = String::from_utf8(out).unwrap();
assert!(text.lines().next().unwrap().contains(r#""runtime":{"id":"org.example.runtime""#));
assert!(text.lines().nth(1).unwrap().contains(r#""code":"unsupported""#));
```

## A launcher

Send `hello`, read the reply, and work out what is live:

```rust
use cordial_protocol::msg::{self, names, Hello, HelloReply};
use cordial_protocol::{decode_line, encode_line, negotiate, Frame, Protocol, Request};

let offered = [("lifecycle".to_string(), 1), ("settings".to_string(), 1)].into();
let hello = Hello {
    protocol: Protocol::CURRENT,
    cordial: "0.25.0".into(),
    session: "a1b2c3d4".into(),
    caps: offered,
    reattach: false,
};
let line = encode_line(&Frame::Request(msg::request(1, names::HELLO, &hello)));
// ... write `line` to the runtime's socket and read one line back. Here, the
// reply a runtime that offers lifecycle and nothing else would send:
let answer = r#"{"id":1,"ok":true,"p":{"protocol":{"major":1,"minor":0},"runtime":{"id":"org.example.runtime","version":"0.1"},"client":{"name":"Example","version":"1","build":"1"},"caps":{"lifecycle":1}}}"#;

let Ok(Frame::Reply(reply)) = decode_line(answer) else { panic!("not a reply") };
let theirs: HelloReply = reply.payload().expect("a valid hello reply");
let live = negotiate(Protocol::CURRENT, &hello.caps, theirs.protocol, &theirs.caps).expect("same major");
// The runtime did not offer `settings`, so the launcher never sends it one.
assert!(msg::offered(names::LIFECYCLE_STOP, &live.caps));
assert!(!msg::offered(names::SETTINGS_SET, &live.caps));
assert!(line.ends_with('\n'));
```

## Testing a peer

With `features = ["conformance"]`, `conformance::run_against_runtime` drives a
runtime from the launcher's side and `conformance::run_against_launcher` drives a
launcher from the runtime's. Both take a closure that opens a connection, so the
same cases run over an in-process pair or a real socket, and
`conformance::run_vectors` checks a decoder against the shared lines in
`vectors/`. Give your streams a read timeout: a timed-out read is how "no reply"
is observed.

## The spec

[`docs/runtime-spec.md`](https://github.com/luohoa97/cordial/blob/main/docs/runtime-spec.md)
is the contract, and the crate follows it. Where the spec was ambiguous the crate
takes the simplest reading and the spec says which. A runtime in any language
can implement the protocol from the spec and the vectors without this crate.

## Licence

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. Copyright 2026 Neil Luo and Cordial contributors.

This crate is the one part of Cordial under these terms, so that other launchers
and runtimes can depend on it. The rest of Cordial is GPL-3.0-or-later.
Contributions to this crate are under the same two licences.
