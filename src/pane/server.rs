//! A local server running in a pane: the address it printed when it
//! started (`http://localhost:5173/`), offered as a link in the pane's
//! header for as long as something answers on its port.

use super::*;

/// How often a running command's output is read again for an address.
const RESCAN: Duration = Duration::from_millis(800);
/// How often the port is knocked on to see the server's still there.
const RECHECK: Duration = Duration::from_secs(2);
/// Where servers announce themselves: the start of the output, and its end
/// (one that moved to another port, or restarted, says so last).
const HEAD_LINES: usize = 200;
const TAIL_LINES: usize = 80;
/// At most this many printed addresses are tried.
const CANDIDATES: usize = 4;

/// What's known about the server in a pane's running command.
#[derive(Default)]
pub(super) struct ServerScan {
    /// The command block read, and its output's generation then.
    block: u64,
    generation: u64,
    scanned_at: Option<Instant>,
    checked_at: Option<Instant>,
    /// Loopback addresses the output printed, in order.
    candidates: Vec<Local>,
    /// The first of them that answers.
    url: Option<String>,
}

/// A loopback address a command printed.
#[derive(Clone, Debug, PartialEq)]
pub struct Local {
    /// What to open: the printed URL, with an any-address host
    /// (`0.0.0.0`, `[::]`) made `localhost`.
    pub url: String,
    pub port: u16,
}

impl Pane {
    /// The address of the server the running command serves on this
    /// computer, once it printed one and something answers there.
    pub fn server_link(&mut self, now: Instant) -> Option<String> {
        let Some(block) = self.session.active().filter(|block| block.kind == BlockKind::Command) else {
            self.server = None;
            return None;
        };
        let scan = self.server.get_or_insert_with(ServerScan::default);
        if scan.block != block.id {
            *scan = ServerScan { block: block.id, ..ServerScan::default() };
        }
        let due = |at: Option<Instant>, every| at.is_none_or(|at| now.saturating_duration_since(at) >= every);
        if scan.generation != block.generation && due(scan.scanned_at, RESCAN) {
            scan.generation = block.generation;
            scan.scanned_at = Some(now);
            let found = printed_locals(block);
            if found != scan.candidates {
                scan.candidates = found;
                scan.checked_at = None;
            }
        }
        if !scan.candidates.is_empty() && due(scan.checked_at, RECHECK) {
            scan.checked_at = Some(now);
            scan.url = scan.candidates.iter().find(|local| answers(local.port)).map(|local| local.url.clone());
        }
        scan.url.clone()
    }
}

/// The loopback addresses in `block`'s output, near its start and its end.
fn printed_locals(block: &Block) -> Vec<Local> {
    let block_text = |from: usize, to: usize| block.text_between((from, 0), (to, usize::MAX));
    let lines = block.term.grid().history_size() + block.term.grid().screen_lines();
    let mut text = block_text(0, HEAD_LINES.min(lines).saturating_sub(1));
    if lines > HEAD_LINES {
        text.push('\n');
        text.push_str(&block_text(lines.saturating_sub(TAIL_LINES).max(HEAD_LINES), lines - 1));
    }
    let mut found: Vec<Local> = Vec::new();
    for local in locals(&text) {
        if !found.iter().any(|seen| seen.port == local.port) && found.len() < CANDIDATES {
            found.push(local);
        }
    }
    found
}

/// Every http(s) URL in `text` on this computer, with its port.
pub fn locals(text: &str) -> Vec<Local> {
    let chars: Vec<char> = text.chars().collect();
    crate::session::find_urls(&chars)
        .into_iter()
        .filter_map(|(start, end)| local(&chars[start..end].iter().collect::<String>()))
        .collect()
}

/// `url` as a `Local`, when it's http(s) to a loopback or any-address
/// host and names a port.
fn local(url: &str) -> Option<Local> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let split = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(split);
    let (host, port) = authority.rsplit_once(':')?;
    let port: u16 = port.parse().ok().filter(|&port| port > 0)?;
    let host = match host {
        "localhost" | "[::1]" => host,
        "0.0.0.0" | "[::]" => "localhost",
        _ if host.starts_with("127.") && host.parse::<std::net::Ipv4Addr>().is_ok() => host,
        _ => return None,
    };
    let path = if path.is_empty() { "/" } else { path };
    Some(Local { url: format!("{scheme}://{host}:{port}{path}"), port })
}

/// Whether something takes connections on `port` here, over IPv4 or IPv6
/// (Node's servers listen on `::1` alone).
fn answers(port: u16) -> bool {
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
    let wait = Duration::from_millis(50);
    [SocketAddr::from((Ipv4Addr::LOCALHOST, port)), SocketAddr::from((Ipv6Addr::LOCALHOST, port))]
        .iter()
        .any(|address| TcpStream::connect_timeout(address, wait).is_ok())
}

/// How a server's address reads in a header: its host and port.
pub fn label(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split(['/', '?', '#']).next().unwrap_or(rest).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(text: &str) -> Vec<String> {
        locals(text).into_iter().map(|local| local.url).collect()
    }

    #[test]
    fn finds_dev_server_addresses() {
        let vite = "  VITE v5.0.0  ready in 300 ms\n\n  ➜  Local:   http://localhost:5173/\n  ➜  Network: http://192.168.1.5:5173/";
        assert_eq!(urls(vite), ["http://localhost:5173/"]);
        assert_eq!(urls("Serving HTTP on :: port 8000 (http://[::]:8000/) ..."), ["http://localhost:8000/"]);
        assert_eq!(urls(" * Running on http://127.0.0.1:5000"), ["http://127.0.0.1:5000/"]);
        assert_eq!(urls("Listening on http://0.0.0.0:3000."), ["http://localhost:3000/"]);
        assert_eq!(urls("    http://localhost:8888/tree?token=abc123"), ["http://localhost:8888/tree?token=abc123"]);
        assert_eq!(urls("https://[::1]:8443/app"), ["https://[::1]:8443/app"]);
    }

    #[test]
    fn skips_other_addresses() {
        assert!(urls("see https://example.com:8080/ and http://localhost/").is_empty());
        assert!(urls("http://localhost:0/ http://localhost:99999/ ftp://localhost:21/").is_empty());
        assert!(urls("http://127.0.0.1.evil.com:80/").is_empty());
    }

    #[test]
    fn labels_host_and_port() {
        assert_eq!(label("http://localhost:5173/"), "localhost:5173");
        assert_eq!(label("http://127.0.0.1:8888/tree?token=x"), "127.0.0.1:8888");
    }

    #[test]
    fn knocks_on_a_listening_port() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(answers(port));
        drop(listener);
        assert!(!answers(port));
    }
}
