//! Local daemon API transport: TCP, one JSON object per line (NDJSON). Message content is defined by each tool.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Mutex;

/// Accept connections; for each received line call `f(line, stream)`. `f` writes its own reply
/// (or keeps a clone of the stream, e.g. for subscriptions). One thread per connection.
pub fn serve<F>(listener: TcpListener, f: F)
where
    F: Fn(&str, &mut TcpStream) -> std::io::Result<()> + Clone + Send + 'static,
{
    for conn in listener.incoming() {
        let Ok(stream) = conn else { continue };
        let f = f.clone();
        std::thread::spawn(move || -> std::io::Result<()> {
            let mut w = stream.try_clone()?;
            for line in BufReader::new(stream).lines() {
                f(&line?, &mut w)?;
            }
            Ok(())
        });
    }
}

/// Streams receiving broadcast lines; dropped on write failure.
#[derive(Default)]
pub struct Subscribers(Mutex<Vec<TcpStream>>);

impl Subscribers {
    pub fn add(&self, s: TcpStream) {
        self.0.lock().unwrap().push(s);
    }
    pub fn broadcast(&self, line: &str) {
        let line = format!("{line}\n");
        self.0.lock().unwrap().retain_mut(|s| s.write_all(line.as_bytes()).is_ok());
    }
}

/// Connect to a daemon.
pub fn connect(addr: &str) -> std::io::Result<TcpStream> {
    TcpStream::connect(addr)
}

/// One request line, one reply line.
pub fn call(addr: &str, line: &str) -> std::io::Result<String> {
    let mut s = connect(addr)?;
    writeln!(s, "{line}")?;
    let mut out = String::new();
    BufReader::new(s).read_line(&mut out)?;
    Ok(out)
}
