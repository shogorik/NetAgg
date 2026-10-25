use windows_sys::Win32::NetworkManagement::IpHelper::{GetIfEntry2, MIB_IF_ROW2};

/// `InterfaceAndOperStatusFlags.ConnectorPresent`.
const CONNECTOR_PRESENT: u8 = 0b0000_0100;

/// True for adapters with a physical connector (a real network card or USB
/// tether), false for virtual ones such as Hyper-V switches or VPNs.
pub fn has_connector(if_index: u32) -> bool {
    let mut row: MIB_IF_ROW2 = unsafe { std::mem::zeroed() };
    row.InterfaceIndex = if_index;
    if unsafe { GetIfEntry2(&mut row) } != 0 {
        return false;
    }
    row.InterfaceAndOperStatusFlags._bitfield & CONNECTOR_PRESENT != 0
}

/// The DNS servers Windows has for this adapter: from DHCP or router
/// advertisements, or set by hand in its properties.
pub fn link_dns_servers(if_index: u32) -> Vec<std::net::IpAddr> {
    netdev::get_interfaces()
        .into_iter()
        .find(|itf| itf.index == if_index)
        .map(|itf| itf.dns_servers)
        .unwrap_or_default()
}
