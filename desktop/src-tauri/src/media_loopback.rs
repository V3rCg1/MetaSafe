//! WebKitGTK's media backend accepts HTTP but cannot read custom URI schemes.
use std::{
    io::{self, Read, Seek, SeekFrom, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use super::{Playback, active_playback, byte_range};

const TIMEOUT: Duration = Duration::from_secs(3);
const MAX_REQUEST: usize = 8 * 1024;
const STREAM_CHUNK: usize = 64 * 1024;

const MAX_PENDING: usize = 64;
const WORKERS: usize = 4;
const ADMISSION_INTERVAL: Duration = Duration::from_millis(4);

pub fn start(playback: Arc<Mutex<Option<Playback>>>) -> io::Result<u16> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    let (accepted, jobs) = mpsc::sync_channel::<AuthorizedRequest>(WORKERS);
    let jobs = Arc::new(Mutex::new(jobs));
    for _ in 0..WORKERS {
        let jobs = jobs.clone();
        let playback = playback.clone();
        thread::spawn(move || {
            loop {
                let job = {
                    let Ok(queue) = jobs.lock() else { return };
                    queue.recv()
                };
                let Ok(job) = job else { return };
                let _ = stream_media(job, &playback);
            }
        });
    }
    thread::spawn(move || admit(listener, &format!("127.0.0.1:{port}"), &playback, &accepted));
    Ok(port)
}

struct Pending {
    connection: TcpStream,
    buffer: Box<[u8; MAX_REQUEST]>,
    used: usize,
    deadline: Instant,
}

impl Pending {
    fn read_header(&mut self) -> io::Result<bool> {
        if self.deadline <= Instant::now() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let count = match self.connection.read(&mut self.buffer[self.used..]) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) => return Err(error),
        };
        let previous = self.used;
        self.used += count;
        if self.buffer[previous.saturating_sub(3)..self.used]
            .windows(4)
            .any(|bytes| bytes == b"\r\n\r\n")
        {
            Ok(true)
        } else if self.used == MAX_REQUEST {
            Err(io::ErrorKind::InvalidData.into())
        } else {
            Ok(false)
        }
    }
}

fn admit(
    listener: TcpListener,
    host: &str,
    playback: &Mutex<Option<Playback>>,
    accepted: &mpsc::SyncSender<AuthorizedRequest>,
) {
    let mut pending: Vec<Pending> = Vec::with_capacity(MAX_PENDING);
    loop {
        let mut index = 0;
        while index < pending.len() {
            match pending[index].read_header() {
                Ok(false) => index += 1,
                Ok(true) => {
                    let entry = pending.swap_remove(index);
                    if let Ok(Some(job)) = authorize(entry, host, playback) {
                        let _ = accepted.try_send(job);
                    }
                }
                Err(_) => {
                    pending.swap_remove(index);
                }
            }
        }
        // Bound each admission pass so already accepted headers are always polled.
        for _ in 0..WORKERS {
            let (connection, _) = match listener.accept() {
                Ok(client) => client,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return,
            };
            if connection.set_nonblocking(true).is_err() {
                continue;
            }
            if pending.len() == MAX_PENDING {
                // Idle headers cannot reserve every pending slot until their deadline.
                let oldest = pending
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, entry)| entry.deadline)
                    .map(|(index, _)| index)
                    .expect("pending slots are full");
                pending.swap_remove(oldest);
            }
            pending.push(Pending {
                connection,
                buffer: Box::new([0; MAX_REQUEST]),
                used: 0,
                deadline: Instant::now() + TIMEOUT,
            });
        }
        thread::sleep(if pending.is_empty() {
            Duration::from_millis(20)
        } else {
            ADMISSION_INTERVAL
        });
    }
}

struct AuthorizedRequest {
    connection: TcpStream,
    path: String,
    head: bool,
    range: Option<(u64, u64)>,
}

