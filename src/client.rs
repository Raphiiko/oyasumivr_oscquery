use log::error;
use std::net::SocketAddr;
use std::sync::LazyLock;
use tokio::sync::Mutex;

use crate::{Error, OSCQueryInitError};

static INITIALIZED: LazyLock<Mutex<bool>> = LazyLock::new(|| Mutex::new(false));
static VRC_OSC_HOST: LazyLock<Mutex<Option<String>>> = LazyLock::new(|| Mutex::default());
static VRC_OSC_PORT: LazyLock<Mutex<Option<u16>>> = LazyLock::new(|| Mutex::default());
static VRC_OSCQUERY_HOST: LazyLock<Mutex<Option<String>>> = LazyLock::new(|| Mutex::default());
static VRC_OSCQUERY_PORT: LazyLock<Mutex<Option<u16>>> = LazyLock::new(|| Mutex::default());

pub async fn get_vrchat_osc_host() -> Option<String> {
    let osc_host = VRC_OSC_HOST.lock().await;
    osc_host.clone()
}

pub async fn get_vrchat_osc_port() -> Option<u16> {
    let osc_port = VRC_OSC_PORT.lock().await;
    osc_port.clone()
}

pub async fn get_vrchat_osc_address() -> Option<(String, u16)> {
    let osc_host = VRC_OSC_HOST.lock().await;
    let osc_port = VRC_OSC_PORT.lock().await;
    if osc_host.is_none() || osc_port.is_none() {
        return None;
    }
    let osc_host = osc_host.clone().unwrap();
    let osc_port = osc_port.clone().unwrap();
    Some((osc_host, osc_port))
}

pub async fn get_vrchat_oscquery_host() -> Option<String> {
    let oscquery_host = VRC_OSCQUERY_HOST.lock().await;
    oscquery_host.clone()
}

pub async fn get_vrchat_oscquery_port() -> Option<u16> {
    let oscquery_port = VRC_OSCQUERY_PORT.lock().await;
    oscquery_port.clone()
}

pub async fn get_vrchat_oscquery_address() -> Option<(String, u16)> {
    let oscquery_host = VRC_OSCQUERY_HOST.lock().await;
    let oscquery_port = VRC_OSCQUERY_PORT.lock().await;
    if oscquery_host.is_none() || oscquery_port.is_none() {
        return None;
    }
    let oscquery_host = oscquery_host.clone().unwrap();
    let oscquery_port = oscquery_port.clone().unwrap();
    Some((oscquery_host, oscquery_port))
}

pub async fn init() -> Result<(), Error> {
    // Stop if we've already initialized
    {
        let mut initialized = INITIALIZED.lock().await;
        if *initialized {
            return Ok(());
        }
        *initialized = true;
    }

    if let Err(e) = crate::mdns::mark_client_started().await {
        error!("Could not start native mDNS: {e}");
        *INITIALIZED.lock().await = false;
        return Err(Error::InitError(crate::OSCQueryInitError::MDNSInitFailed));
    }

    Ok(())
}

pub async fn deinit() -> Result<(), Error> {
    // Ensure to only deinitialize if already initialized
    {
        let initialized = INITIALIZED.lock().await;
        if !*initialized {
            return Err(Error::InitError(OSCQueryInitError::NotYetInitialized));
        }
    }
    if let Err(e) = crate::mdns::mark_client_stopped().await {
        error!("Could not stop native mDNS: {e}");
        return Err(Error::InitError(crate::OSCQueryInitError::MDNSInitFailed));
    }
    // Reset state
    {
        *VRC_OSC_HOST.lock().await = None;
        *VRC_OSC_PORT.lock().await = None;
        *VRC_OSCQUERY_HOST.lock().await = None;
        *VRC_OSCQUERY_PORT.lock().await = None;
        *INITIALIZED.lock().await = false;
    }
    Ok(())
}

pub(crate) async fn process_discovery(instance: String, address: SocketAddr) {
    if !instance.starts_with("VRChat-Client-") {
        return;
    }
    let address = SocketAddr::new(
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        address.port(),
    );
    if instance.ends_with("._osc._udp.local.") {
        *VRC_OSC_HOST.lock().await = Some(address.ip().to_string());
        *VRC_OSC_PORT.lock().await = Some(address.port());
    } else if instance.ends_with("._oscjson._tcp.local.") {
        *VRC_OSCQUERY_HOST.lock().await = Some(address.ip().to_string());
        *VRC_OSCQUERY_PORT.lock().await = Some(address.port());
    }
}
