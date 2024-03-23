use std::collections::HashMap;

use crate::models::{
    Error, OSCMethod, OSCMethodAccessType, OSCMethodValueType, OSCQueryHostInfo, OSCQueryInitError,
    OSCQueryNode, OSCServiceType,
};
use http_body_util::Full;
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::convert::Infallible;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tokio::sync::watch::{channel, Sender};
use tokio::sync::Mutex;

lazy_static! {
    static ref INITIALIZED: Mutex<bool> = Mutex::new(false);
    static ref MDNS_ENABLED: Mutex<bool> = Mutex::new(false);
    static ref MDNS_SERVICE_NAME: Mutex<Option<String>> = Mutex::default();
    static ref MDNS_OSC_SERVICE_FULL_NAME: Mutex<Option<String>> = Mutex::default();
    static ref MDNS_OSC_QUERY_SERVICE_FULL_NAME: Mutex<Option<String>> = Mutex::default();
    static ref OSC_METHODS: Mutex<Vec<OSCMethod>> = Mutex::new(vec![]);
    static ref OSCQUERY_ROOT_NODE: Mutex<Option<OSCQueryNode>> = Mutex::default();
    static ref OSC_HOST: Mutex<Option<String>> = Mutex::default();
    static ref OSC_PORT: Mutex<Option<u16>> = Mutex::default();
    static ref OSCQUERY_HOST: Mutex<Option<String>> = Mutex::default();
    static ref OSCQUERY_PORT: Mutex<Option<u16>> = Mutex::default();
    static ref OSCQUERY_SHUTDOWN_SENDER: Mutex<Option<Sender<bool>>> = Mutex::default();
}

pub async fn init(
    service_name: &str,
    osc_host: &str,
    osc_port: u16,
    mdns_enabled: bool,
) -> Result<(String, u16), Error> {
    // Ensure single initialization
    {
        let mut initialized = INITIALIZED.lock().await;
        if *initialized {
            return Err(Error::InitError(OSCQueryInitError::AlreadyInitialized));
        }
        *initialized = true;
    }
    // Initialize MDNS daemon
    if mdns_enabled {
        crate::init_mdns_daemon().await?;
    }
    *MDNS_ENABLED.lock().await = mdns_enabled;
    // Store service name
    {
        let mut mdns_service_name = MDNS_SERVICE_NAME.lock().await;
        *mdns_service_name = Some(service_name.to_string());
    }
    // Store OSC host and port
    {
        let mut osc_host_ref = OSC_HOST.lock().await;
        *osc_host_ref = Some(osc_host.to_string());
    }
    {
        let mut osc_port_ref = OSC_PORT.lock().await;
        *osc_port_ref = Some(osc_port);
    }
    // Initialize the OSCQuery service
    let (oscquery_host, oscquery_port, oscquery_shutdown_sender) =
        match start_oscquery_service().await {
            Ok(port) => port,
            Err(e) => {
                println!("Could not start the OSCQuery service: {:#?}", e);
                return Err(Error::InitError(OSCQueryInitError::OSCQueryinitFailed));
            }
        };
    // Store OSCQuery host
    {
        let mut oscquery_host_ref = OSCQUERY_HOST.lock().await;
        *oscquery_host_ref = Some(oscquery_host.clone());
    }
    // Store OSCQuery port
    {
        let mut oscquery_port_ref = OSCQUERY_PORT.lock().await;
        *oscquery_port_ref = Some(oscquery_port);
    }
    // Store OSCQuery shutdown sender
    {
        let mut oscquery_shutdown_sender_ref = OSCQUERY_SHUTDOWN_SENDER.lock().await;
        *oscquery_shutdown_sender_ref = Some(oscquery_shutdown_sender);
    }
    Ok((oscquery_host, oscquery_port))
}

