use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::{Arc, LazyLock};

use hickory_proto::op::{Message, MessageType, OpCode, Query, ResponseCode};
use hickory_proto::rr::rdata::{A, AAAA, PTR, SRV};
use hickory_proto::rr::{Name, RData, Record, RecordType};
use hickory_proto::serialize::binary::{BinDecodable, BinEncodable};
use log::{debug, error, warn};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, RwLock};
use tokio::task::JoinHandle;

const MDNS_PORT: u16 = 5353;
const MDNS_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
const RECORD_TTL: u32 = 120;
const OSC_SERVICE: &str = "_osc._udp.local.";
const OSCQUERY_SERVICE: &str = "_oscjson._tcp.local.";

#[derive(Debug)]
enum MdnsError {
    Io(std::io::Error),
    Proto(hickory_proto::ProtoError),
    NoInterface,
}

impl std::fmt::Display for MdnsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(formatter, "{err}"),
            Self::Proto(err) => write!(formatter, "{err}"),
            Self::NoInterface => write!(formatter, "no non-loopback IPv4 interface"),
        }
    }
}

impl From<std::io::Error> for MdnsError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<hickory_proto::ProtoError> for MdnsError {
    fn from(value: hickory_proto::ProtoError) -> Self {
        Self::Proto(value)
    }
}

struct Mdns {
    socket: Arc<UdpSocket>,
    task: JoinHandle<()>,
    registered_services: Arc<RwLock<HashMap<Name, u16>>>,
    followed_services: Arc<RwLock<HashSet<Name>>>,
}

impl Mdns {
    async fn new(discovered: mpsc::Sender<(Name, SocketAddr)>) -> Result<Self, MdnsError> {
        let interface_ip = find_multicast_interface()?;
        let socket = Arc::new(bind_multicast_socket(interface_ip)?);
        let registered_services = Arc::new(RwLock::new(HashMap::new()));
        let followed_services = Arc::new(RwLock::new(HashSet::new()));
        let service_cache = Arc::new(RwLock::new(HashMap::new()));
        let task = tokio::spawn(run_socket(
            socket.clone(),
            discovered,
            registered_services.clone(),
            followed_services.clone(),
            service_cache.clone(),
        ));

        Ok(Self {
            socket,
            task,
            registered_services,
            followed_services,
        })
    }

    async fn follow(&self, service: Name) -> Result<(), MdnsError> {
        let service_name = service.to_utf8();
        self.followed_services.write().await.insert(service.clone());
        if let Err(err) = self.query(service).await {
            warn!("Failed to send mDNS query for {service_name}: {err}");
        }
        Ok(())
    }

    async fn query(&self, service: Name) -> Result<(), MdnsError> {
        let mut message = Message::query();
        message.add_query(Query::query(service, RecordType::ANY));
        self.socket
            .send_to(
                &message.to_bytes()?,
                SocketAddr::new(IpAddr::V4(MDNS_GROUP), MDNS_PORT),
            )
            .await?;
        Ok(())
    }

    async fn register(&self, instance: Name, port: u16) -> Result<(), MdnsError> {
        self.registered_services
            .write()
            .await
            .insert(instance.clone(), port);
        self.send_announcement(instance, port, RECORD_TTL).await
    }

    async fn unregister(&self, instance: Name) {
        if let Some(port) = self.registered_services.write().await.remove(&instance) {
            let _ = self.send_announcement(instance, port, 0).await;
        }
    }

    async fn send_announcement(
        &self,
        instance: Name,
        port: u16,
        ttl: u32,
    ) -> Result<(), MdnsError> {
        send_announcement(&self.socket, instance, port, ttl).await
    }
}

