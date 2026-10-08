//! A line reader with a ceiling.
//!
//! `BufRead::read_line` grows its buffer until it finds a newline, so a peer
//! that sends noise without one makes the reader allocate for as long as the
//! noise lasts. This reads at most [`MAX_LINE`] bytes of a line, and on
//! overflow throws the rest of that line away and says so, so one oversized
//! line costs the receiver a bounded buffer and one protocol error.

use std::io::{self, Read};

/// Longest line either side will accept, in bytes, newline not counted
/// (spec section 2: "at most 64 KiB a line").
pub const MAX_LINE: usize = 64 * 1024;

/// What one call to [`LineReader::next_line`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// A complete line, without its `\n` (and without a `\r` before it).
    Line(String),
    /// The line ran past the cap. Its remainder has been discarded up to the
    /// newline, so the next call starts on the following line.
    TooLong,
    /// A complete line that is not UTF-8.
    NotUtf8,
    /// The stream ended partway through a line. The fragment is dropped: a
    /// message with no terminator is a message that was cut off.
    Truncated,
    /// The stream ended between lines.
    Eof,
}

/// Reads `\n`-terminated lines from any [`Read`], bounded.
///
/// It buffers internally, so hand it the stream itself rather than a second
/// layer of buffering. A read timeout on the underlying stream surfaces as the
/// `io::Error` it is; this type has no clock of its own.
pub struct LineReader<R> {
    inner: R,
    cap: usize,
    // The line in progress. Kept across calls, so a read that times out in the
    // middle of a line loses nothing: the next call carries on from here.
    buf: Vec<u8>,
    overflowed: bool,
    // Bytes read from `inner` and not yet consumed.
    chunk: [u8; 4096],
    pos: usize,
    end: usize,
}

impl<R: Read> LineReader<R> {
    /// A reader with the protocol's cap, [`MAX_LINE`].
    pub fn new(inner: R) -> Self {
        Self::with_cap(inner, MAX_LINE)
    }

    /// A reader with a different cap, for a peer that wants a tighter bound than
    /// the protocol's.
    pub fn with_cap(inner: R, cap: usize) -> Self {
        LineReader { inner, cap, buf: Vec::new(), overflowed: false, chunk: [0; 4096], pos: 0, end: 0 }
    }

    pub fn into_inner(self) -> R {
        self.inner
    }

    pub fn get_ref(&self) -> &R {
        &self.inner
    }

    /// The next line, or why there is not one. `Err` is an I/O failure of the
    /// stream, including a timeout; every protocol-level fault is a [`Next`].
    pub fn next_line(&mut self) -> io::Result<Next> {
        loop {
            if self.pos == self.end {
                let n = loop {
                    match self.inner.read(&mut self.chunk) {
                        Ok(n) => break n,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(e) => return Err(e),
                    }
                };
                if n == 0 {
                    let cut = !self.buf.is_empty() || self.overflowed;
                    self.buf.clear();
                    self.overflowed = false;
                    return Ok(if cut { Next::Truncated } else { Next::Eof });
                }
                self.pos = 0;
                self.end = n;
            }
            let window = &self.chunk[self.pos..self.end];
            match window.iter().position(|b| *b == b'\n') {
                Some(i) => {
                    if !self.overflowed {
                        self.buf.extend_from_slice(&window[..i]);
                    }
                    self.pos += i + 1;
                    return Ok(self.finish());
                }
                None => {
                    if !self.overflowed {
                        self.buf.extend_from_slice(window);
                    }
                    self.pos = self.end;
                    if !self.overflowed && self.buf.len() > self.cap {
                        // Stop keeping bytes; keep reading until the newline so
                        // the stream is positioned on the next line.
                        self.overflowed = true;
                        self.buf.clear();
                    }
                }
            }
        }
    }

