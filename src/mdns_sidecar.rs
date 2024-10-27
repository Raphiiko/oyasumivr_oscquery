use lazy_static::lazy_static;
use log::{debug, error};
use std::process::{self, Stdio};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;

use crate::OSCQueryInitError;

// const CREATE_NO_WINDOW: u32 = 0x08000000;
const DETACHED_PROCESS: u32 = 0x00000008;

lazy_static! {
    static ref SIDECAR_STARTED: Mutex<bool> = Mutex::new(false);
    static ref KILL_TX: Mutex<Option<tokio::sync::mpsc::Sender<()>>> = Mutex::new(None);
    static ref OSC_PORT: Mutex<Option<u16>> = Mutex::new(None);
    static ref OSCQUERY_PORT: Mutex<Option<u16>> = Mutex::new(None);
    static ref SERVICE_NAME: Mutex<Option<String>> = Mutex::new(None);
    static ref CLIENT_ENABLED: Mutex<bool> = Mutex::new(false);
    static ref SERVER_ENABLED: Mutex<bool> = Mutex::new(false);
    static ref EXE_PATH: Mutex<Option<String>> = Mutex::new(None);
}

pub async fn set_exe_path(path_str: String) -> Result<(), OSCQueryInitError> {
    // Verify if there's an executable file at the path
    let path = std::path::Path::new(&path_str);
    if !path.exists() || !path.is_file() {
        return Err(OSCQueryInitError::MDNSExecutableNotFound);
    }
    {
        *EXE_PATH.lock().await = Some(path_str.clone());
    }
    Ok(())
}

pub async fn mark_server_started(
    osc_port: u16,
    oscquery_port: u16,
    service_name: String,
) -> Result<(), String> {
    {
        let mut osc_port_guard = OSC_PORT.lock().await;
        *osc_port_guard = Some(osc_port);
    }
    {
        let mut oscquery_port_guard = OSCQUERY_PORT.lock().await;
        *oscquery_port_guard = Some(oscquery_port);
    }
    {
        let mut service_name_guard = SERVICE_NAME.lock().await;
        *service_name_guard = Some(service_name);
    }
    {
        let mut server_enabled = SERVER_ENABLED.lock().await;
        *server_enabled = true;
    }
    reevaluate_sidecar_state().await
}

pub async fn mark_server_stopped() -> Result<(), String> {
    {
        let mut osc_port_guard = OSC_PORT.lock().await;
        *osc_port_guard = None;
    }
    {
        let mut oscquery_port_guard = OSCQUERY_PORT.lock().await;
        *oscquery_port_guard = None;
    }
    {
        let mut service_name_guard = SERVICE_NAME.lock().await;
        *service_name_guard = None;
    }
    {
        let mut server_enabled = SERVER_ENABLED.lock().await;
        *server_enabled = false;
    }
    reevaluate_sidecar_state().await
}

pub async fn mark_client_started() -> Result<(), String> {
    {
        let mut client_enabled = CLIENT_ENABLED.lock().await;
        *client_enabled = true;
    }
    reevaluate_sidecar_state().await
}

pub async fn mark_client_stopped() -> Result<(), String> {
    {
        let mut client_enabled: tokio::sync::MutexGuard<'_, bool> = CLIENT_ENABLED.lock().await;
        *client_enabled = false;
    }
    reevaluate_sidecar_state().await
}

async fn reevaluate_sidecar_state() -> Result<(), String> {
    let sidecar_running = {
        let started = SIDECAR_STARTED.lock().await;
        *started
    };
    let server_enabled = {
        let server_enabled = SERVER_ENABLED.lock().await;
        *server_enabled
    };
    let client_enabled = {
        let client_enabled = CLIENT_ENABLED.lock().await;
        *client_enabled
    };
    // Stop sidecar if needed
    if sidecar_running && !server_enabled && !client_enabled {
        stop_sidecar(false).await;
        return Ok(());
    }
    // Get the ports and service name
    let osc_port = {
        let osc_port_guard = OSC_PORT.lock().await;
        osc_port_guard.clone()
    };
    let oscquery_port = {
        let oscquery_port_guard = OSCQUERY_PORT.lock().await;
        oscquery_port_guard.clone()
    };
    let service_name = {
        let service_name_guard = SERVICE_NAME.lock().await;
        service_name_guard.clone()
    };
    // (Re)start sidecar
    start_sidecar(osc_port, oscquery_port, service_name).await
}

