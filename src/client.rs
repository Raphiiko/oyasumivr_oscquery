use crate::Error;
use mdns_sd::ServiceEvent;
use tokio::sync::Mutex;

lazy_static! {
    static ref INITIALIZED: Mutex<bool> = Mutex::new(false);
    static ref OSC_HOST: Mutex<Option<String>> = Mutex::default();
    static ref OSC_PORT: Mutex<Option<u16>> = Mutex::default();
    static ref OSCQUERY_HOST: Mutex<Option<String>> = Mutex::default();
    static ref OSCQUERY_PORT: Mutex<Option<u16>> = Mutex::default();
}

pub async fn get_vrchat_osc_host() -> Option<String> {
    let _ = init().await;
    let osc_host = OSC_HOST.lock().await;
    osc_host.clone()
}

pub async fn get_vrchat_osc_port() -> Option<u16> {
    let _ = init().await;
    let osc_port = OSC_PORT.lock().await;
    osc_port.clone()
}

pub async fn get_vrchat_osc_address() -> Option<(String, u16)> {
    let _ = init().await;
    let osc_host = OSC_HOST.lock().await;
    let osc_port = OSC_PORT.lock().await;
    if osc_host.is_none() || osc_port.is_none() {
        return None;
    }
    let osc_host = osc_host.clone().unwrap();
    let osc_port = osc_port.clone().unwrap();
    Some((osc_host, osc_port))
}

pub async fn get_vrchat_oscquery_host() -> Option<String> {
    let _ = init().await;
    let oscquery_host = OSCQUERY_HOST.lock().await;
    oscquery_host.clone()
}

pub async fn get_vrchat_oscquery_port() -> Option<u16> {
    let _ = init().await;
    let oscquery_port = OSCQUERY_PORT.lock().await;
    oscquery_port.clone()
}

pub async fn get_vrchat_oscquery_address() -> Option<(String, u16)> {
    let _ = init().await;
    let oscquery_host = OSCQUERY_HOST.lock().await;
    let oscquery_port = OSCQUERY_PORT.lock().await;
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
    // Initialize MDNS daemon
    crate::init_mdns_daemon().await?;

    // Start browsing for VRChat OSC & OSCQuery services
    let mdns_guard = crate::MDNS_DAEMON.lock().await;
    let mdns = mdns_guard.as_ref().unwrap();
    let osc_receiver = mdns
        .browse("_osc._udp.local.")
        .expect("Could not browse for OSC services");
    let oscquery_receiver = mdns
        .browse("_oscjson._tcp.local.")
        .expect("Could not browse for OSC services");
    drop(mdns_guard);

    tokio::task::spawn(async move {
        loop {
            while let Ok(event) = osc_receiver.recv() {
                match event {
                    ServiceEvent::ServiceResolved(info) => {
                        let full_name = info.get_fullname();
                        if full_name.starts_with("VRChat-Client-")
                            && full_name.ends_with("._osc._udp.local.")
                        {
                            let host = info.get_addresses_v4().iter().next().unwrap().to_string();
                            let port = info.get_port();
                            *OSC_HOST.lock().await = Some(host);
                            *OSC_PORT.lock().await = Some(port);
                        }
                    }
                    _ => {}
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        }
    });
    tokio::task::spawn(async move {
        loop {
            while let Ok(event) = oscquery_receiver.recv() {
                match event {
                    ServiceEvent::ServiceResolved(info) => {
                        let full_name = info.get_fullname();
                        if full_name.starts_with("VRChat-Client-")
                            && full_name.ends_with("._oscjson._tcp.local.")
                        {
                            let host = info.get_addresses_v4().iter().next().unwrap().to_string();
                            let port = info.get_port();
                            *OSCQUERY_HOST.lock().await = Some(host);
                            *OSCQUERY_PORT.lock().await = Some(port);
                        }
                    }
                    _ => {}
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        }
    });

    Ok(())
}
