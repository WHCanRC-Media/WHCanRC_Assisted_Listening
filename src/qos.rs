//! Network QoS (Quality of Service) for real-time audio.
//!
//! Sets DSCP EF (Expedited Forwarding) on UDP sockets so that routers and
//! Wi-Fi access points (via WMM) prioritize audio packets.
//!
//! On Windows, also uses the qWave API for reliable DSCP marking, since
//! Windows ignores setsockopt(IP_TOS) by default.

use std::net::{SocketAddr, UdpSocket};
use tracing::{info, warn};

/// DSCP Expedited Forwarding (46) in the TOS byte = 0xB8.
/// Maps to WMM AC_VO (Voice) — highest Wi-Fi priority.
const DSCP_EF_TOS: u32 = 0xB8;

/// Create a UDP socket with DSCP EF marking for real-time audio.
/// Falls back gracefully if QoS marking fails.
pub fn create_qos_socket(bind_addr: SocketAddr) -> anyhow::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};

    let domain = if bind_addr.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };

    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;

    // Set DSCP EF via IP_TOS (works on Linux/macOS, may be ignored on Windows)
    match socket.set_tos(DSCP_EF_TOS) {
        Ok(()) => info!("Set IP_TOS=0x{:02X} (DSCP EF) on UDP socket", DSCP_EF_TOS),
        Err(e) => warn!("Failed to set IP_TOS: {} (QoS marking may not work)", e),
    }

    socket.set_reuse_address(true)?;
    socket.bind(&bind_addr.into())?;

    let std_socket: UdpSocket = socket.into();

    // On Windows, use qWave for reliable DSCP marking
    #[cfg(windows)]
    apply_qwave_qos(&std_socket);

    Ok(std_socket)
}

/// Use the Windows qWave API to mark the socket's traffic as real-time voice.
/// This is the only reliable way to get DSCP marking on Windows (Vista+).
#[cfg(windows)]
fn apply_qwave_qos(socket: &UdpSocket) {
    use std::os::windows::io::AsRawSocket;

    match qwave_mark_voice(socket.as_raw_socket() as usize) {
        Ok(()) => info!("qWave QoS applied: traffic marked as real-time voice (DSCP EF)"),
        Err(e) => warn!("qWave QoS unavailable: {} (falling back to IP_TOS only)", e),
    }
}

#[cfg(windows)]
fn qwave_mark_voice(raw_socket: usize) -> anyhow::Result<()> {
    use std::mem;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::NetworkManagement::QoS::*;
    use windows::Win32::Networking::WinSock::{SOCKADDR, SOCKADDR_IN, SOCKET, AF_INET};

    let version = QOS_VERSION {
        MajorVersion: 1,
        MinorVersion: 0,
    };

    // Create QoS handle
    let mut qos_handle = HANDLE::default();
    let ok = unsafe { QOSCreateHandle(&version, &mut qos_handle) };
    if !ok.as_bool() {
        return Err(anyhow::anyhow!(
            "QOSCreateHandle failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    // For unconnected UDP sockets, qWave requires a destination address.
    // Use 0.0.0.0:0 as a wildcard to mark all outbound traffic on this socket.
    let dest_addr = SOCKADDR_IN {
        sin_family: AF_INET,
        sin_port: 0,
        sin_addr: unsafe { mem::zeroed() },
        sin_zero: [0; 8],
    };
    let dest_sockaddr: *const SOCKADDR = &dest_addr as *const SOCKADDR_IN as *const SOCKADDR;

    // Add socket to a voice-priority flow
    let mut flow_id: u32 = 0;
    let ok = unsafe {
        QOSAddSocketToFlow(
            qos_handle,
            SOCKET(raw_socket),
            Some(dest_sockaddr),
            QOSTrafficTypeVoice,
            Some(QOS_NON_ADAPTIVE_FLOW),
            &mut flow_id,
        )
    };
    if !ok.as_bool() {
        let err = std::io::Error::last_os_error();
        // If wildcard dest fails, try without destination (connected socket path)
        warn!("qWave with wildcard dest failed: {}, trying without dest", err);
        let ok = unsafe {
            QOSAddSocketToFlow(
                qos_handle,
                SOCKET(raw_socket),
                None,
                QOSTrafficTypeVoice,
                None,
                &mut flow_id,
            )
        };
        if !ok.as_bool() {
            return Err(anyhow::anyhow!(
                "QOSAddSocketToFlow failed: {}",
                std::io::Error::last_os_error()
            ));
        }
    }

    info!("qWave flow created (flow_id={}), setting DSCP EF", flow_id);

    // Set explicit DSCP value (EF = 46)
    let dscp_value: u32 = 46;
    let ok = unsafe {
        QOSSetFlow(
            qos_handle,
            flow_id,
            QOSSetOutgoingDSCPValue,
            mem::size_of::<u32>() as u32,
            &dscp_value as *const u32 as *const _,
            None,
            None,
        )
    };
    if !ok.as_bool() {
        warn!(
            "QOSSetFlow DSCP failed: {} (voice priority still active via traffic type)",
            std::io::Error::last_os_error()
        );
    }

    Ok(())
}
