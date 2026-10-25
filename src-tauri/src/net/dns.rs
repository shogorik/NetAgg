//! The resolver behind the adapter's DNS address.
//!
//! Auto asks each network's own DNS servers (what its router handed out, or
//! what the adapter was set to), through that network. Manual asks the
//! servers the user chose, plain, DNS-over-TLS or DNS-over-HTTPS, through
//! whichever network the balancer picks. An encrypted server that fails is
//! never retried in the clear.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV6};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Bytes;
use hyper::client::conn::http1::SendRequest;
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use rustls::ClientConfig;
use rustls_platform_verifier::BuilderVerifierExt;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;

use super::dial;
use super::links::Link;
use super::router::Router;
use crate::sys;

/// The adapter's DNS server: inside its own /24, so lookups always reach the
/// adapter, and never a real address someone else could answer for.
pub const RESOLVER_V4: Ipv4Addr = Ipv4Addr::new(10, 77, 0, 53);
pub const DNS_PORT: u16 = 53;

/// Per server and network; the OS asks again after about a second anyway.
const TRY_TIMEOUT: Duration = Duration::from_secs(2);
/// The first network plus one failover, as for connections.
const LINKS_TRIED: usize = 2;
/// Encrypted connections are kept for reuse this long; servers close idle
/// ones themselves after 10-30 s.
const IDLE: Duration = Duration::from_secs(20);
const POOL_MAX: usize = 4;
/// How long a network's DNS servers are trusted before asking the OS again.
const LINK_DNS_TTL: Duration = Duration::from_secs(10);
const MAX_MESSAGE: usize = 65535;
/// What a UDP answer may hold: the classic 512 bytes, or with EDNS the size
/// recommended since DNS Flag Day 2020, which also fits in one packet.
const UDP_PLAIN: usize = 512;
const UDP_EDNS: usize = 1232;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DnsMode {
    #[default]
    Auto,
    Manual,
}

/// One server the user chose.
#[derive(Clone, Debug, PartialEq)]
pub enum Upstream {
    Plain(SocketAddr),
    Tls {
        addr: SocketAddr,
        name: ServerName<'static>,
    },
    Https {
        addr: SocketAddr,
        name: ServerName<'static>,
        authority: String,
        path: String,
    },
}

impl Upstream {
    /// Accepts `9.9.9.9`, `[2620:fe::fe]:53`, `tls://1.1.1.1`,
    /// `tls://dns.quad9.net#9.9.9.9` and `https://dns.google/dns-query#8.8.8.8`.
    /// A server known by name needs its address after `#`: looking the name
    /// up would itself be a lookup in the clear, to some other server.
    pub fn parse(spec: &str) -> Result<Upstream, String> {
        let spec = spec.trim();
        if let Ok(ip) = spec.parse::<IpAddr>() {
            return Ok(Upstream::Plain(SocketAddr::new(ip, DNS_PORT)));
        }
        if let Ok(addr) = spec.parse::<SocketAddr>() {
            return Ok(Upstream::Plain(addr));
        }
        let bad = || format!("{spec} is not an IP address, tls:// or https:// server");
        let url = url::Url::parse(spec).map_err(|_| bad())?;
        let default_port = match url.scheme() {
            "tls" => 853,
            "https" => 443,
            _ => return Err(bad()),
        };
        if !url.username().is_empty() || url.password().is_some() {
            return Err(format!("{spec}: no user name or password"));
        }
        // tls:// isn't a special scheme, so its IPv4 host comes as a name.
        let (host, ip) = match url.host().ok_or_else(bad)? {
            url::Host::Ipv6(ip) => (ip.to_string(), Some(IpAddr::V6(ip))),
            url::Host::Ipv4(ip) => (ip.to_string(), Some(IpAddr::V4(ip))),
            url::Host::Domain(name) => (name.to_owned(), name.parse().ok()),
        };
        let (name, ip) = match ip {
            Some(ip) => (ServerName::IpAddress(ip.into()), ip),
            None => {
                let ip = url.fragment().and_then(|f| f.parse().ok()).ok_or_else(|| {
                    format!("{spec}: add the server's IP address after #, as in tls://dns.quad9.net#9.9.9.9")
                })?;
                let name = ServerName::try_from(host.clone())
                    .map_err(|_| format!("{spec}: {host} is not a valid server name"))?;
                (name, ip)
            }
        };
        let port = url.port().unwrap_or(default_port);
        let addr = SocketAddr::new(ip, port);
        if url.scheme() == "tls" {
            if !matches!(url.path(), "" | "/") || url.query().is_some() {
                return Err(format!("{spec}: tls:// takes no path"));
            }
            return Ok(Upstream::Tls { addr, name });
        }
        let authority = match (url.host_str(), url.port()) {
            (Some(h), Some(p)) => format!("{h}:{p}"),
            (Some(h), None) => h.to_owned(),
            (None, _) => return Err(bad()),
        };
        let mut path = match url.path() {
            "" | "/" => "/dns-query".to_owned(),
            p => p.to_owned(),
        };
        if let Some(query) = url.query() {
            path = format!("{path}?{query}");
        }
        Ok(Upstream::Https {
            addr,
            name,
            authority,
            path,
        })
    }

