//! The little bit of websocket the docked DevTools bridge needs, written by
//! hand so the project stays dependency-free (see `devtools`).
//!
//! The frontend connects to the loopback listener as an ordinary websocket
//! client, so the server has to speak RFC 6455: complete the upgrade handshake,
//! unmask what arrives, and frame what goes out. Only the parts the DevTools
//! frontend actually uses are implemented — text, ping/pong, close, and the
//! continuation frames it splits large payloads into.
//!
//! Everything in here is pure except [`Ws::accept`], which wraps a
//! [`TcpStream`], so the framing can be exercised without a browser.

use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// The constant every websocket handshake mixes into its key (RFC 6455 §1.3).
const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Opcode for the frames the bridge speaks.
pub const TEXT: u8 = 0x1;
pub const BINARY: u8 = 0x2;
pub const CLOSE: u8 = 0x8;
pub const PING: u8 = 0x9;
pub const PONG: u8 = 0xA;

fn sha1(data: &[u8]) -> [u8; 20] {
  let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
  let bitlen = (data.len() as u64) * 8;
  let mut msg = data.to_vec();
  msg.push(0x80);
  while msg.len() % 64 != 56 {
    msg.push(0);
  }
  msg.extend_from_slice(&bitlen.to_be_bytes());
  for chunk in msg.chunks(64) {
    let mut w = [0u32; 80];
    for i in 0..16 {
      w[i] = u32::from_be_bytes([chunk[i * 4], chunk[i * 4 + 1], chunk[i * 4 + 2], chunk[i * 4 + 3]]);
    }
    for i in 16..80 {
      w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
    }
    let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
    for i in 0..80 {
      let (f, k) = match i {
        0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
        20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
        40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
        _ => (b ^ c ^ d, 0xCA62C1D6),
      };
      let t = a
        .rotate_left(5)
        .wrapping_add(f)
        .wrapping_add(e)
        .wrapping_add(k)
        .wrapping_add(w[i]);
      e = d;
      d = c;
      c = b.rotate_left(30);
      b = a;
      a = t;
    }
    h[0] = h[0].wrapping_add(a);
    h[1] = h[1].wrapping_add(b);
    h[2] = h[2].wrapping_add(c);
    h[3] = h[3].wrapping_add(d);
    h[4] = h[4].wrapping_add(e);
  }
  let mut out = [0u8; 20];
  for i in 0..5 {
    out[i * 4..i * 4 + 4].copy_from_slice(&h[i].to_be_bytes());
  }
  out
}

fn b64(bytes: &[u8]) -> String {
  const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  let mut out = String::new();
  for c in bytes.chunks(3) {
    let n = c.len();
    let mut b = [0u8; 3];
    b[..n].copy_from_slice(c);
    let x = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
    out.push(T[((x >> 18) & 63) as usize] as char);
    out.push(T[((x >> 12) & 63) as usize] as char);
    out.push(if n > 1 { T[((x >> 6) & 63) as usize] as char } else { '=' });
    out.push(if n > 2 { T[(x & 63) as usize] as char } else { '=' });
  }
  out
}

/// The value the handshake must echo back for a client's `Sec-WebSocket-Key`.
pub fn accept_key(key: &str) -> String {
  b64(&sha1(format!("{}{}", key, GUID).as_bytes()))
}

/// The reply to a client's upgrade request, plus the path it asked for.
///
/// Returns `None` for anything that is not a usable websocket upgrade (no
/// request line, no key, or a request that never terminated).
pub fn handshake(request: &str) -> Option<(String, String)> {
  let path = request.lines().next()?.split_whitespace().nth(1)?.to_string();
  let key = request
    .lines()
    .find(|l| l.to_ascii_lowercase().starts_with("sec-websocket-key:"))
    .map(|l| l.splitn(2, ':').nth(1).unwrap_or("").trim().to_string())?;
  if key.is_empty() {
    return None;
  }
  let resp = format!(
    "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
    accept_key(&key)
  );
  Some((path, resp))
}

/// One frame off the wire, mask already removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
  pub op: u8,
  pub fin: bool,
  pub payload: Vec<u8>,
}

impl Frame {
  fn new(op: u8, fin: bool, payload: Vec<u8>) -> Self {
    Self { op, fin, payload }
  }
}

