//! The Win32 side of `sys`.

mod adapters;
mod clock;
mod dns_guard;
mod elevation;
mod icmp;
mod install_acl;
mod instance;
mod metric;
mod prefix_policy;
mod routes;
mod run_key;
mod service;
mod sockets;
mod wcm;

pub use adapters::{has_connector, link_dns_servers};
pub use clock::local_date;
pub use dns_guard::DnsGuard;
pub use elevation::{can_elevate, elevates_in_place, is_elevated, relaunch_elevated, run_elevated};
pub use icmp::{icmp_echo, icmp_hop};
pub use install_acl::check_install_secure;
pub use instance::Instance;
pub use metric::prefer_interface;
pub use prefix_policy::PreferIpv4;
pub use routes::{add_unicast_address, Route};
pub use run_key::{has_run_value, set_run_value};
pub use service::{
    create_pipe, pipe_client, pipe_owned_by_admins, run_service, service_data_dir, PIPE_NAME,
};
pub use sockets::{is_oversized_datagram, pin_socket_to_interface, process_name, socket_owners};
pub use wcm::{allow_simultaneous_connections, minimizes_connections};
