//! The docked DevTools panel talks to the browser over a loopback websocket that
//! this project serves by hand. These tests drive that socket for real — a real
//! `TcpListener`, a real handshake, real frames — because the framing is where a
//! mistake would be silent: a wrong accept key or a mis-parsed length would only
//! show up as a blank panel.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{channel, Receiver};
use std::thread;
use std::time::Duration;

use nanobrowser::ws::{take_frame, Frame, Ws, CLOSE, PING, PONG, TEXT};

/// A client-side frame the way a browser writes one: always masked.
fn client_frame(op: u8, payload: &[u8]) -> Vec<u8> {
  const MASK: [u8; 4] = [0x1f, 0x2e, 0x3d, 0x4c];
  let mut f = Vec::new();
  f.push(0x80 | op);
  let n = payload.len();
  if n < 126 {
    f.push(0x80 | n as u8);
  } else if n < 65536 {
    f.push(0x80 | 126);
    f.extend_from_slice(&(n as u16).to_be_bytes());
  } else {
    f.push(0x80 | 127);
    f.extend_from_slice(&(n as u64).to_be_bytes());
  }
  f.extend_from_slice(&MASK);
  for (i, b) in payload.iter().enumerate() {
    f.push(b ^ MASK[i % 4]);
  }
  f
}

fn upgrade_request(key: &str) -> String {
  format!(
    "GET /devtools/page/test-session HTTP/1.1\r\nHost: 127.0.0.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {}\r\nSec-WebSocket-Version: 13\r\n\r\n",
    key
  )
}

/// Read the HTTP response head off a socket, returning everything up to and
/// including the blank line that ends it.
fn read_head(s: &mut TcpStream) -> String {
  let mut buf = Vec::new();
  let mut byte = [0u8; 1];
  while !buf.ends_with(b"\r\n\r\n") && buf.len() < 4096 {
    match s.read(&mut byte) {
      Ok(0) | Err(_) => break,
      Ok(_) => buf.push(byte[0]),
    }
  }
  String::from_utf8_lossy(&buf).to_string()
}

/// Read frames off `s` until `want` is found, decoding with the same codec the
/// bridge uses. Returns the frame, or `None` if the socket went quiet first.
fn expect_frame(s: &mut TcpStream, want: u8) -> Option<Frame> {
  let mut buf = Vec::new();
  let mut tmp = [0u8; 1024];
  for _ in 0..200 {
    if let Some(f) = take_frame(&mut buf) {
      if f.op == want {
        return Some(f);
      }
    }
    s.set_read_timeout(Some(Duration::from_millis(50))).ok()?;
    match s.read(&mut tmp) {
      Ok(0) => return None,
      Ok(n) => buf.extend_from_slice(&tmp[..n]),
      Err(_) => {}
    }
  }
  None
}

/// Accept one panel connection on `listener` and hand it to the bridge codec.
fn serve_one(listener: TcpListener) -> Receiver<(String, Frame)> {
  let (tx, rx) = channel();
  thread::spawn(move || {
    let Ok((stream, _)) = listener.accept() else { return };
    let Some((mut ws, path)) = Ws::accept(stream) else { return };
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
      if let Some(f) = take_frame(&mut buf) {
        // answer the way the real bridge does, so the client can check replies
        match f.op {
          0x8 => {
            ws.close();
            break;
          }
          0x9 => ws.pong(&f.payload),
          _ => {
            ws.text(&format!("echo:{}", String::from_utf8_lossy(&f.payload)));
          }
        }
        let _ = tx.send((path.clone(), f));
      }
      match ws.read(&mut tmp) {
        Ok(0) => break,
        Ok(n) => buf.extend_from_slice(&tmp[..n]),
        Err(ref e) if Ws::is_idle(e) => {}
        Err(_) => break,
      }
    }
  });
  rx
}

