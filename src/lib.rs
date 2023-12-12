#[macro_use]
extern crate lazy_static;

use std::collections::HashMap;

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use models::{OSCAddressAd, OSCQueryNode, OSCServiceType};
use std::convert::Infallible;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

pub mod models;
pub use models::*;

lazy_static! {
    static ref INITIALIZED: Mutex<bool> = Mutex::new(false);
    static ref MDNS_DAEMON: Mutex<Option<ServiceDaemon>> = Mutex::default();
    static ref MDNS_SERVICE_NAME: Mutex<Option<String>> = Mutex::default();
    static ref MDNS_OSC_SERVICE_FULL_NAME: Mutex<Option<String>> = Mutex::default();
    static ref MDNS_OSC_QUERY_SERVICE_FULL_NAME: Mutex<Option<String>> = Mutex::default();
    static ref OSC_ADDRESS_ADVERTISEMENTS: Mutex<Vec<OSCAddressAd>> = Mutex::new(vec![]);
    static ref OSCQUERY_ROOT_NODE: Mutex<Option<OSCQueryNode>> = Mutex::default();
    static ref OSC_HOST: Mutex<Option<String>> = Mutex::default();
    static ref OSC_PORT: Mutex<Option<u16>> = Mutex::default();
    static ref OSCQUERY_HOST: Mutex<Option<String>> = Mutex::default();
    static ref OSCQUERY_PORT: Mutex<Option<u16>> = Mutex::default();
}

pub async fn init(
    service_name: &str,
    osc_host: &str,
    osc_port: u16,
) -> Result<(), OSCQueryInitError> {
    // Ensure single initialization
    {
        let mut initialized = INITIALIZED.lock().await;
        if *initialized {
            return Err(OSCQueryInitError::AlreadyInitialized);
        }
        *initialized = true;
    }
    // Initialize MDNS daemon
    {
        let daemon = match ServiceDaemon::new() {
            Ok(daemon) => daemon,
            Err(e) => {
                println!("Could not initialize MDNS daemon: {}", e);
                return Err(OSCQueryInitError::MDNSDaemonInitFailed);
            }
        };
        let mut mdns_daemon = MDNS_DAEMON.lock().await;
        *mdns_daemon = Some(daemon);
    }
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
    let (oscquery_host, oscquery_port) = match start_oscquery_service().await {
        Ok((host, port)) => (host, port),
        Err(e) => {
            println!("Could not start the OSCQuery service: {}", e);
            return Err(OSCQueryInitError::OSCQueryinitFailed);
        }
    };
    // Store OSCQuery host and port
    {
        let mut oscquery_host_ref = OSCQUERY_HOST.lock().await;
        *oscquery_host_ref = Some(oscquery_host);
    }
    {
        let mut oscquery_port_ref = OSCQUERY_PORT.lock().await;
        *oscquery_port_ref = Some(oscquery_port);
    }
    Ok(())
}

pub async fn start_advertising() {
    let osc_host = {
        let osc_host = OSC_HOST.lock().await;
        osc_host
            .as_ref()
            .expect("Tried to start the OSC service before initialization.")
            .to_string()
    };
    let osc_port = {
        let osc_port = OSC_PORT.lock().await;
        *osc_port
            .as_ref()
            .expect("Tried to start the OSC service before initialization.")
    };
    let oscquery_host = {
        let oscquery_host = OSCQUERY_HOST.lock().await;
        oscquery_host
            .as_ref()
            .expect("Tried to start the OSCQuery service before initialization.")
            .to_string()
    };
    let oscquery_port = {
        let oscquery_port = OSCQUERY_PORT.lock().await;
        *oscquery_port
            .as_ref()
            .expect("Tried to start the OSCQuery service before initialization.")
    };
    // Start advertising OSC service
    set_osc_address(&osc_host, osc_port).await;
    // Start advertising the OSCQuery service
    set_oscquery_address(&oscquery_host, oscquery_port).await;
}

pub async fn set_osc_address(host: &str, port: u16) {
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
            let daemon = MDNS_DAEMON.lock().await;
            let daemon = daemon.as_ref().unwrap();
            daemon
                .unregister(name)
                .expect("Could not deregister previous OSC MDNS service.");
            *mdns_osc_service_full_name = None;
        }
    }
    // Register OSC service
    {
        let daemon = MDNS_DAEMON.lock().await;
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
        println!("Registered OSC service: {}", name);
    }
}

