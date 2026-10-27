//! Answers the DNS flows the engine takes for itself: everything sent to the
//! adapter's resolver address and, while the user's own servers are in
//! force, every query to port 53, whichever server the app named.

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use ipstack::{IpStackTcpStream, IpStackUdpStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::task::JoinSet;

use super::MTU;
use crate::net::dns::{Resolver, DNS_PORT, RESOLVER_V4};

/// The DNS settings in force, swapped as they change while every flow keeps
/// running.
pub struct Intercept {
    resolver: RwLock<Arc<Resolver>>,
    /// Answer queries to any server, not only to the adapter's.
    enforce: AtomicBool,
}

impl Intercept {
    pub fn new(resolver: Arc<Resolver>, enforce: bool) -> Intercept {
        Intercept {
            resolver: RwLock::new(resolver),
            enforce: AtomicBool::new(enforce),
        }
    }

    pub fn set(&self, resolver: Arc<Resolver>, enforce: bool) {
        *self.resolver.write().unwrap() = resolver;
        self.enforce.store(enforce, Ordering::Relaxed);
    }

    fn resolver(&self) -> Arc<Resolver> {
        self.resolver.read().unwrap().clone()
    }

    pub fn takes(&self, dst: SocketAddr) -> bool {
        dst.port() == DNS_PORT
            && (self.enforce.load(Ordering::Relaxed) || dst.ip() == IpAddr::V4(RESOLVER_V4))
    }
}

/// One app socket's queries over UDP, answered side by side as they come,
/// each by the resolver in force when it arrives.
pub async fn udp(inbound: IpStackUdpStream, intercept: Arc<Intercept>) {
    let (mut app_r, mut app_w) = tokio::io::split(inbound);
    let mut asking = JoinSet::new();
    let mut buf = vec![0u8; MTU as usize];
    loop {
        tokio::select! {
            // The stack ends the flow (with an error) after it has been idle.
            read = app_r.read(&mut buf) => {
                let Ok(n @ 1..) = read else { break };
                let query = buf[..n].to_vec();
                let resolver = intercept.resolver();
                asking.spawn(async move { resolver.answer(&query, true).await });
            }
            Some(done) = asking.join_next() => {
                if let Ok(Some(reply)) = done {
                    if app_w.write_all(&reply).await.is_err() {
                        break;
                    }
                }
            }
        }
    }
}

/// DNS over TCP: two-byte length, message, in turn.
pub async fn tcp(mut inbound: IpStackTcpStream, intercept: Arc<Intercept>) {
    loop {
        let Ok(len) = inbound.read_u16().await else {
            return;
        };
        let mut query = vec![0u8; len as usize];
        if inbound.read_exact(&mut query).await.is_err() {
            return;
        }
        let Some(reply) = intercept.resolver().answer(&query, false).await else {
            return;
        };
        let mut framed = (reply.len() as u16).to_be_bytes().to_vec();
        framed.extend_from_slice(&reply);
        if inbound.write_all(&framed).await.is_err() {
            return;
        }
    }
}
