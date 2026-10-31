//! Minimal HTTP/1.1 client (std only) for a *local* model server. Plain http on localhost or the
//! LAN only — the assistant never sends anything to the internet.
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

pub struct Url {
    pub host: String,
    pub port: u16,
    pub path: String,
}

pub fn parse_url(u: &str) -> Result<Url, String> {
    let rest = u.strip_prefix("http://").ok_or("only http:// endpoints are supported (a local model server)")?;
    let (hp, path) = rest.split_once('/').map(|(a, b)| (a, format!("/{b}"))).unwrap_or((rest, "/".into()));
    let (host, port) = match hp.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().map_err(|_| format!("bad port in {u}"))?),
        None => (hp.to_string(), 80),
    };
    Ok(Url { host, port, path })
}

pub fn is_local(host: &str) -> bool {
    host == "localhost" || host.starts_with("127.") || host == "::1" || host.starts_with("192.168.") || host.starts_with("10.")
        || host.ends_with(".local") || (host.starts_with("172.") && host.split('.').nth(1).and_then(|o| o.parse::<u8>().ok()).map_or(false, |o| (16..32).contains(&o)))
}

fn dechunk(body: &[u8]) -> Vec<u8> {
    let mut out = vec![];
    let mut i = 0;
    while i < body.len() {
        let Some(nl) = body[i..].windows(2).position(|w| w == b"\r\n") else { break };
        let size = usize::from_str_radix(std::str::from_utf8(&body[i..i + nl]).unwrap_or("0").split(';').next().unwrap_or("0").trim(), 16).unwrap_or(0);
        i += nl + 2;
        if size == 0 || i + size > body.len() {
            break;
        }
        out.extend_from_slice(&body[i..i + size]);
        i += size + 2;
    }
    out
}

pub fn request(method: &str, url: &str, body: Option<&str>, timeout_s: u64) -> Result<(u16, String), String> {
    let u = parse_url(url)?;
    if !is_local(&u.host) {
        return Err(format!("{} is not a local address — the assistant only talks to a model server on this machine or your LAN", u.host));
    }
    let mut s = TcpStream::connect((u.host.as_str(), u.port)).map_err(|e| format!("cannot reach {}:{} ({e}) — is the model server running?", u.host, u.port))?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(timeout_s)));
    let _ = s.set_write_timeout(Some(Duration::from_secs(30)));
    let b = body.unwrap_or("");
    let req = format!(
        "{method} {} HTTP/1.1\r\nHost: {}:{}\r\nUser-Agent: shadow/{}\r\nAccept: application/json\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}",
        u.path, u.host, u.port, env!("CARGO_PKG_VERSION"), b.len()
    );
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut raw = vec![];
    s.read_to_end(&mut raw).map_err(|e| format!("reading response: {e}"))?;
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or("malformed HTTP response")?;
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let code: u16 = head.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    let mut body = raw[split + 4..].to_vec();
    if head.to_lowercase().contains("transfer-encoding: chunked") {
        body = dechunk(&body);
    }
    Ok((code, String::from_utf8_lossy(&body).to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn urls_and_locality() {
        let u = parse_url("http://127.0.0.1:8000/v3").unwrap();
        assert_eq!((u.host.as_str(), u.port, u.path.as_str()), ("127.0.0.1", 8000, "/v3"));
        assert!(parse_url("https://api.example.com/v1").is_err());
        assert!(is_local("localhost") && is_local("192.168.1.20") && is_local("172.20.0.2"));
        assert!(!is_local("8.8.8.8") && !is_local("api.openai.com") && !is_local("172.40.0.1"));
    }
    #[test]
    fn chunked() {
        assert_eq!(dechunk(b"4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n"), b"Wikipedia");
    }
}