async fn start_sidecar(
    osc_port: Option<u16>,
    oscquery_port: Option<u16>,
    service_name: Option<String>,
) -> Result<(), String> {
    {
        let started = SIDECAR_STARTED.lock().await;
        if *started {
            drop(started);
            stop_sidecar(false).await;
        }
    }
    // Create a channel for killing the sidecar
    let (kill_tx, mut kill_rx) = tokio::sync::mpsc::channel::<()>(1);
    // Store the kill tx
    {
        let mut kill_tx_global = KILL_TX.lock().await;
        *kill_tx_global = Some(kill_tx);
    }
    // Start the sidecar
    let pid = process::id();
    let args: Vec<String> =
        if osc_port.is_some() && oscquery_port.is_some() && service_name.is_some() {
            vec![
                pid.to_string(),
                osc_port.unwrap().to_string(),
                oscquery_port.unwrap().to_string(),
                service_name.unwrap().to_string(),
            ]
        } else {
            vec![pid.to_string()]
        };
    let sidecar_path = {
        let guard = EXE_PATH.lock().await;
        match &*guard {
            Some(path) => path.clone(),
            None => {
                error!("Failed to start sidecar: No executable path set");
                return Err("NO_EXE_PATH".to_string());
            }
        }
    };
    let mut cmd = Command::new(sidecar_path);
    cmd.creation_flags(DETACHED_PROCESS);
    cmd.args(args);
    cmd.stdout(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            error!("Failed to start sidecar: Spawn failed: {}", e);
            return Err("SPAWN_FAILED".to_string());
        }
    };
    // Get its STDOUT
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            error!("Failed to start sidecar: No stdout");
            return Err("NO_STDOUT".to_string());
        }
    };
    // Keeps the process alive until the sidecar exits
    tokio::spawn(async move {
        let status = tokio::select! {
            _ = kill_rx.recv() => {
                child.kill().await.unwrap();
                child.wait().await.unwrap()
            }
            status = child.wait() => status.unwrap()
        };
        debug!("MDNS sidecar exited: {}", status);
    });
    // Read its lines from STDOUT
    let mut reader = BufReader::new(stdout).lines();
    tokio::spawn(async move {
        'line_reader: loop {
            let line = reader.next_line().await;
            match line {
                Ok(line) => {
                    if let Some(line) = line {
                        handle_stdout_line(line).await;
                    } else {
                        break 'line_reader;
                    }
                }
                Err(e) => {
                    error!(
                        "Failed to read line from MDNS sidecar. Forcing MDNS sidecar to quit. Details: {}",
                        e
                    );
                    stop_sidecar(true).await;
                    break 'line_reader;
                }
            }
        }
    });
    {
        let mut started = SIDECAR_STARTED.lock().await;
        *started = true;
    }
    debug!("Started MDNS sidecar");
    Ok(())
}

async fn stop_sidecar(force: bool) {
    // Only stop if started (unless we force)
    {
        let started = SIDECAR_STARTED.lock().await;
        if *started && !force {
            return;
        }
    }
    // Kill the sidecar
    {
        let mut kill_tx_guard = KILL_TX.lock().await;
        if let Some(kill_tx) = &*kill_tx_guard {
            let _ = kill_tx.send(()).await;
            // Set the kill tx to None
            *kill_tx_guard = None;
        }
    }
    // Reset the started flag
    {
        let mut started = SIDECAR_STARTED.lock().await;
        *started = false;
    }
    debug!("[OSCQUERY-MDNS] Stopped MDNS sidecar");
}

async fn handle_stdout_line(line: String) {
    crate::client::process_log_line(line).await;
}