    fn addr(&self) -> SocketAddr {
        match self {
            Upstream::Plain(addr) | Upstream::Tls { addr, .. } | Upstream::Https { addr, .. } => {
                *addr
            }
        }
    }

    fn encrypted(&self) -> bool {
        !matches!(self, Upstream::Plain(_))
    }
}

/// Idle encrypted connections, by network and server index.
type Pool = HashMap<(String, usize), Vec<(Instant, Conn)>>;

enum Conn {
    Tls(Box<TlsStream<TcpStream>>),
    Https(SendRequest<Full<Bytes>>),
}

pub struct Resolver {
    router: Arc<Router>,
    mode: DnsMode,
    upstreams: Vec<Upstream>,
    dot: Option<TlsConnector>,
    doh: Option<TlsConnector>,
    /// Encrypted connections to reuse, by network and server.
    idle: Mutex<Pool>,
    link_dns: Mutex<HashMap<u32, (Instant, Vec<SocketAddr>)>>,
}

impl Resolver {
    pub fn new(
        router: Arc<Router>,
        mode: DnsMode,
        upstreams: Vec<Upstream>,
    ) -> io::Result<Resolver> {
        if mode == DnsMode::Manual && upstreams.is_empty() {
            return Err(io::Error::other(
                "add at least one DNS server for manual DNS",
            ));
        }
        let encrypted = mode == DnsMode::Manual && upstreams.iter().any(Upstream::encrypted);
        let (dot, doh) = if encrypted {
            (Some(connector(&[])?), Some(connector(&[b"http/1.1"])?))
        } else {
            (None, None)
        };
        Ok(Resolver {
            router,
            mode,
            upstreams,
            dot,
            doh,
            idle: Mutex::default(),
            link_dns: Mutex::default(),
        })
    }

    /// The answer to one DNS message from an app. `udp` says it came over
    /// UDP, where a long answer is cut short so the app asks again over TCP.
    /// `None` for something that isn't a query.
    pub async fn answer(&self, query: &[u8], udp: bool) -> Option<Vec<u8>> {
        if !is_query(query) {
            return None;
        }
        let reply = match self.mode {
            DnsMode::Auto => self.ask_networks(query).await,
            DnsMode::Manual => self.ask_chosen(query).await,
        };
        let reply = reply.unwrap_or_else(|| {
            log::debug!("dns: no server answered");
            reply_header(query, RCODE_SERVFAIL, false)
        });
        Some(if udp && reply.len() > udp_limit(query) {
            reply_header(query, 0, true)
        } else {
            reply
        })
    }

    /// Auto: each network's own servers, through that network.
    async fn ask_networks(&self, query: &[u8]) -> Option<Vec<u8>> {
        let mut tried: Vec<String> = Vec::new();
        for _ in 0..LINKS_TRIED {
            let exclude: Vec<&str> = tried.iter().map(String::as_str).collect();
            let link = self.router.choose(false, None, &exclude)?;
            tried.push(link.id.clone());
            for server in self.link_servers(&link) {
                match tokio::time::timeout(TRY_TIMEOUT, plain(&link, server, query)).await {
                    Ok(Ok(reply)) => return Some(reply),
                    Ok(Err(err)) => log::debug!("dns {server} via {}: {err}", link.id),
                    Err(_) => log::debug!("dns {server} via {}: timed out", link.id),
                }
            }
        }
        None
    }