pub async fn deinit() -> Result<(), Error> {
    let mdns_enabled = {
        let mdns_enabled = MDNS_ENABLED.lock().await;
        *mdns_enabled
    };
    // Ensure to only deinitialize if already initialized
    {
        let initialized = INITIALIZED.lock().await;
        if !*initialized {
            return Err(Error::InitError(OSCQueryInitError::NotYetInitialized));
        }
    }
    // Deregister previous OSC service if needed
    if mdns_enabled {
        let mut mdns_osc_service_full_name = MDNS_OSC_SERVICE_FULL_NAME.lock().await;
        if let Some(name) = mdns_osc_service_full_name.as_ref() {
            let daemon = crate::MDNS_DAEMON.lock().await;
            let daemon = daemon.as_ref().unwrap();
            daemon
                .unregister(name)
                .expect("Could not deregister previous OSC MDNS service.");
            *mdns_osc_service_full_name = None;
        }
    }
    // Deregister previous OSCQuery service if needed
    if mdns_enabled {
        let mut mdns_oscquery_service_full_name = MDNS_OSC_QUERY_SERVICE_FULL_NAME.lock().await;
        if let Some(name) = mdns_oscquery_service_full_name.as_ref() {
            let daemon = crate::MDNS_DAEMON.lock().await;
            let daemon = daemon.as_ref().unwrap();
            daemon
                .unregister(name)
                .expect("Could not deregister previous OSCQuery MDNS service.");
            *mdns_oscquery_service_full_name = None;
        }
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
        *OSC_HOST.lock().await = None;
        *OSC_PORT.lock().await = None;
        *OSCQUERY_HOST.lock().await = None;
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

pub async fn advertise() -> Result<(), Error> {
    // Only advertise if mdns is enabled
    if !*MDNS_ENABLED.lock().await {
        return Ok(());
    }
    // Ensure single initialization
    {
        let initialized = INITIALIZED.lock().await;
        if !*initialized {
            return Err(Error::InitError(OSCQueryInitError::NotYetInitialized));
        }
    }

    let osc_host = {
        let osc_host = OSC_HOST.lock().await;
        osc_host.as_ref().unwrap().clone()
    };
    let osc_port = {
        let osc_port = OSC_PORT.lock().await;
        *osc_port.as_ref().unwrap()
    };
    let oscquery_host = match local_ip_address::local_ip() {
        Ok(ip) => match ip {
            std::net::IpAddr::V4(ip) => ip.to_string(),
            std::net::IpAddr::V6(_) => return Err(Error::IPV4Unavailable()),
        },
        Err(e) => {
            return Err(Error::LocalIpUnavailable(e));
        }
    };
    let oscquery_port = {
        let oscquery_port = OSCQUERY_PORT.lock().await;
        *oscquery_port.as_ref().unwrap()
    };
    // Start advertising OSC service
    set_osc_address(&osc_host, osc_port).await;
    // Start advertising the OSCQuery service
    set_oscquery_address(&oscquery_host, oscquery_port).await;
    Ok(())
}

/// Can be used to change the advertised OSC address after initialization, or advertisements have started.
pub async fn set_osc_address(host: &str, port: u16) {
    // Only advertise if mdns is enabled
    if !*MDNS_ENABLED.lock().await {
        return;
    }
    // Get the service name
    let mdns_service_name = {
        let mdns_service_name = MDNS_SERVICE_NAME.lock().await;
        mdns_service_name
            .as_ref()
            .expect("Tried to change the advertised OSC address before initialization.")
            .to_string()
    };
    // Deregister previous OSC service if needed
    {
        let mut mdns_osc_service_full_name = MDNS_OSC_SERVICE_FULL_NAME.lock().await;
        if let Some(name) = mdns_osc_service_full_name.as_ref() {
            let daemon = crate::MDNS_DAEMON.lock().await;
            let daemon = daemon.as_ref().unwrap();
            daemon
                .unregister(name)
                .expect("Could not deregister previous OSC MDNS service.");
            *mdns_osc_service_full_name = None;
        }
    }
    // Register OSC service
    {
        let daemon = crate::MDNS_DAEMON.lock().await;
        let daemon = daemon.as_ref().unwrap();
        let name = mdns_register_osc_service(
            daemon,
            OSCServiceType::OSC,
            host,
            port,
            mdns_service_name.as_str(),
        )
        .unwrap();
        let mut mdns_osc_service_full_name = MDNS_OSC_SERVICE_FULL_NAME.lock().await;
        *mdns_osc_service_full_name = Some(name.clone());
    }
}

async fn set_oscquery_address(host: &str, port: u16) {
    // Only advertise if mdns is enabled
    if !*MDNS_ENABLED.lock().await {
        return;
    }
    // Get the service name
    let mdns_service_name = {
        let mdns_service_name = MDNS_SERVICE_NAME.lock().await;
        mdns_service_name
            .as_ref()
            .expect("Tried to change the advertised OSCQuery address before initialization.")
            .to_string()
    };
    // Deregister previous OSCQuery service if needed
    {
        let mut mdns_oscquery_service_full_name = MDNS_OSC_QUERY_SERVICE_FULL_NAME.lock().await;
        if let Some(name) = mdns_oscquery_service_full_name.as_ref() {
            let daemon = crate::MDNS_DAEMON.lock().await;
            let daemon = daemon.as_ref().unwrap();
            daemon
                .unregister(name)
                .expect("Could not deregister previous OSCQuery MDNS service.");
            *mdns_oscquery_service_full_name = None;
        }
    }
    // Register OSCQuery service
    {
        let daemon = crate::MDNS_DAEMON.lock().await;
        let daemon = daemon.as_ref().unwrap();
        let name = mdns_register_osc_service(
            daemon,
            OSCServiceType::Query,
            host,
            port,
            mdns_service_name.as_str(),
        )
        .unwrap();
        let mut mdns_oscquery_service_full_name = MDNS_OSC_QUERY_SERVICE_FULL_NAME.lock().await;
        *mdns_oscquery_service_full_name = Some(name.clone());
    }
}

fn mdns_register_osc_service(
    daemon: &ServiceDaemon,
    osc_type: OSCServiceType,
    ip: &str,
    port: u16,
    name: &str,
) -> Result<String, mdns_sd::Error> {
    let type_as_string = match osc_type {
        OSCServiceType::OSC => "_osc._udp.local.",
        OSCServiceType::Query => "_oscjson._tcp.local.",
    };
    let properties: [(&str, &str); 1] = [("", "")];
    let service = ServiceInfo::new(
        type_as_string,
        name,
        type_as_string,
        ip,
        port,
        &properties[..],
    )
    .unwrap();
    let name = service.clone().get_fullname().to_string();
    match daemon.register(service) {
        Ok(_) => Ok(name),
        Err(e) => Err(e),
    }
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
    let osc_host = {
        let osc_host = OSC_HOST.lock().await;
        osc_host
            .as_ref()
            .expect("Tried to get the host info before initialization.")
            .to_string()
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
        osc_ip: osc_host,
        osc_port,
        extensions: HashMap::from([
            ("ACCESS".to_string(), true),
            ("VALUE".to_string(), true),
            ("DESCRIPTION".to_string(), true),
        ]),
    };
    serde_json::to_string(&host_info).unwrap()
}