impl Drop for Mdns {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn run_socket(
    socket: Arc<UdpSocket>,
    discovered: mpsc::Sender<(Name, SocketAddr)>,
    registered_services: Arc<RwLock<HashMap<Name, u16>>>,
    followed_services: Arc<RwLock<HashSet<Name>>>,
    service_cache: Arc<RwLock<HashMap<Name, SocketAddr>>>,
) {
    let mut buffer = [0u8; 4096];
    loop {
        let (length, sender) = match socket.recv_from(&mut buffer).await {
            Ok(received) => received,
            Err(err) => {
                if err.kind() != std::io::ErrorKind::ConnectionReset {
                    error!("mDNS receive failed: {err}");
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            }
        };

        let message = match Message::from_bytes(&buffer[..length]) {
            Ok(message) => message,
            Err(err) => {
                debug!("Ignoring invalid mDNS message from {sender}: {err}");
                continue;
            }
        };
        if message.metadata.response_code != ResponseCode::NoError {
            continue;
        }

        match message.metadata.message_type {
            MessageType::Query => {
                handle_query(message, &socket, &registered_services, sender).await;
            }
            MessageType::Response => {
                handle_response(
                    message,
                    &discovered,
                    &registered_services,
                    &followed_services,
                    &service_cache,
                )
                .await;
            }
        }
    }
}

async fn handle_query(
    query: Message,
    socket: &UdpSocket,
    registered_services: &RwLock<HashMap<Name, u16>>,
    sender: SocketAddr,
) {
    let services = registered_services.read().await;
    for question in &query.queries {
        for (instance, port) in services.iter() {
            let service_type = instance.trim_to(3);
            let matches_service_type = *question.name() == service_type
                && matches!(question.query_type(), RecordType::PTR | RecordType::ANY);
            let matches_instance = *question.name() == *instance;
            if !matches_service_type && !matches_instance {
                continue;
            }

            let response = create_response(instance, IpAddr::V4(Ipv4Addr::LOCALHOST), *port);
            let bytes = match response.to_bytes() {
                Ok(bytes) => bytes,
                Err(err) => {
                    error!("Failed to encode mDNS response for {instance}: {err}");
                    continue;
                }
            };
            let destination = if question.mdns_unicast_response {
                sender
            } else {
                SocketAddr::new(IpAddr::V4(MDNS_GROUP), MDNS_PORT)
            };
            if let Err(err) = socket.send_to(&bytes, destination).await {
                warn!("Failed to send mDNS response for {instance}: {err}");
            }
        }
    }
}

async fn handle_response(
    response: Message,
    discovered: &mpsc::Sender<(Name, SocketAddr)>,
    registered_services: &RwLock<HashMap<Name, u16>>,
    followed_services: &RwLock<HashSet<Name>>,
    service_cache: &RwLock<HashMap<Name, SocketAddr>>,
) {
    let followed = followed_services.read().await;
    let owned = registered_services.read().await;
    for service in extract_services(&response) {
        if !followed.contains(&service_type(&service.0)) || owned.contains_key(&service.0) {
            continue;
        }
        let mut cache = service_cache.write().await;
        if cache.insert(service.0.clone(), service.1) != Some(service.1)
            && discovered.send(service).await.is_err()
        {
            break;
        }
    }
}

fn extract_services(message: &Message) -> Vec<(Name, SocketAddr)> {
    let records: Vec<_> = message.answers.iter().chain(&message.additionals).collect();
    let mut services = Vec::new();
    for record in &records {
        let RData::PTR(ptr) = &record.data else {
            continue;
        };
        let instance = ptr.0.clone();
        for srv_record in &records {
            if srv_record.name != instance {
                continue;
            }
            let RData::SRV(srv) = &srv_record.data else {
                continue;
            };
            if let Some(address_record) = records.iter().find(|record| {
                record.name == srv.target && matches!(record.data, RData::A(_) | RData::AAAA(_))
            }) {
                let address = match &address_record.data {
                    RData::A(address) => IpAddr::V4(address.0),
                    RData::AAAA(address) => IpAddr::V6(address.0),
                    _ => continue,
                };
                services.push((instance.clone(), SocketAddr::new(address, srv.port)));
            }
        }
    }
    services
}

fn service_type(instance: &Name) -> Name {
    instance.trim_to(3)
}

async fn send_announcement(
    socket: &UdpSocket,
    instance: Name,
    port: u16,
    ttl: u32,
) -> Result<(), MdnsError> {
    let response = create_response_with_ttl(&instance, IpAddr::V4(Ipv4Addr::LOCALHOST), port, ttl);
    let bytes = response.to_bytes()?;
    if let Err(err) = socket
        .send_to(&bytes, SocketAddr::new(IpAddr::V4(MDNS_GROUP), MDNS_PORT))
        .await
    {
        warn!("Failed to send mDNS announcement for {instance}: {err}");
    }
    Ok(())
}

fn create_response(instance: &Name, address: IpAddr, port: u16) -> Message {
    create_response_with_ttl(instance, address, port, RECORD_TTL)
}

fn create_response_with_ttl(instance: &Name, address: IpAddr, port: u16, ttl: u32) -> Message {
    let mut message = Message::response(0, OpCode::Query);
    message.metadata.authoritative = true;
    message.add_answer(Record::from_rdata(
        instance.trim_to(3),
        ttl,
        RData::PTR(PTR(instance.clone())),
    ));
    message.add_additional(Record::from_rdata(
        instance.clone(),
        ttl,
        RData::SRV(SRV::new(0, 0, port, instance.clone())),
    ));
    let address_record = match address {
        IpAddr::V4(address) => RData::A(A(address)),
        IpAddr::V6(address) => RData::AAAA(AAAA(address)),
    };
    message.add_additional(Record::from_rdata(instance.clone(), ttl, address_record));
    message
}

fn bind_multicast_socket(interface_ip: Ipv4Addr) -> Result<UdpSocket, MdnsError> {
    let socket = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )?;
    socket.set_reuse_address(true)?;
    #[cfg(target_family = "unix")]
    socket.set_reuse_port(true)?;
    socket.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, MDNS_PORT).into())?;
    socket.set_multicast_loop_v4(true)?;
    socket.join_multicast_v4(&MDNS_GROUP, &interface_ip)?;
    socket.set_multicast_if_v4(&interface_ip)?;
    socket.set_nonblocking(true)?;
    Ok(UdpSocket::from_std(std::net::UdpSocket::from(socket))?)
}