    /// Manual: the user's servers in order, each through up to two networks.
    async fn ask_chosen(&self, query: &[u8]) -> Option<Vec<u8>> {
        for (i, upstream) in self.upstreams.iter().enumerate() {
            let addr = upstream.addr();
            let key = addr.ip().to_string();
            let mut tried: Vec<String> = Vec::new();
            for _ in 0..LINKS_TRIED {
                let exclude: Vec<&str> = tried.iter().map(String::as_str).collect();
                let Some(link) = self.router.choose(addr.is_ipv6(), Some(&key), &exclude) else {
                    break;
                };
                tried.push(link.id.clone());
                let asked = self.ask(&link, i, upstream, query);
                match tokio::time::timeout(TRY_TIMEOUT, asked).await {
                    Ok(Ok(reply)) => return Some(reply),
                    Ok(Err(err)) => log::debug!("dns {addr} via {}: {err}", link.id),
                    Err(_) => log::debug!("dns {addr} via {}: timed out", link.id),
                }
            }
        }
        None
    }

    async fn ask(
        &self,
        link: &Link,
        i: usize,
        upstream: &Upstream,
        query: &[u8],
    ) -> io::Result<Vec<u8>> {
        let key = (link.id.clone(), i);
        match upstream {
            Upstream::Plain(addr) => plain(link, *addr, query).await,
            Upstream::Tls { addr, name } => {
                if let Some(Conn::Tls(mut stream)) = self.take(&key) {
                    if let Ok(reply) = exchange(&mut stream, query).await {
                        self.put(key, Conn::Tls(stream));
                        return Ok(reply);
                    }
                }
                let dot = self
                    .dot
                    .as_ref()
                    .expect("a connector for encrypted servers");
                let mut stream = tls(dot, link, *addr, name).await?;
                let reply = exchange(&mut stream, query).await?;
                self.put(key, Conn::Tls(Box::new(stream)));
                Ok(reply)
            }
            Upstream::Https {
                addr,
                name,
                authority,
                path,
            } => {
                if let Some(Conn::Https(mut sender)) = self.take(&key) {
                    if let Ok(reply) = post(&mut sender, authority, path, query).await {
                        self.put(key, Conn::Https(sender));
                        return Ok(reply);
                    }
                }
                let doh = self
                    .doh
                    .as_ref()
                    .expect("a connector for encrypted servers");
                let stream = tls(doh, link, *addr, name).await?;
                let (mut sender, conn) =
                    hyper::client::conn::http1::handshake(TokioIo::new(stream))
                        .await
                        .map_err(io::Error::other)?;
                tokio::spawn(async move {
                    let _ = conn.await;
                });
                let reply = post(&mut sender, authority, path, query).await?;
                self.put(key, Conn::Https(sender));
                Ok(reply)
            }
        }
    }

    fn take(&self, key: &(String, usize)) -> Option<Conn> {
        let mut idle = self.idle.lock().unwrap();
        let conns = idle.get_mut(key)?;
        conns.retain(|(at, _)| at.elapsed() < IDLE);
        conns.pop().map(|(_, conn)| conn)
    }

    fn put(&self, key: (String, usize), conn: Conn) {
        let mut idle = self.idle.lock().unwrap();
        let conns = idle.entry(key).or_default();
        if conns.len() < POOL_MAX {
            conns.push((Instant::now(), conn));
        }
    }

    /// The network's DNS servers, as the OS knows them.
    fn link_servers(&self, link: &Link) -> Vec<SocketAddr> {
        let if_index = link.info().if_index;
        let mut cache = self.link_dns.lock().unwrap();
        if let Some((at, servers)) = cache.get(&if_index) {
            if at.elapsed() < LINK_DNS_TTL {
                return servers.clone();
            }
        }
        // shortcut: asks the OS with the lock held (a few ms, at most every
        // 10 s per network); move it off the lock if lookups ever queue here.
        let servers: Vec<SocketAddr> = sys::link_dns_servers(if_index)
            .into_iter()
            .filter(is_network_dns)
            .map(|ip| match ip {
                // A link-local server is only reachable with its interface.
                IpAddr::V6(v6) if v6.segments()[0] & 0xffc0 == 0xfe80 => {
                    SocketAddr::V6(SocketAddrV6::new(v6, DNS_PORT, 0, if_index))
                }
                ip => SocketAddr::new(ip, DNS_PORT),
            })
            .collect();
        cache.insert(if_index, (Instant::now(), servers.clone()));
        servers
    }
}

