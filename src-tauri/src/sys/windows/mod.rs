//! The Win32 side of `sys`.

mod adapters;
mod clock;
mod dns_guard;
mod elevation;
mod icmp;
mod instance;
mod prefix_policy;
mod routes;
mod sockets;

pub use adapters::{has_connector, link_dns_servers};
pub use clock::local_date;
pub use dns_guard::DnsGuard;
pub use elevation::{can_elevate, elevates_in_place, is_elevated, relaunch_elevated, run_elevated};
pub use icmp::icmp_echo;
pub use instance::Instance;
pub use prefix_policy::PreferIpv4;
pub use routes::{add_unicast_address, Route};
pub use sockets::{is_oversized_datagram, pin_socket_to_interface, process_name, socket_owners};
