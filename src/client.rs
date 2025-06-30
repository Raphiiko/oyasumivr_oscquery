use log::error;
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

pub async fn init(mdns_sidecar_path: &str) -> Result<(), Error> {
    // Stop if we've already initialized
    {
        let mut initialized = INITIALIZED.lock().await;
        if *initialized {
            return Ok(());
        }
        *initialized = true;
    }

    // Set the MDNS sidecar executable path
    if let Err(e) = crate::mdns_sidecar::set_exe_path(mdns_sidecar_path.to_string()).await {
        error!("Could not set the MDNS sidecar executable path: {:#?}", e);
        *INITIALIZED.lock().await = false;
        return Err(Error::InitError(e));
    }

    if let Err(e) = crate::mdns_sidecar::mark_client_started().await {
        error!("Could not start the MDNS Sidecar: {:#?}", e);
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
    // Stop the MDNS sidecar
    if let Err(e) = crate::mdns_sidecar::mark_client_stopped().await {
        error!("Could not stop the MDNS Sidecar: {:#?}", e);
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

pub(crate) async fn process_log_line(line: String) {
    if line.starts_with("VRC_OSC_ADDR_DISCOVERY ") {
        let parts: Vec<&str> = line.split(' ').collect();
        if parts.len() != 2 {
            error!("Invalid VRC_OSC_ADDR_DISCOVERY line: {}", line);
            return;
        }
        let addr = parts[1];
        let addr_parts: Vec<&str> = addr.split(':').collect();
        if addr_parts.len() != 2 {
            error!("Invalid VRC_OSC_ADDR_DISCOVERY address: {}", addr);
            return;
        }
        let host = addr_parts[0].to_string();
        let port = addr_parts[1].parse::<u16>();
        if port.is_err() {
            error!("Invalid VRC_OSC_ADDR_DISCOVERY port: {}", addr_parts[1]);
            return;
        }
        let port = port.unwrap();
        *VRC_OSC_HOST.lock().await = Some(host);
        *VRC_OSC_PORT.lock().await = Some(port);
    } else if line.starts_with("VRC_OSCQUERY_ADDR_DISCOVERY ") {
        let parts: Vec<&str> = line.split(' ').collect();
        if parts.len() != 2 {
            error!("Invalid VRC_OSCQUERY_ADDR_DISCOVERY line: {}", line);
            return;
        }
        let addr = parts[1];
        let addr_parts: Vec<&str> = addr.split(':').collect();
        if addr_parts.len() != 2 {
            error!("Invalid VRC_OSCQUERY_ADDR_DISCOVERY address: {}", addr);
            return;
        }
        let host = addr_parts[0].to_string();
        let port = addr_parts[1].parse::<u16>();
        if port.is_err() {
            error!("Invalid VRC_OSCQUERY_ADDR_DISCOVERY port: {}", addr_parts[1]);
            return;
        }
        let port = port.unwrap();
        *VRC_OSCQUERY_HOST.lock().await = Some(host);
        *VRC_OSCQUERY_PORT.lock().await = Some(port);
    }
}