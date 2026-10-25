//! Which network Windows routes through by default: the one with the lowest
//! interface metric. NetAgg marks its choice with a metric of its own, so
//! a later run finds and undoes it without having to remember anything.

use std::io;

use windows_sys::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GetIpInterfaceTable, SetIpInterfaceEntry, MIB_IPINTERFACE_ROW,
    MIB_IPINTERFACE_TABLE,
};
use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};

/// Below any metric Windows picks itself (the lowest automatic one is 5).
const PREFERRED_METRIC: u32 = 2;

/// Makes `if_index` the network Windows prefers, for IPv4 and IPv6, and
/// gives any network an earlier call preferred its automatic metric back.
/// `None` only undoes. Needs administrator rights.
pub fn prefer_interface(if_index: Option<u32>) -> io::Result<()> {
    let mut first_err = None;
    for family in [AF_INET, AF_INET6] {
        for row in interface_rows(family)? {
            let ours = !row.UseAutomaticMetric && row.Metric == PREFERRED_METRIC;
            let wanted = Some(row.InterfaceIndex) == if_index;
            let result = match (ours, wanted) {
                (true, false) => set_metric(row, None),
                (false, true) => set_metric(row, Some(PREFERRED_METRIC)),
                _ => Ok(()),
            };
            if let Err(err) = result {
                first_err.get_or_insert(err);
            }
        }
    }
    first_err.map_or(Ok(()), Err)
}

fn interface_rows(family: u16) -> io::Result<Vec<MIB_IPINTERFACE_ROW>> {
    let mut table: *mut MIB_IPINTERFACE_TABLE = std::ptr::null_mut();
    let code = unsafe { GetIpInterfaceTable(family, &mut table) };
    if code != 0 {
        return Err(io::Error::from_raw_os_error(code as i32));
    }
    let rows = unsafe {
        let count = (*table).NumEntries as usize;
        let rows = std::slice::from_raw_parts((*table).Table.as_ptr(), count).to_vec();
        FreeMibTable(table as *const _);
        rows
    };
    Ok(rows)
}

/// `None` hands the metric back to Windows.
fn set_metric(mut row: MIB_IPINTERFACE_ROW, metric: Option<u32>) -> io::Result<()> {
    row.UseAutomaticMetric = metric.is_none();
    if let Some(metric) = metric {
        row.Metric = metric;
    }
    // SetIpInterfaceEntry rejects an IPv4 row unless this is 0.
    if row.Family == AF_INET {
        row.SitePrefixLength = 0;
    }
    let code = unsafe { SetIpInterfaceEntry(&mut row) };
    if code == 0 {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "couldn't change the metric of interface {}: {}",
            row.InterfaceIndex,
            io::Error::from_raw_os_error(code as i32)
        )))
    }
}