/// Leaves out what can't be a network's own server: loopback stubs
/// (systemd-resolved's 127.0.0.53), MultiPath's own address, and the
/// site-local placeholders Windows lists when IPv6 has no DNS.
fn is_network_dns(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !v4.is_loopback()
                && !v4.is_unspecified()
                && v4.octets()[..3] != RESOLVER_V4.octets()[..3]
        }
        IpAddr::V6(v6) => {
            !v6.is_loopback() && !v6.is_unspecified() && v6.segments()[0] & 0xffc0 != 0xfec0
        }
    }
}

fn connector(alpn: &[&[u8]]) -> io::Result<TlsConnector> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .and_then(|b| b.with_platform_verifier())
        .map_err(io::Error::other)?
        .with_no_client_auth();
    config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    Ok(TlsConnector::from(Arc::new(config)))
}

async fn tls(
    connector: &TlsConnector,
    link: &Link,
    addr: SocketAddr,
    name: &ServerName<'static>,
) -> io::Result<TlsStream<TcpStream>> {
    let tcp = dial::tcp(&link.info(), addr, TRY_TIMEOUT).await?;
    connector.connect(name.clone(), tcp).await
}

/// Plain DNS over UDP, through `link`, and over TCP if the answer was too
/// long for UDP.
async fn plain(link: &Link, server: SocketAddr, query: &[u8]) -> io::Result<Vec<u8>> {
    let info = link.info();
    let socket = dial::udp(&info, server.is_ipv6())?;
    socket.connect(server).await?;
    socket.send(query).await?;
    let mut buf = vec![0u8; MAX_MESSAGE];
    let n = loop {
        let n = socket.recv(&mut buf).await?;
        // The socket is connected, so only the server can answer; a
        // mismatched ID is a stale or forged answer and is ignored.
        if is_reply_to(query, &buf[..n]) {
            break n;
        }
    };
    if buf[2] & FLAG_TC == 0 {
        return Ok(buf[..n].to_vec());
    }
    let mut tcp = dial::tcp(&info, server, TRY_TIMEOUT).await?;
    exchange(&mut tcp, query).await
}

/// One query over a stream: TCP or DNS-over-TLS (RFC 7766, RFC 7858).
async fn exchange<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    query: &[u8],
) -> io::Result<Vec<u8>> {
    let len = u16::try_from(query.len()).map_err(|_| io::Error::other("query too long"))?;
    let mut framed = Vec::with_capacity(query.len() + 2);
    framed.extend_from_slice(&len.to_be_bytes());
    framed.extend_from_slice(query);
    stream.write_all(&framed).await?;
    stream.flush().await?;
    loop {
        let len = stream.read_u16().await? as usize;
        let mut reply = vec![0u8; len];
        stream.read_exact(&mut reply).await?;
        if is_reply_to(query, &reply) {
            return Ok(reply);
        }
    }
}

/// One query as a DNS-over-HTTPS POST (RFC 8484).
async fn post(
    sender: &mut SendRequest<Full<Bytes>>,
    authority: &str,
    path: &str,
    query: &[u8],
) -> io::Result<Vec<u8>> {
    sender.ready().await.map_err(io::Error::other)?;
    let request = hyper::Request::post(path)
        .header(hyper::header::HOST, authority)
        .header(hyper::header::CONTENT_TYPE, "application/dns-message")
        .header(hyper::header::ACCEPT, "application/dns-message")
        .body(Full::new(Bytes::copy_from_slice(query)))
        .map_err(io::Error::other)?;
    let response = sender
        .send_request(request)
        .await
        .map_err(io::Error::other)?;
    if response.status() != hyper::StatusCode::OK {
        return Err(io::Error::other(format!("HTTP {}", response.status())));
    }
    let body = Limited::new(response.into_body(), MAX_MESSAGE)
        .collect()
        .await
        .map_err(io::Error::other)?
        .to_bytes();
    if !is_reply_to(query, &body) {
        return Err(io::Error::other("not an answer to this query"));
    }
    Ok(body.to_vec())
}

const FLAG_QR: u8 = 0x80;
const FLAG_TC: u8 = 0x02;
const RCODE_SERVFAIL: u8 = 2;

fn is_query(msg: &[u8]) -> bool {
    msg.len() >= 12 && msg[2] & FLAG_QR == 0
}

fn is_reply_to(query: &[u8], reply: &[u8]) -> bool {
    reply.len() >= 12 && reply[..2] == query[..2] && reply[2] & FLAG_QR != 0
}