/// Pop one complete frame off `buf`, leaving whatever follows it in place.
///
/// Returns `None` when the buffer does not yet hold a whole frame — the caller
/// keeps it and reads on, which is how a payload split across TCP segments is
/// handled.
pub fn take_frame(buf: &mut Vec<u8>) -> Option<Frame> {
  if buf.len() < 2 {
    return None;
  }
  let fin = buf[0] & 0x80 != 0;
  let op = buf[0] & 0x0f;
  let masked = buf[1] & 0x80 != 0;
  let mut len = (buf[1] & 0x7f) as usize;
  let mut off = 2;
  if len == 126 {
    if buf.len() < 4 {
      return None;
    }
    len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
    off = 4;
  } else if len == 127 {
    if buf.len() < 10 {
      return None;
    }
    let mut a = [0u8; 8];
    a.copy_from_slice(&buf[2..10]);
    len = u64::from_be_bytes(a) as usize;
    off = 10;
  }
  let mask = if masked {
    if buf.len() < off + 4 {
      return None;
    }
    let m = [buf[off], buf[off + 1], buf[off + 2], buf[off + 3]];
    off += 4;
    Some(m)
  } else {
    None
  };
  if buf.len() < off + len {
    return None;
  }
  let mut payload = buf[off..off + len].to_vec();
  if let Some(m) = mask {
    for (i, b) in payload.iter_mut().enumerate() {
      *b ^= m[i % 4];
    }
  }
  buf.drain(..off + len);
  Some(Frame::new(op, fin, payload))
}

/// Serialise one unmasked server frame (servers never mask).
pub fn encode_frame(op: u8, payload: &[u8]) -> Vec<u8> {
  let mut f = Vec::with_capacity(payload.len() + 10);
  f.push(0x80 | op);
  let n = payload.len();
  if n < 126 {
    f.push(n as u8);
  } else if n < 65536 {
    f.push(126);
    f.extend_from_slice(&(n as u16).to_be_bytes());
  } else {
    f.push(127);
    f.extend_from_slice(&(n as u64).to_be_bytes());
  }
  f.extend_from_slice(payload);
  f
}

/// A websocket connection to the panel's frontend.
pub struct Ws {
  s: TcpStream,
}

impl Ws {
  /// Complete the upgrade handshake; returns the connection and the request path.
  pub fn accept(mut s: TcpStream) -> Option<(Ws, String)> {
    let mut req = Vec::new();
    let mut tmp = [0u8; 2048];
    loop {
      let n = s.read(&mut tmp).ok()?;
      if n == 0 || req.len() > 16384 {
        return None;
      }
      req.extend_from_slice(&tmp[..n]);
      if req.windows(4).any(|w| w == b"\r\n\r\n") {
        break;
      }
    }
    let (path, resp) = handshake(&String::from_utf8_lossy(&req))?;
    s.write_all(resp.as_bytes()).ok()?;
    // the serving loop polls this socket, so a read timeout is the tick it runs on
    let _ = s.set_read_timeout(Some(Duration::from_millis(25)));
    Some((Ws { s }, path))
  }

