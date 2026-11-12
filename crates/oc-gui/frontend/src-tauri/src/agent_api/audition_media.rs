use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::OnceLock;

use ring::{hmac, rand};

const PREFIX: &str = "/audition-render/";
static KEY: OnceLock<hmac::Key> = OnceLock::new();

pub(super) fn matches(path: &str) -> bool {
    path.starts_with(PREFIX)
}

pub(super) fn url(port: u16, path: &Path) -> Result<String, String> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("Invalid archive name")?;
    let key = match KEY.get() {
        Some(key) => key,
        None => {
            let key = hmac::Key::generate(hmac::HMAC_SHA256, &rand::SystemRandom::new())
                .map_err(|_| "Could not authorize archive playback")?;
            KEY.get_or_init(|| key)
        }
    };
    let tag = oc_market::verify::hex(hmac::sign(key, name.as_bytes()).as_ref());
    Ok(format!("http://127.0.0.1:{port}{PREFIX}{tag}/{name}"))
}

fn authorized_name(path: &str) -> Option<&str> {
    let (tag, name) = path.strip_prefix(PREFIX)?.split_once('/')?;
    let (stem, ext) = name.split_once('.')?;
    if tag.len() != 64
        || stem.len() != 64
        || !stem.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !matches!(ext, "wav" | "mp3")
    {
        return None;
    }
    let tag = oc_market::index::hex_decode(tag).ok()?;
    hmac::verify(KEY.get()?, name.as_bytes(), &tag).ok()?;
    Some(name)
}

pub(super) fn serve(
    out: &mut impl Write,
    root: &Path,
    method: &str,
    path: &str,
    range: Option<&str>,
) -> io::Result<()> {
    if !matches!(method, "GET" | "HEAD") {
        return out.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nAllow: GET, HEAD\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    }
    let file = authorized_name(path).and_then(|name| {
        let requested = root.join("audition-renders").join(name);
        let path = crate::commands::saved_audition_render_path(root, &requested).ok()?;
        if path.file_name() != Some(std::ffi::OsStr::new(name)) {
            return None;
        }
        let file = File::open(path).ok()?;
        let metadata = file.metadata().ok()?;
        let length = metadata.len();
        (metadata.is_file() && length > 0 && length <= crate::ai::sessions::AUDITION_ARCHIVE_BUDGET)
            .then_some((
                file,
                length,
                if name.ends_with(".wav") {
                    "audio/wav"
                } else {
                    "audio/mpeg"
                },
            ))
    });
    let Some((mut file, length, mime)) = file else {
        return out.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n");
    };
    // Ignore multipart requests and stream the representation. The media
    // players seek with a single byte interval.
    let range = range.filter(|range| method == "GET" && !range.contains(','));
    let (start, count, status) = match range {
        Some(range) => {
            let Some((start, end)) = byte_interval(range, length) else {
                return write!(
                    out,
                    "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{length}\r\nContent-Length: 0\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n"
                );
            };
            (start, end - start + 1, "206 Partial Content")
        }
        None => (0, length, "200 OK"),
    };
    write!(
        out,
        "HTTP/1.1 {status}\r\nContent-Type: {mime}\r\nContent-Length: {count}\r\nAccept-Ranges: bytes\r\nCache-Control: no-store\r\nConnection: close\r\n"
    )?;
    if range.is_some() {
        write!(
            out,
            "Content-Range: bytes {start}-{}/{length}\r\n",
            start + count - 1
        )?;
    }
    out.write_all(b"\r\n")?;
    if method == "GET" {
        file.seek(SeekFrom::Start(start))?;
        io::copy(&mut file.take(count), out)?;
    }
    Ok(())
}