/// shortcut: any additional record is taken to be EDNS's OPT, without
/// reading the size it offers; read the OPT record if answers get cut early.
fn udp_limit(query: &[u8]) -> usize {
    if u16::from_be_bytes([query[10], query[11]]) > 0 {
        UDP_EDNS
    } else {
        UDP_PLAIN
    }
}

/// Where the first question ends, if the query has exactly one.
fn question_end(query: &[u8]) -> Option<usize> {
    if u16::from_be_bytes([query[4], query[5]]) != 1 {
        return None;
    }
    let mut i = 12;
    loop {
        let len = *query.get(i)? as usize;
        if len == 0 {
            break;
        }
        // Queries carry no compression pointers.
        if len & 0xc0 != 0 {
            return None;
        }
        i += 1 + len;
    }
    let end = i + 1 + 4;
    (end <= query.len()).then_some(end)
}

/// An answer with no records: SERVFAIL, or truncated (TC) to send the app to TCP.
fn reply_header(query: &[u8], rcode: u8, truncated: bool) -> Vec<u8> {
    let end = question_end(query);
    let mut reply = query[..end.unwrap_or(12)].to_vec();
    // Keep the opcode and "recursion desired"; it's an answer, with recursion.
    reply[2] = FLAG_QR | (query[2] & 0x79) | if truncated { FLAG_TC } else { 0 };
    reply[3] = 0x80 | rcode;
    reply[4..6].copy_from_slice(&[0, u8::from(end.is_some())]);
    reply[6..12].fill(0);
    reply
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A query for example.com A, with or without an EDNS OPT record.
    fn query(edns: bool) -> Vec<u8> {
        let mut q = vec![0xab, 0xcd, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, u8::from(edns)];
        q.extend_from_slice(b"\x07example\x03com\x00\x00\x01\x00\x01");
        if edns {
            q.extend_from_slice(&[0, 0, 41, 0x04, 0xd0, 0, 0, 0, 0, 0, 0]);
        }
        q
    }

    #[test]
    fn parses_plain_tls_and_https_servers() {
        let plain = Upstream::parse(" 9.9.9.9 ").unwrap();
        assert_eq!(plain, Upstream::Plain("9.9.9.9:53".parse().unwrap()));
        assert_eq!(
            Upstream::parse("[2620:fe::fe]:5353").unwrap(),
            Upstream::Plain("[2620:fe::fe]:5353".parse().unwrap())
        );

        let Upstream::Tls { addr, name } = Upstream::parse("tls://1.1.1.1").unwrap() else {
            panic!("not tls");
        };
        assert_eq!(addr, "1.1.1.1:853".parse().unwrap());
        assert!(matches!(name, ServerName::IpAddress(_)));

        let Upstream::Tls { addr, name } = Upstream::parse("tls://dns.quad9.net#9.9.9.9").unwrap()
        else {
            panic!("not tls");
        };
        assert_eq!(addr, "9.9.9.9:853".parse().unwrap());
        assert_eq!(name, ServerName::try_from("dns.quad9.net").unwrap());

        let Upstream::Https {
            addr,
            authority,
            path,
            ..
        } = Upstream::parse("https://dns.google:8443/resolve?x=1#8.8.8.8").unwrap()
        else {
            panic!("not https");
        };
        assert_eq!(addr, "8.8.8.8:8443".parse().unwrap());
        assert_eq!(authority, "dns.google:8443");
        assert_eq!(path, "/resolve?x=1");

        let Upstream::Https { addr, path, .. } =
            Upstream::parse("https://[2606:4700:4700::1111]").unwrap()
        else {
            panic!("not https");
        };
        assert_eq!(addr, "[2606:4700:4700::1111]:443".parse().unwrap());
        assert_eq!(path, "/dns-query");
    }

    #[test]
    fn refuses_what_would_leak_or_isnt_a_server() {
        // A name with no address would need a lookup in the clear first.
        assert!(Upstream::parse("tls://dns.quad9.net").is_err());
        assert!(Upstream::parse("https://dns.google/dns-query").is_err());
        assert!(Upstream::parse("dns.google").is_err());
        assert!(Upstream::parse("http://1.1.1.1/dns-query").is_err());
        assert!(Upstream::parse("tls://1.1.1.1/path").is_err());
        assert!(Upstream::parse("https://user:pw@1.1.1.1/dns-query").is_err());
        assert!(Upstream::parse("").is_err());
    }

    #[test]
    fn servfail_keeps_the_id_and_question() {
        let q = query(true);
        let r = reply_header(&q, RCODE_SERVFAIL, false);
        assert!(is_reply_to(&q, &r));
        assert_eq!(r[3] & 0x0f, RCODE_SERVFAIL);
        assert_eq!(r[2] & FLAG_TC, 0);
        assert_eq!(&r[4..12], &[0, 1, 0, 0, 0, 0, 0, 0]);
        // Header plus the question, without the OPT record.
        assert_eq!(&r[12..], &query(false)[12..]);
    }

    #[test]
    fn truncated_answer_sends_the_app_to_tcp() {
        let q = query(false);
        let r = reply_header(&q, 0, true);
        assert!(is_reply_to(&q, &r) && r[2] & FLAG_TC != 0 && r[3] & 0x0f == 0);
        // Recursion desired is echoed.
        assert_eq!(r[2] & 0x01, 1);
    }

    #[test]
    fn udp_limit_follows_edns() {
        assert_eq!(udp_limit(&query(false)), UDP_PLAIN);
        assert_eq!(udp_limit(&query(true)), UDP_EDNS);
    }

    #[test]
    fn only_queries_are_answered_and_only_matching_replies_taken() {
        let q = query(false);
        assert!(is_query(&q));
        assert!(!is_query(&q[..11]));
        let mut r = q.clone();
        r[2] |= FLAG_QR;
        assert!(!is_query(&r) && is_reply_to(&q, &r));
        r[1] ^= 1;
        assert!(!is_reply_to(&q, &r));
    }

    #[test]
    fn network_dns_skips_stubs_and_placeholders() {
        let ok = |s: &str| is_network_dns(&s.parse().unwrap());
        assert!(ok("192.168.1.1") && ok("8.8.8.8") && ok("fe80::1") && ok("2001:db8::53"));
        assert!(!ok("127.0.0.53") && !ok("::1") && !ok("10.77.0.53") && !ok("fec0:0:0:ffff::1"));
        assert!(!ok("0.0.0.0"));
    }

    #[test]
    fn manual_needs_a_server() {
        let router = Arc::new(Router::new(Arc::default(), Default::default()));
        assert!(Resolver::new(router.clone(), DnsMode::Manual, vec![]).is_err());
        assert!(Resolver::new(router, DnsMode::Auto, vec![]).is_ok());
    }

    /// Resolves example.com through every server kind on this machine's
    /// networks. Run with `cargo test live_dns -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn live_dns() {
        let links = Arc::new(crate::net::links::Links::default());
        links.refresh();
        for link in links.present() {
            link.record_success(None);
        }
        let router = Arc::new(Router::new(links, Default::default()));
        let auto = Resolver::new(router.clone(), DnsMode::Auto, vec![]).unwrap();
        let answer = auto.answer(&query(true), true).await.unwrap();
        println!("auto: {} bytes, rcode {}", answer.len(), answer[3] & 0x0f);
        assert_eq!(answer[3] & 0x0f, 0);
        for spec in [
            "9.9.9.9",
            "tls://1.1.1.1",
            "tls://dns.quad9.net#9.9.9.9",
            "https://1.1.1.1/dns-query",
            "https://dns.google/dns-query#8.8.8.8",
        ] {
            let upstream = Upstream::parse(spec).unwrap();
            let manual = Resolver::new(router.clone(), DnsMode::Manual, vec![upstream]).unwrap();
            // Twice: the second reuses the connection.
            for _ in 0..2 {
                let answer = manual.answer(&query(true), false).await.unwrap();
                println!("{spec}: {} bytes, rcode {}", answer.len(), answer[3] & 0x0f);
                assert_eq!(answer[3] & 0x0f, 0, "{spec}");
                assert_ne!(&answer[6..8], &[0, 0], "{spec} gave no answer records");
            }
        }
        // Cloudflare's certificate isn't Quad9's: the lookup must fail, not
        // go ahead unverified or in the clear.
        let wrong = Upstream::parse("tls://dns.quad9.net#1.1.1.1").unwrap();
        let manual = Resolver::new(router, DnsMode::Manual, vec![wrong]).unwrap();
        let answer = manual.answer(&query(true), false).await.unwrap();
        assert_eq!(answer[3] & 0x0f, RCODE_SERVFAIL);
    }
}