fn find_multicast_interface() -> Result<Ipv4Addr, MdnsError> {
    if let Ok(probe) = std::net::UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)) {
        if probe
            .connect(SocketAddrV4::new(MDNS_GROUP, MDNS_PORT))
            .is_ok()
        {
            if let Ok(local_addr) = probe.local_addr() {
                if let IpAddr::V4(address) = local_addr.ip() {
                    if !address.is_loopback() && !address.is_unspecified() {
                        return Ok(address);
                    }
                }
            }
        }
    }

    let interfaces =
        if_addrs::get_if_addrs().map_err(|err| MdnsError::Io(std::io::Error::other(err)))?;
    interfaces
        .iter()
        .filter(|interface| !interface.is_loopback())
        .find_map(|interface| match interface.addr.ip() {
            IpAddr::V4(address) => Some(address),
            IpAddr::V6(_) => None,
        })
        .ok_or(MdnsError::NoInterface)
}

fn sanitize_service_name(name: &str) -> String {
    name.chars()
        .map(|character| {
            if character.is_control() || !character.is_ascii_alphanumeric() {
                '-'
            } else {
                character.to_ascii_lowercase()
            }
        })
        .collect()
}

static CLIENT_ENABLED: LazyLock<tokio::sync::Mutex<bool>> =
    LazyLock::new(|| tokio::sync::Mutex::new(false));
static SERVER_ENABLED: LazyLock<tokio::sync::Mutex<bool>> =
    LazyLock::new(|| tokio::sync::Mutex::new(false));
static MDNS: LazyLock<tokio::sync::Mutex<Option<Arc<Mdns>>>> =
    LazyLock::new(|| tokio::sync::Mutex::new(None));
static REGISTERED_INSTANCES: LazyLock<tokio::sync::Mutex<Vec<Name>>> =
    LazyLock::new(|| tokio::sync::Mutex::new(Vec::new()));

async fn ensure_mdns() -> Result<(), crate::OSCQueryInitError> {
    let mut mdns = MDNS.lock().await;
    if mdns.is_some() {
        return Ok(());
    }
    let (discovered_tx, mut discovered_rx) = mpsc::channel(16);
    let instance = Mdns::new(discovered_tx).await.map_err(|err| {
        debug!("Failed to initialize native mDNS: {err:?}");
        crate::OSCQueryInitError::MDNSInitFailed
    })?;
    tokio::spawn(async move {
        while let Some((instance, address)) = discovered_rx.recv().await {
            crate::client::process_discovery(instance.to_utf8(), address).await;
        }
    });
    *mdns = Some(Arc::new(instance));
    Ok(())
}

