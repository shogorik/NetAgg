//! Blocks DNS (port 53) that would go around NetAgg while the user's own
//! servers are in force. The Windows DNS client can also ask each network's
//! servers directly, and a router's server is on-link, where the capture
//! routes don't reach. The filters live in a dynamic WFP session, so Windows
//! removes them when this process exits, however it exits.

use std::io;
use std::ptr::{null, null_mut};

use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::NetworkManagement::IpHelper::ConvertInterfaceIndexToLuid;
use windows_sys::Win32::NetworkManagement::Ndis::NET_LUID_LH;
use windows_sys::Win32::NetworkManagement::WindowsFilteringPlatform::*;
use windows_sys::Win32::System::Rpc::RPC_C_AUTHN_WINNT;

use super::elevation::wide;

/// NetAgg's own sublayer, so its permits and its block are weighed
/// against each other and not against other software's filters.
const SUBLAYER: GUID = GUID::from_u128(0x6d70_6e73_3a1f_4b8e_9c2d_4d55_4c54_4950);
const DNS_PORT: u16 = 53;

/// The filters, removed with the session when dropped.
pub struct DnsGuard {
    engine: HANDLE,
}

// The handle is only used to close the session.
unsafe impl Send for DnsGuard {}
unsafe impl Sync for DnsGuard {}

fn check(code: u32, what: &str) -> io::Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "couldn't {what} (WFP error {code:#x})"
        )))
    }
}

impl DnsGuard {
    /// Lets port 53 out only through the adapter (`tun_index`), from
    /// NetAgg itself (its upstream queries) and on loopback. Needs
    /// administrator rights.
    pub fn apply(tun_index: u32) -> io::Result<DnsGuard> {
        let mut name = wide("NetAgg DNS guard");
        let mut session: FWPM_SESSION0 = unsafe { std::mem::zeroed() };
        session.flags = FWPM_SESSION_FLAG_DYNAMIC;
        session.displayData.name = name.as_mut_ptr();
        let mut engine: HANDLE = null_mut();
        check(
            unsafe { FwpmEngineOpen0(null(), RPC_C_AUTHN_WINNT, null(), &session, &mut engine) },
            "open the Windows Filtering Platform",
        )?;
        let guard = DnsGuard { engine };
        unsafe {
            check(FwpmTransactionBegin0(engine, 0), "start a WFP transaction")?;
            match guard.add_filters(tun_index, &mut name) {
                Ok(()) => check(FwpmTransactionCommit0(engine), "commit the DNS filters")?,
                Err(err) => {
                    FwpmTransactionAbort0(engine);
                    return Err(err);
                }
            }
        }
        Ok(guard)
    }

    unsafe fn add_filters(&self, tun_index: u32, name: &mut [u16]) -> io::Result<()> {
        let mut luid = NET_LUID_LH { Value: 0 };
        check(
            ConvertInterfaceIndexToLuid(tun_index, &mut luid),
            "find the adapter",
        )?;
        let mut tun_luid = luid.Value;

        let exe = std::env::current_exe()?;
        let exe = wide(&exe.to_string_lossy());
        let mut app_id: *mut FWP_BYTE_BLOB = null_mut();
        check(
            FwpmGetAppIdFromFileName0(exe.as_ptr(), &mut app_id),
            "identify NetAgg to WFP",
        )?;
        let result = self.add_with(&mut tun_luid, app_id, name);
        FwpmFreeMemory0(&mut app_id as *mut _ as *mut *mut core::ffi::c_void);
        result
    }

    unsafe fn add_with(
        &self,
        tun_luid: *mut u64,
        app_id: *mut FWP_BYTE_BLOB,
        name: &mut [u16],
    ) -> io::Result<()> {
        let mut sublayer: FWPM_SUBLAYER0 = std::mem::zeroed();
        sublayer.subLayerKey = SUBLAYER;
        sublayer.displayData.name = name.as_mut_ptr();
        sublayer.weight = u16::MAX;
        check(
            FwpmSubLayerAdd0(self.engine, &sublayer, null_mut()),
            "add the WFP sublayer",
        )?;

        let on_tun = condition(FWPM_CONDITION_IP_LOCAL_INTERFACE, FWP_MATCH_EQUAL, |v| {
            v.r#type = FWP_UINT64;
            v.Anonymous.uint64 = tun_luid;
        });
        let from_us = condition(FWPM_CONDITION_ALE_APP_ID, FWP_MATCH_EQUAL, |v| {
            v.r#type = FWP_BYTE_BLOB_TYPE;
            v.Anonymous.byteBlob = app_id;
        });
        let loopback = condition(FWPM_CONDITION_FLAGS, FWP_MATCH_FLAGS_ALL_SET, |v| {
            v.r#type = FWP_UINT32;
            v.Anonymous.uint32 = FWP_CONDITION_FLAG_IS_LOOPBACK;
        });
        let to_dns = condition(FWPM_CONDITION_IP_REMOTE_PORT, FWP_MATCH_EQUAL, |v| {
            v.r#type = FWP_UINT16;
            v.Anonymous.uint16 = DNS_PORT;
        });

        for layer in [
            FWPM_LAYER_ALE_AUTH_CONNECT_V4,
            FWPM_LAYER_ALE_AUTH_CONNECT_V6,
        ] {
            // Within the sublayer the heaviest matching filter decides.
            for (weight, action, cond) in [
                (15, FWP_ACTION_PERMIT, on_tun),
                (14, FWP_ACTION_PERMIT, from_us),
                (13, FWP_ACTION_PERMIT, loopback),
                (1, FWP_ACTION_BLOCK, to_dns),
            ] {
                let mut cond = cond;
                let mut filter: FWPM_FILTER0 = std::mem::zeroed();
                filter.displayData.name = name.as_mut_ptr();
                filter.layerKey = layer;
                filter.subLayerKey = SUBLAYER;
                filter.weight.r#type = FWP_UINT8;
                filter.weight.Anonymous.uint8 = weight;
                filter.numFilterConditions = 1;
                filter.filterCondition = &mut cond;
                filter.action.r#type = action;
                check(
                    FwpmFilterAdd0(self.engine, &filter, null_mut(), null_mut()),
                    "add a DNS filter",
                )?;
            }
        }
        Ok(())
    }
}

fn condition(
    field: GUID,
    match_type: FWP_MATCH_TYPE,
    set: impl FnOnce(&mut FWP_CONDITION_VALUE0),
) -> FWPM_FILTER_CONDITION0 {
    let mut cond: FWPM_FILTER_CONDITION0 = unsafe { std::mem::zeroed() };
    cond.fieldKey = field;
    cond.matchType = match_type;
    set(&mut cond.conditionValue);
    cond
}

impl Drop for DnsGuard {
    fn drop(&mut self) {
        // Closing a dynamic session deletes everything it added.
        unsafe { FwpmEngineClose0(self.engine) };
    }
}