fn byte_interval(range: &str, length: u64) -> Option<(u64, u64)> {
    let (start, end) = range.strip_prefix("bytes=")?.split_once('-')?;
    if start.is_empty() {
        let suffix: u64 = end.parse().ok()?;
        return (suffix != 0).then(|| (length.saturating_sub(suffix), length - 1));
    }
    let start: u64 = start.parse().ok()?;
    let end = if end.is_empty() {
        length - 1
    } else {
        end.parse::<u64>().ok()?.min(length - 1)
    };
    (start <= end).then_some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, std::path::PathBuf, String) {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("audition-renders");
        std::fs::create_dir(&folder).unwrap();
        let file = folder.join(format!("{}.wav", "a".repeat(64)));
        std::fs::write(&file, b"abcdef").unwrap();
        let url = url(38500, &file).unwrap();
        let path = url
            .strip_prefix("http://127.0.0.1:38500")
            .unwrap()
            .to_owned();
        (root, file, path)
    }

    #[test]
    fn audition_media_streams_full_and_ranged_responses() {
        let (root, _, path) = fixture();
        for (method, range, status, interval, body) in [
            ("GET", None, 200, None, "abcdef"),
            ("HEAD", Some("bytes=2-4"), 200, None, ""),
            ("GET", Some("bytes=2-4"), 206, Some("bytes 2-4/6"), "cde"),
            ("GET", Some("bytes=3-"), 206, Some("bytes 3-5/6"), "def"),
            ("GET", Some("bytes=-2"), 206, Some("bytes 4-5/6"), "ef"),
            (
                "GET",
                Some("bytes=0-99"),
                206,
                Some("bytes 0-5/6"),
                "abcdef",
            ),
            ("GET", Some("bytes=6-"), 416, Some("bytes */6"), ""),
            ("GET", Some("bytes=0-1,4-5"), 200, None, "abcdef"),
        ] {
            let mut result = Vec::new();
            serve(&mut result, root.path(), method, &path, range).unwrap();
            let result = String::from_utf8(result).unwrap();
            let (headers, returned) = result.split_once("\r\n\r\n").unwrap();
            assert!(
                headers.starts_with(&format!("HTTP/1.1 {status} ")),
                "{headers}"
            );
            assert_eq!(returned, body);
            assert_eq!(headers.contains("Content-Range:"), interval.is_some());
            if let Some(interval) = interval {
                assert!(headers.contains(&format!("Content-Range: {interval}")));
            }
            let length = if method == "HEAD" { 6 } else { body.len() };
            assert!(headers.contains(&format!("Content-Length: {length}")));
        }
    }

    #[test]
    fn audition_media_rejects_ungranted_paths_and_deleted_renders() {
        let (root, file, path) = fixture();
        let other = format!("{}.wav", "b".repeat(64));
        std::fs::write(file.with_file_name(&other), b"private").unwrap();
        let ungranted = format!("{}/{other}", path.rsplit_once('/').unwrap().0);
        for denied in [
            ungranted,
            format!(
                "{PREFIX}missing/{}",
                file.file_name().unwrap().to_string_lossy()
            ),
        ] {
            let mut out = Vec::new();
            serve(&mut out, root.path(), "GET", &denied, None).unwrap();
            assert!(out.starts_with(b"HTTP/1.1 404"));
        }
        std::fs::remove_file(file).unwrap();
        let mut out = Vec::new();
        serve(&mut out, root.path(), "GET", &path, None).unwrap();
        assert!(out.starts_with(b"HTTP/1.1 404"));
    }

    #[test]
    fn audition_media_full_get_uses_bounded_writes() {
        struct CountingSink(usize);
        impl Write for CountingSink {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                assert!(buf.len() <= 128 * 1024, "entire audio was buffered");
                self.0 += buf.len();
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let (root, file, path) = fixture();
        let bytes = 8 * 1024 * 1024;
        File::options()
            .write(true)
            .open(file)
            .unwrap()
            .set_len(bytes)
            .unwrap();
        let mut out = CountingSink(0);
        serve(&mut out, root.path(), "GET", &path, None).unwrap();
        assert!((bytes as usize..bytes as usize + 1024).contains(&out.0));
    }

    #[test]
    fn audition_media_socket_uses_the_grant_without_agent_credentials() {
        use std::net::{TcpListener, TcpStream};
        use std::time::Duration;

        let (root, _, path) = fixture();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let server = std::thread::spawn(move || {
            let _root = crate::ai::sessions::test_root(root.path().to_path_buf());
            let (mut socket, _) = listener.accept().unwrap();
            super::super::handle_connection_with(&mut socket, |_| panic!("unexpected agent route"))
                .unwrap();
        });
        write!(
            client,
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nrAnGe: bytes=1-3\r\n\r\n"
        )
        .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        server.join().unwrap();
        assert!(response.starts_with("HTTP/1.1 206 Partial Content\r\n"));
        assert!(response.ends_with("\r\n\r\nbcd"));
    }
}
