//! The Linux side of `sys`.

mod adapters;
mod clock;
mod default_route;
mod dns;
mod dns_guard;
mod elevation;
mod icmp;
mod instance;
mod routes;
mod rp_filter;
mod service;
mod sockets;

pub use adapters::{adapter_description, adapter_kind, has_connector};
pub use clock::local_date;
pub use default_route::prefer_interface;
pub use dns::{link_dns_servers, set_dns};
pub use dns_guard::DnsGuard;
#[cfg(feature = "gui")]
pub use elevation::relaunch_elevated;
pub use elevation::{can_elevate, elevates_in_place, is_elevated};
pub use icmp::{icmp_echo, icmp_hop};
pub use instance::Instance;
pub use routes::{add_unicast_address, Route};
pub use rp_filter::LooseRpFilter;
#[cfg(feature = "server")]
pub use service::WEBUI_USER;
pub use service::{
    check_install_secure, control_listener, control_socket_trusted, pipe_client, prepare_data_dir,
    service_data_dir, PipeClient, CONTROL_SOCKET,
};
#[cfg(feature = "gui")]
pub use service::{run_elevated, without_deleted};
pub use sockets::{is_oversized_datagram, pin_socket_to_interface, process_name, socket_owners};