#[test]
fn the_panel_handshake_completes_and_reports_its_session_path() {
  let listener = TcpListener::bind("127.0.0.1:0").unwrap();
  let addr = listener.local_addr().unwrap();
  let got = serve_one(listener);

  let mut s = TcpStream::connect(addr).unwrap();
  s.write_all(upgrade_request("dGhlIHNhbXBsZSBub25jZQ==").as_bytes()).unwrap();
  let head = read_head(&mut s);
  assert!(head.starts_with("HTTP/1.1 101 Switching Protocols"), "{}", head);
  // the key the client sent, hashed per RFC 6455: a mismatch here shows up as a
  // frontend that refuses the connection
  assert!(head.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo="), "{}", head);

  s.write_all(&client_frame(TEXT, b"{\"id\":1}")).unwrap();
  let (path, frame) = got.recv_timeout(Duration::from_secs(10)).unwrap();
  assert_eq!(path, "/devtools/page/test-session");
  assert_eq!(frame.payload, b"{\"id\":1}");
  let reply = expect_frame(&mut s, TEXT).expect("the bridge echoes back");
  assert_eq!(reply.payload, b"echo:{\"id\":1}");
}

#[test]
fn a_connection_without_the_session_key_is_answered_nothing() {
  // the loopback port is not a debugging port: a page that scans localhost and
  // knocks without the key must get no upgrade and no data
  let listener = TcpListener::bind("127.0.0.1:0").unwrap();
  let addr = listener.local_addr().unwrap();
  let (tx, rx) = channel();
  thread::spawn(move || {
    let (stream, _) = listener.accept().unwrap();
    let accepted = Ws::accept(stream).is_some();
    let _ = tx.send(accepted);
  });

  let mut s = TcpStream::connect(addr).unwrap();
  s.write_all(b"GET /devtools/page/guessed HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
  assert_eq!(rx.recv_timeout(Duration::from_secs(10)).unwrap(), false);
  // and nothing came back over the wire either
  s.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
  let mut buf = [0u8; 64];
  assert_eq!(s.read(&mut buf).unwrap_or(0), 0, "no bytes, no response head");
}

#[test]
fn a_large_payload_survives_being_split_across_reads() {
  // CDP replies routinely run past a single read, and the bridge buffers what it
  // has until the frame is whole. One 70 KiB write is enough to guarantee the
  // reader sees several pieces: `expect_frame` reads 1 KiB at a time.
  let listener = TcpListener::bind("127.0.0.1:0").unwrap();
  let addr = listener.local_addr().unwrap();
  thread::spawn(move || {
    let (stream, _) = listener.accept().unwrap();
    let (mut ws, _) = Ws::accept(stream).unwrap();
    ws.text(&"x".repeat(70_000));
    thread::sleep(Duration::from_millis(500));
  });

  let mut s = TcpStream::connect(addr).unwrap();
  s.write_all(upgrade_request("AAAAAAAAAAAAAAAAAAAAAA==").as_bytes()).unwrap();
  let head = read_head(&mut s);
  assert!(head.starts_with("HTTP/1.1 101"), "{}", head);
  let f = expect_frame(&mut s, TEXT).expect("the whole frame arrives");
  assert_eq!(f.payload.len(), 70_000);
  assert!(f.payload.iter().all(|b| *b == b'x'));
}

#[test]
fn ping_is_answered_and_close_ends_the_session() {
  let listener = TcpListener::bind("127.0.0.1:0").unwrap();
  let addr = listener.local_addr().unwrap();
  let _got = serve_one(listener);

  let mut s = TcpStream::connect(addr).unwrap();
  s.write_all(upgrade_request("AAAAAAAAAAAAAAAAAAAAAA==").as_bytes()).unwrap();
  let head = read_head(&mut s);
  assert!(head.starts_with("HTTP/1.1 101"), "{}", head);

  // a keepalive must come back as a pong with the same payload
  s.write_all(&client_frame(PING, b"ka")).unwrap();
  let pong = expect_frame(&mut s, PONG).expect("a pong");
  assert_eq!(pong.payload, b"ka");

  // and a close must be acknowledged with a close frame, not an echo
  s.write_all(&client_frame(CLOSE, &[])).unwrap();
  let closed = expect_frame(&mut s, CLOSE).expect("a close");
  assert!(closed.payload.is_empty());
}

#[test]
fn two_frames_arriving_in_one_read_are_both_delivered() {
  // a page can post two CDP calls back to back before the server reads
  let listener = TcpListener::bind("127.0.0.1:0").unwrap();
  let addr = listener.local_addr().unwrap();
  let got = serve_one(listener);

  let mut s = TcpStream::connect(addr).unwrap();
  s.write_all(upgrade_request("AAAAAAAAAAAAAAAAAAAAAA==").as_bytes()).unwrap();
  let _ = read_head(&mut s);
  let mut both = client_frame(TEXT, b"one");
  both.extend(client_frame(TEXT, b"two"));
  s.write_all(&both).unwrap();

  let (_, first) = got.recv_timeout(Duration::from_secs(10)).unwrap();
  let (_, second) = got.recv_timeout(Duration::from_secs(10)).unwrap();
  assert_eq!(first.payload, b"one");
  assert_eq!(second.payload, b"two");
}