    fn finish(&mut self) -> Next {
        let overflowed = std::mem::take(&mut self.overflowed);
        let mut bytes = std::mem::take(&mut self.buf);
        if overflowed {
            return Next::TooLong;
        }
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
        if bytes.len() > self.cap {
            return Next::TooLong;
        }
        match String::from_utf8(bytes) {
            Ok(s) => Next::Line(s),
            Err(_) => Next::NotUtf8,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn all(input: &[u8], cap: usize) -> Vec<Next> {
        let mut r = LineReader::with_cap(Cursor::new(input.to_vec()), cap);
        let mut out = Vec::new();
        loop {
            let n = r.next_line().unwrap();
            let done = matches!(n, Next::Eof | Next::Truncated);
            out.push(n);
            if done {
                return out;
            }
        }
    }

    #[test]
    fn splits_on_newlines_and_ends_cleanly() {
        assert_eq!(
            all(b"{\"a\":1}\n{\"b\":2}\n", 64),
            vec![Next::Line("{\"a\":1}".into()), Next::Line("{\"b\":2}".into()), Next::Eof]
        );
    }

    #[test]
    fn a_carriage_return_before_the_newline_is_not_part_of_the_line() {
        assert_eq!(all(b"x\r\n", 8), vec![Next::Line("x".into()), Next::Eof]);
    }

    #[test]
    fn an_unterminated_tail_is_truncated_not_a_line() {
        assert_eq!(all(b"ok\npartial", 64), vec![Next::Line("ok".into()), Next::Truncated]);
    }

    #[test]
    fn an_overlong_line_is_discarded_to_its_newline_and_the_next_line_survives() {
        let mut input = vec![b'a'; 10_000];
        input.extend_from_slice(b"\nnext\n");
        assert_eq!(all(&input, 100), vec![Next::TooLong, Next::Line("next".into()), Next::Eof]);
    }

    #[test]
    fn exactly_the_cap_is_accepted_and_one_over_is_not() {
        let mut at = vec![b'a'; 100];
        at.push(b'\n');
        assert_eq!(all(&at, 100)[0], Next::Line("a".repeat(100)));
        let mut over = vec![b'a'; 101];
        over.push(b'\n');
        assert_eq!(all(&over, 100)[0], Next::TooLong);
    }

    #[test]
    fn an_overlong_line_that_never_ends_is_truncated_and_buffers_boundedly() {
        let input = vec![b'a'; 1_000_000];
        let mut r = LineReader::with_cap(Cursor::new(input), 100);
        assert_eq!(r.next_line().unwrap(), Next::Truncated);
        assert!(r.buf.capacity() < 20_000, "the buffer must not follow the input");
    }

    #[test]
    fn invalid_utf8_is_reported() {
        assert_eq!(all(b"\xff\xfe\n", 16), vec![Next::NotUtf8, Next::Eof]);
    }

    #[test]
    fn a_line_spanning_many_small_reads_is_assembled() {
        struct OneByte<'a>(&'a [u8]);
        impl Read for OneByte<'_> {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                match self.0.split_first() {
                    Some((b, rest)) => {
                        out[0] = *b;
                        self.0 = rest;
                        Ok(1)
                    }
                    None => Ok(0),
                }
            }
        }
        let mut r = LineReader::new(OneByte(b"hello\nworld\n"));
        assert_eq!(r.next_line().unwrap(), Next::Line("hello".into()));
        assert_eq!(r.next_line().unwrap(), Next::Line("world".into()));
        assert_eq!(r.next_line().unwrap(), Next::Eof);
    }

    #[test]
    fn a_timeout_in_the_middle_of_a_line_loses_nothing() {
        struct Script(Vec<io::Result<Vec<u8>>>);
        impl Read for Script {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                if self.0.is_empty() {
                    return Ok(0);
                }
                match self.0.remove(0) {
                    Ok(bytes) => {
                        out[..bytes.len()].copy_from_slice(&bytes);
                        Ok(bytes.len())
                    }
                    Err(e) => Err(e),
                }
            }
        }
        let mut r = LineReader::new(Script(vec![
            Ok(b"{\"a\"".to_vec()),
            Err(io::Error::new(io::ErrorKind::TimedOut, "slow peer")),
            Ok(b":1}\n".to_vec()),
        ]));
        assert_eq!(r.next_line().unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert_eq!(r.next_line().unwrap(), Next::Line("{\"a\":1}".into()));
    }
}