  /// One read into `buf`; `Ok(0)` means the frontend went away, a timeout or a
  /// would-block means "nothing yet".
  pub fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
    self.s.read(buf)
  }

  pub fn is_idle(err: &std::io::Error) -> bool {
    matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
  }

  pub fn text(&mut self, msg: &str) {
    self.send(TEXT, msg.as_bytes());
  }

  pub fn pong(&mut self, payload: &[u8]) {
    self.send(PONG, payload);
  }

  pub fn close(&mut self) {
    self.send(CLOSE, &[]);
  }

  fn send(&mut self, op: u8, payload: &[u8]) {
    let _ = self.s.write_all(&encode_frame(op, payload));
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// An independent client-side encoder, so the decoder is never checked
  /// against its own inverse. Clients always mask; `fin` and `op` are explicit
  /// so fragmented and control frames can be built too.
  fn client_frame(op: u8, fin: bool, payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
    let mut f = Vec::new();
    f.push(if fin { 0x80 } else { 0x00 } | op);
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
    f.extend_from_slice(&mask);
    for (i, b) in payload.iter().enumerate() {
      f.push(b ^ mask[i % 4]);
    }
    f
  }

  #[test]
  fn sha1_matches_known_digests() {
    assert_eq!(
      sha1(b"").iter().map(|b| format!("{:02x}", b)).collect::<String>(),
      "da39a3ee5e6b4b0d3255bfef95601890afd80709"
    );
    // 448-bit and multi-block inputs, to cover the padding boundaries
    assert_eq!(
      sha1(b"abc").iter().map(|b| format!("{:02x}", b)).collect::<String>(),
      "a9993e364706816aba3e25717850c26c9cd0d89d"
    );
    assert_eq!(
      sha1(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>(),
      "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
    );
  }

  #[test]
  fn base64_matches_rfc4648() {
    assert_eq!(b64(b""), "");
    assert_eq!(b64(b"f"), "Zg==");
    assert_eq!(b64(b"fo"), "Zm8=");
    assert_eq!(b64(b"foo"), "Zm9v");
    assert_eq!(b64(b"foob"), "Zm9vYg==");
    assert_eq!(b64(b"fooba"), "Zm9vYmE=");
    assert_eq!(b64(b"foobar"), "Zm9vYmFy");
  }

  #[test]
  fn accept_key_matches_the_rfc_example() {
    assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
  }

  #[test]
  fn handshake_answers_an_upgrade_and_reports_its_path() {
    let req = "GET /devtools/page/abc123 HTTP/1.1\r\nHost: 127.0.0.1:1234\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n";
    let (path, resp) = handshake(req).expect("a valid upgrade");
    assert_eq!(path, "/devtools/page/abc123");
    assert!(resp.starts_with("HTTP/1.1 101"));
    assert!(resp.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo="));
    assert!(resp.ends_with("\r\n\r\n"));
  }

  #[test]
  fn handshake_rejects_what_it_cannot_serve() {
    assert!(handshake("").is_none());
    assert!(handshake("GET /x HTTP/1.1\r\n\r\n").is_none(), "no key");
    assert!(handshake("GET /x HTTP/1.1\r\nSec-WebSocket-Key:   \r\n\r\n").is_none(), "empty key");
    // the header name is matched case-insensitively, as clients vary
    let odd = "GET /p HTTP/1.1\r\nSEC-WEBSOCKET-KEY: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n";
    assert!(handshake(odd).is_some());
  }

  #[test]
  fn take_frame_unmasks_a_short_payload() {
    let mask = [0x37, 0xfa, 0x21, 0x3d];
    let mut buf = client_frame(TEXT, true, b"hello", mask);
    let f = take_frame(&mut buf).expect("a whole frame");
    assert_eq!(f.op, TEXT);
    assert!(f.fin);
    assert_eq!(f.payload, b"hello");
    assert!(buf.is_empty(), "the frame is consumed");
  }

  #[test]
  fn take_frame_waits_for_the_rest_of_a_split_frame() {
    let mask = [1, 2, 3, 4];
    let whole = client_frame(TEXT, true, b"a long enough payload", mask);
    for cut in 1..whole.len() {
      let mut buf = whole[..cut].to_vec();
      assert!(take_frame(&mut buf).is_none(), "cut at {} must not decode yet", cut);
      assert_eq!(buf.len(), cut, "nothing may be consumed before the frame is whole");
    }
  }

  #[test]
  fn take_frame_reads_one_of_two_coalesced_frames() {
    let mask = [9, 8, 7, 6];
    let mut buf = client_frame(TEXT, true, b"first", mask);
    buf.extend(client_frame(TEXT, true, b"second", mask));
    let a = take_frame(&mut buf).expect("the first");
    assert_eq!(a.payload, b"first");
    let b = take_frame(&mut buf).expect("the second");
    assert_eq!(b.payload, b"second");
    assert!(buf.is_empty());
    assert!(take_frame(&mut buf).is_none());
  }

  #[test]
  fn take_frame_handles_the_extended_length_forms() {
    let idx = [5, 5, 5, 5];
    for len in [0usize, 125, 126, 300, 65535, 65536, 70000] {
      let payload: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
      let mut buf = client_frame(BINARY, true, &payload, idx);
      let f = take_frame(&mut buf).expect("a whole frame");
      assert_eq!(f.payload.len(), len);
      assert_eq!(f.payload, payload);
      assert!(buf.is_empty());
    }
  }

  #[test]
  fn take_frame_reports_fragmentation() {
    let mask = [3, 1, 4, 1];
    // a continuation frame is what the frontend sends for a large JSON payload
    let mut buf = client_frame(TEXT, false, b"half ", mask);
    let a = take_frame(&mut buf).expect("first half");
    assert!(!a.fin);
    assert_eq!(a.op, TEXT);
    let mut buf = client_frame(0x0, true, b"and half", mask);
    let b = take_frame(&mut buf).expect("second half");
    assert!(b.fin);
    assert_eq!(b.op, 0x0);
  }

  #[test]
  fn take_frame_passes_control_frames_through_unmasked_too() {
    // the server side of the protocol never masks, and the codec must not care
    let mut buf = vec![0x88, 0x02, 0x03, 0xE8];
    let f = take_frame(&mut buf).expect("a close frame");
    assert_eq!(f.op, CLOSE);
    assert_eq!(f.payload, [0x03, 0xE8]);
  }

  #[test]
  fn encode_frame_picks_the_right_length_form() {
    let short = encode_frame(TEXT, b"hi");
    assert_eq!(short, vec![0x81, 0x02, b'h', b'i']);

    let mid = encode_frame(TEXT, &vec![0u8; 126]);
    assert_eq!(&mid[..4], &[0x81, 126, 0x00, 126]);
    assert_eq!(mid.len(), 4 + 126);

    let big = encode_frame(TEXT, &vec![0u8; 65536]);
    assert_eq!(&big[..2], &[0x81, 127]);
    assert_eq!(u64::from_be_bytes(big[2..10].try_into().unwrap()), 65536);
    assert_eq!(big.len(), 10 + 65536);

    // fin is always set, and the opcode survives
    assert_eq!(encode_frame(PONG, b"x")[0], 0x80 | PONG);
  }

  #[test]
  fn encode_then_take_round_trips_a_large_server_frame() {
    // the bridge pushes whole CDP payloads out as one unmasked text frame
    let payload = vec![b'{'; 70000];
    let mut buf = encode_frame(TEXT, &payload);
    let f = take_frame(&mut buf).expect("a whole frame");
    assert_eq!(f.payload.len(), 70000);
    assert_eq!(f.payload, payload);
  }
}
