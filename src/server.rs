use std::collections::HashMap;

use crate::models::{
    Error, OSCMethod, OSCMethodAccessType, OSCMethodValueType, OSCQueryHostInfo, OSCQueryInitError,
    OSCQueryNode,
};
use http_body_util::Full;
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use log::{debug, error};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::LazyLock;
use tokio::net::TcpListener;
use tokio::sync::watch::{channel, Sender};
use tokio::sync::Mutex;

static INITIALIZED: LazyLock<Mutex<bool>> = LazyLock::new(|| Mutex::new(false));
static MDNS_SERVICE_NAME: LazyLock<Mutex<Option<String>>> = LazyLock::new(|| Mutex::default());
static OSC_METHODS: LazyLock<Mutex<Vec<OSCMethod>>> = LazyLock::new(|| Mutex::new(vec![]));
static OSCQUERY_ROOT_NODE: LazyLock<Mutex<Option<OSCQueryNode>>> = LazyLock::new(|| Mutex::default());
static OSC_PORT: LazyLock<Mutex<Option<u16>>> = LazyLock::new(|| Mutex::default());
static OSCQUERY_PORT: LazyLock<Mutex<Option<u16>>> = LazyLock::new(|| Mutex::default());
static OSCQUERY_SHUTDOWN_SENDER: LazyLock<Mutex<Option<Sender<bool>>>> = LazyLock::new(|| Mutex::default());

pub async fn init(
    service_name: &str,
    osc_port: u16,
    mdns_sidecar_path: &str,
) -> Result<(String, u16), Error> {
    // Ensure single initialization
    {
        let mut initialized = INITIALIZED.lock().await;
        if *initialized {
            return Err(Error::InitError(OSCQueryInitError::AlreadyInitialized));
        }
        *initialized = true;
    }
    // Store service name
    {
        let mut mdns_service_name = MDNS_SERVICE_NAME.lock().await;
        *mdns_service_name = Some(service_name.to_string());
    }
    // Store OSC port
    {
        let mut osc_port_ref = OSC_PORT.lock().await;
        *osc_port_ref = Some(osc_port);
    }
    // Set the MDNS sidecar executable path
    if let Err(e) = crate::mdns_sidecar::set_exe_path(mdns_sidecar_path.to_string()).await {
        error!("Could not set the MDNS sidecar executable path: {:#?}", e);
        *INITIALIZED.lock().await = false;
        return Err(Error::InitError(e));
    }
    // Initialize the OSCQuery service
    let (oscquery_host, oscquery_port, oscquery_shutdown_sender) =
        match start_oscquery_service().await {
            Ok(port) => port,
            Err(e) => {
                error!("Could not start the OSCQuery service: {:#?}", e);
                *INITIALIZED.lock().await = false;
                return Err(Error::InitError(
                    OSCQueryInitError::OSCQueryServiceInitFailed,
                ));
            }
        };
    // Store OSCQuery port
    {
        let mut oscquery_port_ref = OSCQUERY_PORT.lock().await;
        *oscquery_port_ref = Some(oscquery_port);
        debug!("OSCQuery Port: {}", oscquery_port);
    }
    // Store OSCQuery shutdown sender
    {
        let mut oscquery_shutdown_sender_ref = OSCQUERY_SHUTDOWN_SENDER.lock().await;
        *oscquery_shutdown_sender_ref = Some(oscquery_shutdown_sender);
    }
    Ok((oscquery_host, oscquery_port))
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
    if let Err(e) = crate::mdns_sidecar::mark_server_stopped().await {
        error!("Could not stop the MDNS Sidecar: {:#?}", e);
        return Err(Error::InitError(OSCQueryInitError::MDNSInitFailed));
    }
    // Stop the OSC Query server
    {
        let mut oscquery_shutdown_sender = OSCQUERY_SHUTDOWN_SENDER.lock().await;
        if let Some(sender) = oscquery_shutdown_sender.take() {
            sender.send(false).unwrap();
        }
    }
    // Reset state
    {
        *OSC_PORT.lock().await = None;
        *OSCQUERY_PORT.lock().await = None;
        *MDNS_SERVICE_NAME.lock().await = None;
        *OSCQUERY_ROOT_NODE.lock().await = None;
        *OSC_METHODS.lock().await = vec![];
        *INITIALIZED.lock().await = false;
    }
    Ok(())
}

//
// MDNS Advertising
//

pub async fn set_osc_port(port: u16) -> Result<(), Error> {
    let mut osc_port = OSC_PORT.lock().await;
    *osc_port = Some(port);
    advertise().await
}