fn authorize(
    Pending {
        mut connection,
        buffer,
        used,
        ..
    }: Pending,
    host: &str,
    playback: &Mutex<Option<Playback>>,
) -> io::Result<Option<AuthorizedRequest>> {
    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut request = httparse::Request::new(&mut headers);
    if !matches!(
        request.parse(&buffer[..used]),
        Ok(httparse::Status::Complete(_))
    ) {
        return denied(&mut connection, "400 Bad Request");
    }
    let head = match request.method {
        Some("GET") => false,
        Some("HEAD") => true,
        _ => return denied(&mut connection, "405 Method Not Allowed"),
    };
    let mut hosts = request
        .headers
        .iter()
        .filter(|header| header.name.eq_ignore_ascii_case("host"));
    if !hosts
        .next()
        .is_some_and(|header| header.value == host.as_bytes())
        || hosts.next().is_some()
    {
        return denied(&mut connection, "400 Bad Request");
    }
    // Media requests may omit Origin or send the opaque origin "null".
    // A web origin, when supplied, must be the local development UI.
    for origin in request
        .headers
        .iter()
        .filter(|header| header.name.eq_ignore_ascii_case("origin"))
    {
        if origin.value != b"null"
            && origin.value != b"tauri://localhost"
            && !(cfg!(debug_assertions) && origin.value == b"http://localhost:1422")
        {
            return denied(&mut connection, "403 Forbidden");
        }
    }
    let Some(path) = request.path else {
        return denied(&mut connection, "400 Bad Request");
    };
    let media_info = {
        let mut current = playback
            .lock()
            .map_err(|_| io::Error::other("Playback unavailable"))?;
        active_playback(&mut current, path).map(|media| (media.length, media.mime))
    };
    let Some((length, _)) = media_info.filter(|(length, _)| *length != 0) else {
        return denied(&mut connection, "404 Not Found");
    };

    let mut ranges = request
        .headers
        .iter()
        .filter(|header| header.name.eq_ignore_ascii_case("range"));
    let range = if let Some(range) = ranges.next() {
        let parsed = std::str::from_utf8(range.value)
            .ok()
            .and_then(|value| byte_range(value, length));
        let Some(range) = parsed.filter(|_| ranges.next().is_none()) else {
            write!(
                connection,
                "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{length}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )?;
            return Ok(None);
        };
        Some(range)
    } else {
        None
    };
    Ok(Some(AuthorizedRequest {
        connection,
        path: path.to_owned(),
        head,
        range,
    }))
}

fn stream_media(
    AuthorizedRequest {
        mut connection,
        path,
        head,
        range,
    }: AuthorizedRequest,
    playback: &Mutex<Option<Playback>>,
) -> io::Result<()> {
    connection.set_nonblocking(false)?;
    connection.set_write_timeout(Some(TIMEOUT))?;
    let media_info = {
        let mut current = playback
            .lock()
            .map_err(|_| io::Error::other("Playback unavailable"))?;
        active_playback(&mut current, &path).map(|media| (media.length, media.mime))
    };
    let Some((length, mime)) = media_info else {
        return Ok(());
    };
    let (start, end) = range.unwrap_or((0, length - 1));
    let status = if range.is_some() {
        "206 Partial Content"
    } else {
        "200 OK"
    };
    write!(
        connection,
        "HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n",
        end - start + 1,
    )?;
    if status == "206 Partial Content" {
        write!(
            connection,
            "Content-Range: bytes {start}-{end}/{length}\r\n"
        )?;
    }
    connection.write_all(b"\r\n")?;
    if head {
        return Ok(());
    }

    let mut chunk = [0_u8; STREAM_CHUNK];
    let mut offset = start;
    while offset <= end {
        let bytes = (end - offset + 1).min(chunk.len() as u64) as usize;
        {
            let mut current = playback
                .lock()
                .map_err(|_| io::Error::other("Playback unavailable"))?;
            let Some(media) = active_playback(&mut current, &path) else {
                // Revocation stops an in-progress stream, too.
                return Ok(());
            };
            media.file.seek(SeekFrom::Start(offset))?;
            media.file.read_exact(&mut chunk[..bytes])?;
        }
        connection.write_all(&chunk[..bytes])?;
        offset += bytes as u64;
    }
    Ok(())
}

fn denied(connection: &mut TcpStream, status: &str) -> io::Result<Option<AuthorizedRequest>> {
    write!(
        connection,
        "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )?;
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    #[test]
    fn loopback_validates_host_and_current_token_before_serving_bytes() {
        const PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
        let file = File::open(PATH).unwrap();
        let length = file.metadata().unwrap().len();
        let playback = Arc::new(Mutex::new(Some(Playback {
            token: "active".into(),
            file,
            length,
            mime: "video/webm",
        })));
        let port = start(playback.clone()).unwrap();
        let host = format!("127.0.0.1:{port}");
        let request = |path: &str, host_header: &str| {
            let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
            stream.set_read_timeout(Some(TIMEOUT)).unwrap();
            write!(
                stream,
                "GET {path} HTTP/1.1\r\n{host_header}Range: bytes=0-2\r\n\r\n"
            )
            .unwrap();
            let mut response = Vec::new();
            stream.read_to_end(&mut response).unwrap();
            response
        };
        let valid_host = format!("Host: {host}\r\n");
        // A normal client may split its headers across writes. Keep its request
        // pending while a separate complete request is processed.
        let mut pending = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        pending.set_read_timeout(Some(TIMEOUT)).unwrap();
        write!(pending, "HEAD /active HTTP/1.1\r\n{valid_host}").unwrap();
        let response = request("/active", &valid_host);
        assert!(response.starts_with(b"HTTP/1.1 206 Partial Content\r\n"));
        assert!(response.ends_with(&std::fs::read(PATH).unwrap()[..3]));
        pending.write_all(b"\r\n").unwrap();
        let mut head = Vec::new();
        pending.read_to_end(&mut head).unwrap();
        assert!(head.starts_with(b"HTTP/1.1 200 OK\r\n"));
        assert!(head.ends_with(b"\r\n\r\n"));
        assert!(request("/wrong", &valid_host).starts_with(b"HTTP/1.1 404 "));
        assert!(request("/", &valid_host).starts_with(b"HTTP/1.1 404 "));
        assert!(request("/active", "Host: localhost\r\n").starts_with(b"HTTP/1.1 400 "));
        assert!(request("/active", "").starts_with(b"HTTP/1.1 400 "));
        assert!(
            request(
                "/active",
                &format!("{valid_host}Origin: https://example.invalid\r\n")
            )
            .starts_with(b"HTTP/1.1 403 ")
        );
        *playback.lock().unwrap() = None;
        assert!(request("/active", &valid_host).starts_with(b"HTTP/1.1 404 "));
    }
}