pub async fn mark_client_started() -> Result<(), String> {
    ensure_mdns()
        .await
        .map_err(|_| "MDNS_INIT_FAILED".to_string())?;
    *CLIENT_ENABLED.lock().await = true;
    let mdns = MDNS.lock().await.clone().expect("mDNS was not initialized");
    for service in [OSC_SERVICE, OSCQUERY_SERVICE] {
        let name = Name::from_ascii(service).map_err(|err| err.to_string())?;
        mdns.follow(name).await.map_err(|err| err.to_string())?;
    }
    Ok(())
}

pub async fn mark_client_stopped() -> Result<(), String> {
    *CLIENT_ENABLED.lock().await = false;
    stop_if_unused().await;
    Ok(())
}

pub async fn mark_server_started(
    osc_port: u16,
    oscquery_port: u16,
    service_name: String,
) -> Result<(), String> {
    ensure_mdns()
        .await
        .map_err(|_| "MDNS_INIT_FAILED".to_string())?;
    let mdns = MDNS.lock().await.clone().expect("mDNS was not initialized");
    unregister_current_services(&mdns).await;
    let sanitized = sanitize_service_name(&service_name);
    let osc_instance =
        Name::from_ascii(format!("{sanitized}.{OSC_SERVICE}")).map_err(|err| err.to_string())?;
    let oscquery_instance = Name::from_ascii(format!("{sanitized}.{OSCQUERY_SERVICE}"))
        .map_err(|err| err.to_string())?;
    mdns.register(osc_instance.clone(), osc_port)
        .await
        .map_err(|err| err.to_string())?;
    mdns.register(oscquery_instance.clone(), oscquery_port)
        .await
        .map_err(|err| err.to_string())?;
    *REGISTERED_INSTANCES.lock().await = vec![osc_instance, oscquery_instance];
    *SERVER_ENABLED.lock().await = true;
    Ok(())
}

pub async fn mark_server_stopped() -> Result<(), String> {
    let mdns = MDNS.lock().await.clone();
    if let Some(mdns) = mdns {
        unregister_current_services(&mdns).await;
    }
    *SERVER_ENABLED.lock().await = false;
    stop_if_unused().await;
    Ok(())
}

async fn unregister_current_services(mdns: &Arc<Mdns>) {
    for instance in REGISTERED_INSTANCES.lock().await.drain(..) {
        mdns.unregister(instance).await;
    }
}

async fn stop_if_unused() {
    let client_enabled = *CLIENT_ENABLED.lock().await;
    let server_enabled = *SERVER_ENABLED.lock().await;
    if !client_enabled && !server_enabled {
        *MDNS.lock().await = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{timeout, Duration};

    #[test]
    fn puts_ptr_in_answers_and_details_in_additionals() {
        let instance = Name::from_ascii("test._osc._udp.local.").unwrap();
        let message = create_response(&instance, IpAddr::V4(Ipv4Addr::LOCALHOST), 1234);

        assert!(matches!(&message.answers[0].data, RData::PTR(_)));
        assert_eq!(message.additionals.len(), 2);
        let services = extract_services(&message);
        assert_eq!(services.len(), 1);
        assert_eq!(
            services[0].1,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1234)
        );
    }

    #[test]
    fn extracts_ptr_records_from_either_section() {
        let instance = Name::from_ascii("test._oscjson._tcp.local.").unwrap();
        let mut message = create_response(&instance, IpAddr::V4(Ipv4Addr::LOCALHOST), 49153);
        let answer = message.answers.remove(0);
        message.additionals.push(answer);

        let services = extract_services(&message);
        assert_eq!(services.len(), 1);
        assert_eq!(
            services[0].1,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49153)
        );
    }

    #[tokio::test]
    #[ignore = "requires a working local multicast interface"]
    async fn discovers_a_registered_service() {
        let (discovered_tx, mut discovered_rx) = mpsc::channel(1);
        let listener = Mdns::new(discovered_tx).await.unwrap();
        let advertiser = Mdns::new(mpsc::channel(1).0).await.unwrap();
        let instance = Name::from_ascii("native-mdns-test._osc._udp.local.").unwrap();

        listener
            .follow(Name::from_ascii(OSC_SERVICE).unwrap())
            .await
            .unwrap();
        advertiser.register(instance.clone(), 49152).await.unwrap();

        let discovered = timeout(Duration::from_secs(5), discovered_rx.recv())
            .await
            .expect("timed out waiting for mDNS discovery")
            .expect("mDNS discovery channel closed");
        assert_eq!(discovered.0, instance);
        assert_eq!(
            discovered.1,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49152)
        );
    }
}