pub async fn advertise() -> Result<(), Error> {
    // Ensure single initialization
    {
        let initialized = INITIALIZED.lock().await;
        if !*initialized {
            return Err(Error::InitError(OSCQueryInitError::NotYetInitialized));
        }
    }
    let osc_port = {
        let osc_port = OSC_PORT.lock().await;
        *osc_port.as_ref().unwrap()
    };
    let oscquery_port = {
        let oscquery_port = OSCQUERY_PORT.lock().await;
        *oscquery_port.as_ref().unwrap()
    };
    let service_name = {
        let name = MDNS_SERVICE_NAME.lock().await;
        name.as_ref().unwrap().to_string()
    };
    match crate::mdns_sidecar::mark_server_started(osc_port, oscquery_port, service_name).await {
        Ok(_) => {}
        Err(e) => {
            error!("Failed to start MDNS sidecar: {:#?}", e);
            return Err(Error::InitError(OSCQueryInitError::MDNSInitFailed));
        }
    }
    Ok(())
}

//
// Managing OSC Methods
//

pub async fn add_osc_method(ad: OSCMethod) {
    {
        let mut osc_methods = OSC_METHODS.lock().await;
        // Check if the address is already advertised
        if let Some(index) = osc_methods.iter().position(|a| a.address == ad.address) {
            // Replace the existing advertisement
            osc_methods[index] = ad;
        } else {
            // Add the new advertisement
            osc_methods.push(ad);
        }
    }
    // Update the OSCQuery root node
    update_oscquery_root_node().await;
}

pub async fn receive_vrchat_avatar_parameters() {
    add_osc_method(OSCMethod {
        description: Some("VRChat Avatar Parameters".to_string()),
        address: "/avatar".to_string(),
        ad_type: OSCMethodAccessType::Write,
        value_type: None,
        value: None,
    })
    .await
}

pub async fn receive_vrchat_tracking_data() {
    add_osc_method(OSCMethod {
        description: Some("VRChat VR Tracking Data".to_string()),
        address: "/tracking/vrsystem".to_string(),
        ad_type: OSCMethodAccessType::Write,
        value_type: None,
        value: None,
    })
    .await
}

pub async fn remove_osc_method(full_address: String) {
    let updated = {
        let mut osc_methods = OSC_METHODS.lock().await;
        // Check if the method is already added
        if let Some(index) = osc_methods.iter().position(|a| a.address == full_address) {
            // Remove the method
            osc_methods.remove(index);
            true
        } else {
            false
        }
    };
    // Update the OSCQuery root node
    if updated {
        update_oscquery_root_node().await;
    }
}

pub async fn set_osc_method_value(full_address: String, value: Option<String>) {
    let mut osc_methods = OSC_METHODS.lock().await;
    // Check if the method is already added
    if let Some(index) = osc_methods.iter().position(|a| a.address == full_address) {
        let method = &mut osc_methods[index];
        method.value = value;
        drop(osc_methods);
        update_oscquery_root_node().await;
    }
}

async fn update_oscquery_root_node() {
    let mut root_node = OSCQueryNode {
        description: Some("Root Container".to_string()),
        full_path: "/".to_string(),
        access: 0,
        contents: HashMap::<String, OSCQueryNode>::new(),
        value_type: None,
        value: vec![],
    };
    let osc_address_advertisements = OSC_METHODS.lock().await;
    for ad in osc_address_advertisements.iter() {
        let mut current_node = &mut root_node;
        let address_parts = ad.address.split('/');
        let mut full_address = String::new();
        for part in address_parts {
            if part.is_empty() {
                continue;
            }
            full_address.push_str("/");
            full_address.push_str(part);
            current_node = current_node
                .contents
                .entry(part.to_string())
                .or_insert_with(|| OSCQueryNode {
                    description: None,
                    full_path: full_address.clone(),
                    access: 0,
                    contents: HashMap::<String, OSCQueryNode>::new(),
                    value_type: None,
                    value: vec![],
                });
        }
        current_node.description = ad.description.clone();
        current_node.access = match ad.ad_type {
            OSCMethodAccessType::Write => 2,
            OSCMethodAccessType::Read => 1,
            OSCMethodAccessType::ReadWrite => 3,
        };
        if let Some(value_type) = ad.value_type.clone() {
            current_node.value_type = Some(value_type.osc_type().to_string());
            if let Some(value) = ad.value.clone() {
                match value_type {
                    OSCMethodValueType::Bool => {
                        current_node.value = vec![serde_json::Value::Bool(value == "true")];
                    }
                    OSCMethodValueType::Int | OSCMethodValueType::Float => {
                        current_node.value =
                            vec![serde_json::Value::Number(value.parse().unwrap())];
                    }
                    OSCMethodValueType::String => {
                        current_node.value = vec![serde_json::Value::String(value)];
                    }
                }
            }
        }
    }
    OSCQUERY_ROOT_NODE.lock().await.replace(root_node);
}

