//! Minimal FastCGI client (responder role) for talking to php-fpm over
//! a Unix socket. One connection per request; php-fpm closes after the
//! response (we send FCGI_KEEP_CONN off).

use std::io;
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

const FCGI_BEGIN_REQUEST: u8 = 1;
const FCGI_END_REQUEST: u8 = 3;
const FCGI_PARAMS: u8 = 4;
const FCGI_STDIN: u8 = 5;
const FCGI_STDOUT: u8 = 6;
const FCGI_STDERR: u8 = 7;

const MAX_CONTENT: usize = 65535;

pub struct FcgiResponse {
    pub stdout: Vec<u8>,
    pub stderr: String,
}

fn record(record_type: u8, request_id: u16, content: &[u8]) -> Vec<u8> {
    let padding = (8 - (content.len() % 8)) % 8;
    let mut out = Vec::with_capacity(8 + content.len() + padding);
    out.push(1); // FCGI_VERSION_1
    out.push(record_type);
    out.extend_from_slice(&request_id.to_be_bytes());
    out.extend_from_slice(&(content.len() as u16).to_be_bytes());
    out.push(padding as u8);
    out.push(0); // reserved
    out.extend_from_slice(content);
    out.extend(std::iter::repeat_n(0, padding));
    out
}

fn encode_len(len: usize, out: &mut Vec<u8>) {
    if len < 128 {
        out.push(len as u8);
    } else {
        out.extend_from_slice(&((len as u32) | 0x8000_0000).to_be_bytes());
    }
}

fn params_records(request_id: u16, params: &[(String, String)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chunk: Vec<u8> = Vec::new();
    for (name, value) in params {
        let mut entry = Vec::with_capacity(name.len() + value.len() + 8);
        encode_len(name.len(), &mut entry);
        encode_len(value.len(), &mut entry);
        entry.extend_from_slice(name.as_bytes());
        entry.extend_from_slice(value.as_bytes());
        if chunk.len() + entry.len() > MAX_CONTENT {
            out.extend(record(FCGI_PARAMS, request_id, &chunk));
            chunk.clear();
        }
        chunk.extend(entry);
    }
    out.extend(record(FCGI_PARAMS, request_id, &chunk));
    out.extend(record(FCGI_PARAMS, request_id, &[])); // params terminator
    out
}

pub async fn request(
    socket: &Path,
    params: &[(String, String)],
    body: &[u8],
) -> io::Result<FcgiResponse> {
    let mut stream = UnixStream::connect(socket).await?;
    let mut outgoing = record(FCGI_BEGIN_REQUEST, 1, &[0, 1, 0, 0, 0, 0, 0, 0]);
    outgoing.extend(params_records(1, params));
    for chunk in body.chunks(MAX_CONTENT) {
        outgoing.extend(record(FCGI_STDIN, 1, chunk));
    }
    outgoing.extend(record(FCGI_STDIN, 1, &[])); // stdin terminator
    stream.write_all(&outgoing).await?;

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    loop {
        let mut header = [0u8; 8];
        stream.read_exact(&mut header).await?;
        let record_type = header[1];
        let content_len = u16::from_be_bytes([header[4], header[5]]) as usize;
        let padding_len = header[6] as usize;
        let mut content = vec![0u8; content_len];
        stream.read_exact(&mut content).await?;
        if padding_len > 0 {
            let mut padding = vec![0u8; padding_len];
            stream.read_exact(&mut padding).await?;
        }
        match record_type {
            FCGI_STDOUT => stdout.extend_from_slice(&content),
            FCGI_STDERR => stderr.extend_from_slice(&content),
            FCGI_END_REQUEST => break,
            _ => {}
        }
    }
    Ok(FcgiResponse {
        stdout,
        stderr: String::from_utf8_lossy(&stderr).to_string(),
    })
}

/// Split FastCGI stdout into (headers, body). php-fpm separates CGI
/// headers from the body with a blank line.
pub fn split_headers(stdout: &[u8]) -> (Vec<(String, String)>, &[u8]) {
    for (marker, skip) in [(b"\r\n\r\n".as_slice(), 4), (b"\n\n".as_slice(), 2)] {
        if let Some(pos) = find_subslice(stdout, marker) {
            let head = &stdout[..pos];
            let body = &stdout[pos + skip..];
            let headers = std::str::from_utf8(head)
                .unwrap_or("")
                .lines()
                .filter_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    Some((name.trim().to_string(), value.trim().to_string()))
                })
                .collect();
            return (headers, body);
        }
    }
    (Vec::new(), stdout)
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_layout_is_fcgi_compliant() {
        let rec = record(FCGI_STDIN, 1, b"hello");
        assert_eq!(rec[0], 1);
        assert_eq!(rec[1], FCGI_STDIN);
        assert_eq!(&rec[2..4], &1u16.to_be_bytes());
        assert_eq!(&rec[4..6], &5u16.to_be_bytes());
        assert_eq!(rec[6], 3); // padding to 8
        assert_eq!(rec.len(), 8 + 5 + 3);
    }

    #[test]
    fn params_encoding_short_and_long() {
        let records = params_records(1, &[("A".into(), "B".into())]);
        // 8-byte record header, then name-len, value-len, name, value.
        assert_eq!(records[8], 1);
        assert_eq!(records[9], 1);
        assert_eq!(records[10], b'A');
        assert_eq!(records[11], b'B');
        let long = params_records(1, &[("K".into(), "v".repeat(200))]);
        // value length >= 128 uses 4-byte form
        assert_eq!(long[9] & 0x80, 0x80);
    }

    #[test]
    fn splits_cgi_headers() {
        let out = b"Status: 404 Not Found\r\nContent-Type: text/html\r\n\r\n<body>";
        let (headers, body) = split_headers(out);
        assert_eq!(headers.len(), 2);
        assert_eq!(headers[0], ("Status".into(), "404 Not Found".into()));
        assert_eq!(body, b"<body>");
    }
}