async fn set_oscquery_address(host: &str, port: u16) {
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
            let daemon = MDNS_DAEMON.lock().await;
            let daemon = daemon.as_ref().unwrap();
            daemon
                .unregister(name)
                .expect("Could not deregister previous OSCQuery MDNS service.");
            *mdns_oscquery_service_full_name = None;
        }
    }
    // Register OSCQuery service
    {
        let daemon = MDNS_DAEMON.lock().await;
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
        println!("Registered OSCQuery service: {}", name);
    }
}

pub async fn add_osc_address_advertisement(ad: OSCAddressAd) {
    {
        let mut osc_address_advertisements = OSC_ADDRESS_ADVERTISEMENTS.lock().await;
        // Check if the address is already advertised
        if let Some(index) = osc_address_advertisements
            .iter()
            .position(|a| a.address == ad.address)
        {
            // Replace the existing advertisement
            osc_address_advertisements[index] = ad;
        } else {
            // Add the new advertisement
            osc_address_advertisements.push(ad);
        }
    }
    // Update the OSCQuery root node
    update_oscquery_root_node().await;
}

pub async fn remove_address_advertisement(full_address: String) {
    let updated = {
        let mut osc_address_advertisements = OSC_ADDRESS_ADVERTISEMENTS.lock().await;
        // Check if the address is already advertised
        if let Some(index) = osc_address_advertisements
            .iter()
            .position(|a| a.address == full_address)
        {
            // Remove the advertisement
            osc_address_advertisements.remove(index);
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

async fn update_oscquery_root_node() {
    let mut root_node = OSCQueryNode {
        description: Some("root node".to_string()),
        full_path: "/".to_string(),
        access: 0,
        contents: HashMap::<String, OSCQueryNode>::new(),
        value_type: None,
        value: vec![],
    };
    let osc_address_advertisements = OSC_ADDRESS_ADVERTISEMENTS.lock().await;
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
            models::OSCAddressAdType::WriteAll => 2,
            models::OSCAddressAdType::WriteValue => 2,
            models::OSCAddressAdType::ReadValue => 1,
            models::OSCAddressAdType::ReadWriteValue => 3,
        };
        if let Some(value_type) = ad.value_type.clone() {
            current_node.value_type = Some(value_type.osc_type().to_string());
            if let Some(value) = ad.value.clone() {
                match value_type {
                    models::OSCAddressValueType::Bool => {
                        current_node.value = vec![serde_json::Value::Bool(value == "true")];
                    }
                    models::OSCAddressValueType::Int | models::OSCAddressValueType::Float => {
                        current_node.value =
                            vec![serde_json::Value::Number(value.parse().unwrap())];
                    }
                }
            }
        }
    }
    OSCQUERY_ROOT_NODE.lock().await.replace(root_node);
}

async fn start_oscquery_service() -> Result<(String, u16), std::io::Error> {
    let addr = SocketAddr::from(([127, 0, 0, 1], 0));
    // let addr = SocketAddr::from(([127, 0, 0, 1], 8083));
    let listener = match TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(e) => {
            println!("Could not start OSCQuery HTTP server: {}", e);
            return Err(e);
        }
    };
    let port = listener.local_addr().unwrap().port();

    tokio::task::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok((stream, addr)) => (stream, addr),
                Err(e) => {
                    println!("Error accepting connection: {:?}", e);
                    continue;
                }
            };
            let io = TokioIo::new(stream);
            tokio::task::spawn(async move {
                if let Err(err) = http1::Builder::new()
                    .serve_connection(io, service_fn(handle_oscquery_request))
                    .await
                {
                    println!("Error serving connection: {:?}", err);
                }
            });
        }
    });

    println!("Started OSCQuery service on port {}", port);
    Ok(("127.0.0.1".to_string(), port))
}

async fn handle_oscquery_request(
    req: Request<hyper::body::Incoming>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    // Get path from request
    let path = req.uri().path().to_string();
    let json = get_json_for_osc_address(path).await;
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
            ("RANGE".to_string(), true),
            ("CLIPMODE".to_string(), true),
            ("UNIT".to_string(), true),
            ("LISTEN".to_string(), true),
            ("PATH_CHANGED".to_string(), false),
            ("PATH_RENAMED".to_string(), false),
            ("PATH_ADDED".to_string(), true),
            ("PATH_REMOVED".to_string(), true),
            ("TAGS".to_string(), false),
            ("EXTENDED_TYPE".to_string(), false),
            ("CRITICAL".to_string(), false),
            ("OVERLOADS".to_string(), false),
            ("HTML".to_string(), false),
        ]),
    };
    serde_json::to_string(&host_info).unwrap()
}