//
// OSCQuery (HTTP) server
//

async fn start_oscquery_service() -> Result<(String, u16, Sender<bool>), Error> {
    // ip for all interfaces
    let addr = SocketAddr::from(([127, 0, 0, 1], 0));
    let listener = match TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(e) => {
            return Err(Error::IO(e));
        }
    };
    let ip = listener.local_addr().unwrap().ip().to_string();
    let port = listener.local_addr().unwrap().port();

    let (shutdown_sender, mut shutdown_receiver) = channel::<bool>(true);

    tokio::task::spawn(async move {
        loop {
            tokio::select! {
                accept_result = listener.accept() => {
                    match accept_result {
                        Ok((stream, _)) => {
                            let mut shutdown_receiver = shutdown_receiver.clone();
                            tokio::task::spawn(async move {
                                let io = TokioIo::new(stream);
                                tokio::select! {
                                    _ = http1::Builder::new().serve_connection(io, service_fn(handle_oscquery_request)) => {}
                                    // Shutdown signal received
                                    _ = shutdown_receiver.changed() => {}
                                }
                            });
                        }
                        Err(_) => {
                            continue;
                        }
                    }
                }
                _ = shutdown_receiver.changed() => {
                    // Shutdown signal received
                    break;
                }
            }
        }
    });
    Ok((ip, port, shutdown_sender))
}

async fn handle_oscquery_request(
    req: Request<hyper::body::Incoming>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    // Get path from request
    let path = req.uri().path().to_string();
    let json = get_json_for_osc_address(path.clone()).await;
    // Get query parameters from request
    let query = req.uri().query();
    if let Some(query) = query {
        match query {
            "HOST_INFO" => {
                let mut response =
                    Response::new(Full::new(Bytes::from(get_host_info_json().await)));
                let headers = response.headers_mut();
                headers.insert("Access-Control-Allow-Origin", "*".parse().unwrap());
                headers.insert("Content-Type", "application/json".parse().unwrap());
                return Ok(response);
            }
            _ => {
                let mut response = Response::new(Full::new(Bytes::from("Unknown Attribute")));
                *response.status_mut() = hyper::StatusCode::NO_CONTENT;
                return Ok(response);
            }
        }
    }
    match json {
        Some(json) => {
            let mut response = Response::new(Full::new(Bytes::from(json)));
            let headers = response.headers_mut();
            headers.insert("Access-Control-Allow-Origin", "*".parse().unwrap());
            headers.insert("Content-Type", "application/json".parse().unwrap());
            Ok(response)
        }
        None => {
            let mut response = Response::new(Full::new(Bytes::from("No Content")));
            *response.status_mut() = hyper::StatusCode::NO_CONTENT;
            Ok(response)
        }
    }
}

//
// Generate (JSON) Request Output
//

async fn get_json_for_osc_address(address: String) -> Option<String> {
    let root_query_node = {
        let root_query_node = OSCQUERY_ROOT_NODE.lock().await;
        match root_query_node.clone() {
            Some(node) => node,
            None => return None,
        }
    };
    let address_parts = address.split('/');
    let mut current_node = &root_query_node;
    for part in address_parts {
        if part.is_empty() {
            continue;
        }
        if let Some(node) = current_node.contents.get(part) {
            current_node = node;
        } else {
            return None;
        }
    }
    Some(serde_json::to_string(&current_node).unwrap())
}

async fn get_host_info_json() -> String {
    let service_name = {
        let name = MDNS_SERVICE_NAME.lock().await;
        name.as_ref().unwrap().to_string()
    };
    let osc_port = {
        let osc_port = OSC_PORT.lock().await;
        *osc_port
            .as_ref()
            .expect("Tried to get the host info before initialization.")
    };
    let host_info = OSCQueryHostInfo {
        name: service_name,
        osc_transport: "UDP".to_string(),
        osc_ip: "127.0.0.1".to_string(),
        osc_port,
        extensions: HashMap::from([
            ("ACCESS".to_string(), true),
            ("VALUE".to_string(), true),
            ("DESCRIPTION".to_string(), true),
        ]),
    };
    serde_json::to_string(&host_info).unwrap()
}
