use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use hickory_proto::op::{Message, MessageType, OpCode, Query, ResponseCode};
use hickory_proto::rr::rdata::{A, AAAA, PTR, SRV};
use hickory_proto::rr::{Name, RData, Record, RecordType};
use hickory_proto::serialize::binary::{BinDecodable, BinEncodable};
use log::{debug, error, info, warn};
use socket2::{InterfaceIndexOrAddress, SockRef};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, RwLock};
use tokio::task::JoinHandle;

const MDNS_PORT: u16 = 5353;
const MDNS_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
const RECORD_TTL: u32 = 120;
const OSC_SERVICE: &str = "_osc._udp.local.";
const OSCQUERY_SERVICE: &str = "_oscjson._tcp.local.";
/// The largest mDNS message RFC 6762 allows.
const MAX_MESSAGE_SIZE: usize = 9000;
const MAINTENANCE_TICK: Duration = Duration::from_secs(1);
const INTERFACE_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
/// The gap between repeated queries doubles from `MAINTENANCE_TICK` up to this cap.
const MAX_QUERY_INTERVAL: Duration = Duration::from_secs(60);
/// RFC 6762 section 6 allows multicasting a record at most once per this interval.
const MIN_MULTICAST_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug)]
enum MdnsError {
    Io(std::io::Error),
    Proto(hickory_proto::ProtoError),
}

impl std::fmt::Display for MdnsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(formatter, "{err}"),
            Self::Proto(err) => write!(formatter, "{err}"),
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
    interfaces: Arc<Interfaces>,
    receive_task: JoinHandle<()>,
    maintenance_task: JoinHandle<()>,
    registered_services: Arc<RwLock<HashMap<Name, u16>>>,
    followed_services: Arc<RwLock<HashSet<Name>>>,
    service_cache: Arc<RwLock<HashMap<Name, SocketAddr>>>,
    query_schedule: Arc<std::sync::Mutex<QuerySchedule>>,
}

/// When to query the followed services again, following RFC 6762 section 5.2.
struct QuerySchedule {
    last_query: tokio::time::Instant,
    interval: Duration,
}

impl QuerySchedule {
    fn new() -> Self {
        Self {
            last_query: tokio::time::Instant::now(),
            interval: MAINTENANCE_TICK,
        }
    }

    /// Returns whether a query is due, and if so, schedules the next one.
    fn take_due(&mut self) -> bool {
        if self.last_query.elapsed() < self.interval {
            return false;
        }
        self.last_query = tokio::time::Instant::now();
        self.interval = (self.interval * 2).min(MAX_QUERY_INTERVAL);
        true
    }
}

/// The interfaces the socket joined the mDNS group on, by interface index.
#[derive(Default)]
struct Interfaces {
    /// Each joined interface's index, with the IPv4 address it has now.
    joined: std::sync::Mutex<HashMap<u32, Ipv4Addr>>,
    /// Interfaces whose join already failed, so the failure is logged once.
    failed: std::sync::Mutex<HashSet<u32>>,
    /// Interfaces whose last send failed, so a failure is logged once until it recovers.
    send_failing: std::sync::Mutex<HashSet<u32>>,
    /// Every IPv4 address of this machine, so answers from other machines can be ignored.
    local_addresses: std::sync::Mutex<HashSet<Ipv4Addr>>,
    /// Hashes of queries this socket sent in the last `MIN_MULTICAST_INTERVAL`, with when they were
    /// sent, so their looped-back copies are not answered.
    own_queries: std::sync::Mutex<HashMap<u64, std::time::Instant>>,
    /// Held from choosing the outgoing interface until the send completes.
    send_lock: tokio::sync::Mutex<()>,
}

impl Mdns {
    async fn new(discovered: mpsc::Sender<(Name, SocketAddr)>) -> Result<Self, MdnsError> {
        let socket = Arc::new(bind_multicast_socket()?);
        let interfaces = Arc::new(Interfaces::default());
        refresh_interfaces(&socket, &interfaces);
        let registered_services = Arc::new(RwLock::new(HashMap::new()));
        let followed_services = Arc::new(RwLock::new(HashSet::new()));
        let service_cache = Arc::new(RwLock::new(HashMap::new()));
        let query_schedule = Arc::new(std::sync::Mutex::new(QuerySchedule::new()));
        let receive_task = tokio::spawn(run_socket(
            socket.clone(),
            interfaces.clone(),
            discovered,
            registered_services.clone(),
            followed_services.clone(),
            service_cache.clone(),
        ));
        let maintenance_task = tokio::spawn(maintain_interfaces(
            socket.clone(),
            interfaces.clone(),
            registered_services.clone(),
            followed_services.clone(),
            query_schedule.clone(),
        ));

        Ok(Self {
            socket,
            interfaces,
            receive_task,
            maintenance_task,
            registered_services,
            followed_services,
            service_cache,
            query_schedule,
        })
    }

    async fn follow(&self, service: Name) -> Result<(), MdnsError> {
        let service_name = service.to_utf8();
        self.followed_services.write().await.insert(service.clone());
        *self.query_schedule.lock().unwrap() = QuerySchedule::new();
        if let Err(err) = send_query(&self.socket, &self.interfaces, service).await {
            warn!("Failed to send mDNS query for {service_name}: {err}");
        }
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
        send_announcement(&self.socket, &self.interfaces, instance, port, ttl).await
    }
}

impl Drop for Mdns {
    fn drop(&mut self) {
        self.receive_task.abort();
        self.maintenance_task.abort();
    }
}

/// Follows interfaces that appear or disappear, and queries followed services again.
///
/// A new or readdressed interface gets the registered services announced and the followed
/// services queried straight away, as a service on it may have announced itself before.
async fn maintain_interfaces(
    socket: Arc<UdpSocket>,
    interfaces: Arc<Interfaces>,
    registered_services: Arc<RwLock<HashMap<Name, u16>>>,
    followed_services: Arc<RwLock<HashSet<Name>>>,
    query_schedule: Arc<std::sync::Mutex<QuerySchedule>>,
) {
    let mut last_refresh = tokio::time::Instant::now();
    loop {
        tokio::time::sleep(MAINTENANCE_TICK).await;
        let refresh_due = last_refresh.elapsed() >= INTERFACE_REFRESH_INTERVAL;
        if refresh_due {
            last_refresh = tokio::time::Instant::now();
        }
        let interfaces_gained = refresh_due && refresh_interfaces(&socket, &interfaces);

        // announce registered services on new or readdressed interfaces
        if interfaces_gained {
            // held until sent, so an unregister cannot send its goodbye in between
            let registered = registered_services.read().await;
            for (instance, port) in registered.iter() {
                let _ =
                    send_announcement(&socket, &interfaces, instance.clone(), *port, RECORD_TTL)
                        .await;
            }
        }

        // query followed services
        let query_due = query_schedule.lock().unwrap().take_due();
        if interfaces_gained || query_due {
            let followed: Vec<_> = followed_services.read().await.iter().cloned().collect();
            for service in followed {
                let service_name = service.to_utf8();
                if let Err(err) = send_query(&socket, &interfaces, service).await {
                    debug!("Failed to send mDNS query for {service_name}: {err}");
                }
            }
        }
    }
}

/// Joins the mDNS group on every current IPv4 interface and leaves vanished ones.
///
/// Membership is per interface index, so an interface with several addresses joins once, and an
/// address change keeps the membership. Returns whether an interface joined or changed address.
fn refresh_interfaces(socket: &UdpSocket, interfaces: &Interfaces) -> bool {
    let Some(snapshot) = current_interfaces() else {
        return false;
    };
    *interfaces.local_addresses.lock().unwrap() = snapshot.local_addresses;
    let current = snapshot.first_address_by_index;
    let socket = SockRef::from(socket);
    let mut joined = interfaces.joined.lock().unwrap();
    let mut failed = interfaces.failed.lock().unwrap();

    // leave interfaces that no longer exist
    let joined_before = joined.len();
    joined.retain(|index, _| {
        if current.contains_key(index) {
            return true;
        }
        let _ = socket.leave_multicast_v4_n(&MDNS_GROUP, &InterfaceIndexOrAddress::Index(*index));
        false
    });
    let left_interface = joined.len() != joined_before;
    failed.retain(|index| current.contains_key(index));
    interfaces
        .send_failing
        .lock()
        .unwrap()
        .retain(|index| current.contains_key(index));

    // join new interfaces and follow address changes on joined ones
    let mut joined_new_interface = false;
    let mut address_changed = false;
    for (index, address) in current {
        if let Some(joined_address) = joined.get_mut(&index) {
            if *joined_address != address {
                *joined_address = address;
                address_changed = true;
            }
            continue;
        }
        match socket.join_multicast_v4_n(&MDNS_GROUP, &InterfaceIndexOrAddress::Index(index)) {
            Ok(()) => {
                joined.insert(index, address);
                failed.remove(&index);
                joined_new_interface = true;
            }
            Err(err) => {
                if failed.insert(index) {
                    warn!("Could not join mDNS group on {address} (interface {index}): {err}");
                }
            }
        }
    }

    // log the interfaces whenever they change
    let interfaces_gained = joined_new_interface || address_changed;
    if interfaces_gained || left_interface {
        let mut addresses: Vec<_> = joined.values().collect();
        addresses.sort();
        info!("mDNS joined on interfaces {addresses:?}");
    }
    interfaces_gained
}

struct InterfaceSnapshot {
    /// Each non-loopback interface's index, with its first IPv4 address.
    first_address_by_index: HashMap<u32, Ipv4Addr>,
    /// Every IPv4 address of this machine, loopback included.
    local_addresses: HashSet<Ipv4Addr>,
}

/// Lists the current interfaces, or returns `None` when the system cannot list them.
fn current_interfaces() -> Option<InterfaceSnapshot> {
    let interfaces = match if_addrs::get_if_addrs() {
        Ok(interfaces) => interfaces,
        Err(err) => {
            warn!("Could not list network interfaces for mDNS: {err}");
            return None;
        }
    };
    let mut snapshot = InterfaceSnapshot {
        first_address_by_index: HashMap::new(),
        local_addresses: HashSet::new(),
    };
    for interface in &interfaces {
        let IpAddr::V4(address) = interface.addr.ip() else {
            continue;
        };
        snapshot.local_addresses.insert(address);
        if let (Some(index), false) = (interface.index, interface.is_loopback()) {
            snapshot
                .first_address_by_index
                .entry(index)
                .or_insert(address);
        }
    }
    Some(snapshot)
}

/// Whether a message came from this machine.
fn is_local_sender(interfaces: &Interfaces, sender: SocketAddr) -> bool {
    match sender.ip() {
        IpAddr::V4(address) => {
            address.is_loopback()
                || interfaces
                    .local_addresses
                    .lock()
                    .unwrap()
                    .contains(&address)
        }
        IpAddr::V6(address) => address.is_loopback(),
    }
}

/// The `IP_MULTICAST_IF` value that selects an interface.
///
/// Windows reads an address in 0.0.0.0/8 as an interface index, which stays valid when the
/// interface's address changes. Elsewhere the option takes the interface's address.
fn outgoing_interface(index: u32, address: Ipv4Addr) -> Ipv4Addr {
    if cfg!(windows) {
        Ipv4Addr::from(index)
    } else {
        address
    }
}

/// Sends a multicast message out of every joined interface.
///
/// Fails only when no interface could send it.
async fn send_multicast(
    socket: &UdpSocket,
    interfaces: &Interfaces,
    bytes: &[u8],
) -> Result<(), MdnsError> {
    let _send_guard = interfaces.send_lock.lock().await;
    let joined: Vec<_> = interfaces
        .joined
        .lock()
        .unwrap()
        .iter()
        .map(|(index, address)| (*index, *address))
        .collect();
    let destination = SocketAddr::new(IpAddr::V4(MDNS_GROUP), MDNS_PORT);
    let mut last_error = None;
    let mut sent = false;
    for (index, address) in joined {
        let interface = outgoing_interface(index, address);
        let result = match SockRef::from(socket).set_multicast_if_v4(&interface) {
            Ok(()) => socket.send_to(bytes, destination).await.map(|_| ()),
            Err(err) => Err(err),
        };
        let mut send_failing = interfaces.send_failing.lock().unwrap();
        match result {
            Ok(()) => {
                sent = true;
                if send_failing.remove(&index) {
                    info!("mDNS sending on {address} works again");
                }
            }
            Err(err) => {
                if send_failing.insert(index) {
                    warn!("Could not send mDNS message on {address}: {err}");
                }
                last_error = Some(err);
            }
        }
    }
    match (sent, last_error) {
        (false, Some(err)) => Err(err.into()),
        _ => Ok(()),
    }
}

async fn send_query(
    socket: &UdpSocket,
    interfaces: &Interfaces,
    service: Name,
) -> Result<(), MdnsError> {
    let mut message = Message::query();
    message.add_query(Query::query(service, RecordType::ANY));
    let bytes = message.to_bytes()?;
    {
        let now = std::time::Instant::now();
        let mut own_queries = interfaces.own_queries.lock().unwrap();
        own_queries.retain(|_, sent| now.duration_since(*sent) < MIN_MULTICAST_INTERVAL);
        own_queries.insert(message_hash(&bytes), now);
    }
    send_multicast(socket, interfaces, &bytes).await
}

fn message_hash(bytes: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(bytes, &mut hasher);
    std::hash::Hasher::finish(&hasher)
}

/// Logs the first of a run of identical receive errors, and the recovery after a run.
#[derive(Default)]
struct ReceiveErrors {
    last: Option<String>,
    count: u64,
}

impl ReceiveErrors {
    fn failed(&mut self, err: &std::io::Error) {
        self.count += 1;
        let message = err.to_string();
        if self.last.as_ref() != Some(&message) {
            error!("mDNS receive failed: {message}");
            self.last = Some(message);
        }
    }

    fn recovered(&mut self) {
        if self.count > 0 {
            info!("mDNS receive works again after {} failures", self.count);
            *self = Self::default();
        }
    }
}

async fn run_socket(
    socket: Arc<UdpSocket>,
    interfaces: Arc<Interfaces>,
    discovered: mpsc::Sender<(Name, SocketAddr)>,
    registered_services: Arc<RwLock<HashMap<Name, u16>>>,
    followed_services: Arc<RwLock<HashSet<Name>>>,
    service_cache: Arc<RwLock<HashMap<Name, SocketAddr>>>,
) {
    let mut buffer = [0u8; MAX_MESSAGE_SIZE];
    let mut receive_errors = ReceiveErrors::default();
    let mut recent_multicasts = HashMap::new();
    loop {
        let (length, sender) = match socket.recv_from(&mut buffer).await {
            Ok(received) => {
                receive_errors.recovered();
                received
            }
            Err(err) => {
                if err.kind() != std::io::ErrorKind::ConnectionReset {
                    receive_errors.failed(&err);
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
                // registered services point at localhost, so only this machine's queries count
                let own_query = interfaces
                    .own_queries
                    .lock()
                    .unwrap()
                    .get(&message_hash(&buffer[..length]))
                    .is_some_and(|sent| sent.elapsed() < MIN_MULTICAST_INTERVAL);
                if own_query || !is_local_sender(&interfaces, sender) {
                    continue;
                }
                handle_query(
                    message,
                    &socket,
                    &interfaces,
                    &registered_services,
                    &mut recent_multicasts,
                    sender,
                )
                .await;
            }
            MessageType::Response => {
                handle_response(
                    message,
                    sender,
                    &interfaces,
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

/// Answers questions about registered services.
///
/// `recent_multicasts` holds when each instance was last multicast. A query arrives once per
/// joined interface, and this keeps those copies to one multicast answer.
async fn handle_query(
    query: Message,
    socket: &UdpSocket,
    interfaces: &Interfaces,
    registered_services: &RwLock<HashMap<Name, u16>>,
    recent_multicasts: &mut HashMap<Name, std::time::Instant>,
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
            let multicast = !question.mdns_unicast_response;
            if multicast {
                let now = std::time::Instant::now();
                let sent_recently = recent_multicasts
                    .get(instance)
                    .is_some_and(|sent| now.duration_since(*sent) < MIN_MULTICAST_INTERVAL);
                if sent_recently {
                    continue;
                }
                recent_multicasts.insert(instance.clone(), now);
            }

            let response = create_response(instance, IpAddr::V4(Ipv4Addr::LOCALHOST), *port);
            let bytes = match response.to_bytes() {
                Ok(bytes) => bytes,
                Err(err) => {
                    error!("Failed to encode mDNS response for {instance}: {err}");
                    continue;
                }
            };
            let result = if multicast {
                send_multicast(socket, interfaces, &bytes).await
            } else {
                socket
                    .send_to(&bytes, sender)
                    .await
                    .map(|_| ())
                    .map_err(MdnsError::from)
            };
            if let Err(err) = result {
                debug!("Failed to send mDNS response for {instance}: {err}");
            }
        }
    }
}

/// Reports followed services that this machine announced.
///
/// Answers from other machines are ignored: a VPN or an mDNS reflector can carry another PC's
/// VRChat, and the client would otherwise point at that PC's ports on localhost.
async fn handle_response(
    response: Message,
    sender: SocketAddr,
    interfaces: &Interfaces,
    discovered: &mpsc::Sender<(Name, SocketAddr)>,
    registered_services: &RwLock<HashMap<Name, u16>>,
    followed_services: &RwLock<HashSet<Name>>,
    service_cache: &RwLock<HashMap<Name, SocketAddr>>,
) {
    if !is_local_sender(interfaces, sender) {
        return;
    }
    let followed = followed_services.read().await;
    let owned = registered_services.read().await;
    for service in extract_services(&response) {
        if !followed.contains(&service_type(&service.0)) || owned.contains_key(&service.0) {
            continue;
        }
        let mut cache = service_cache.write().await;
        if cache.insert(service.0.clone(), service.1) == Some(service.1) {
            continue;
        }
        info!(
            "Discovered mDNS service {} on port {} (from {sender})",
            service.0,
            service.1.port()
        );
        if discovered.send(service).await.is_err() {
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
    interfaces: &Interfaces,
    instance: Name,
    port: u16,
    ttl: u32,
) -> Result<(), MdnsError> {
    let response = create_response_with_ttl(&instance, IpAddr::V4(Ipv4Addr::LOCALHOST), port, ttl);
    let bytes = response.to_bytes()?;
    if let Err(err) = send_multicast(socket, interfaces, &bytes).await {
        debug!("Failed to send mDNS announcement for {instance}: {err}");
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

/// Binds the shared mDNS port; `refresh_interfaces` joins the group per interface.
fn bind_multicast_socket() -> Result<UdpSocket, MdnsError> {
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
    socket.set_nonblocking(true)?;
    Ok(UdpSocket::from_std(std::net::UdpSocket::from(socket))?)
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
    mdns.service_cache.write().await.clear();
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
    async fn ignores_answers_from_other_machines() {
        let interfaces = Interfaces::default();
        let local_address = Ipv4Addr::new(192, 0, 2, 10);
        interfaces
            .local_addresses
            .lock()
            .unwrap()
            .insert(local_address);
        let followed = RwLock::new(HashSet::from([Name::from_ascii(OSC_SERVICE).unwrap()]));
        let registered = RwLock::new(HashMap::new());
        let cache = RwLock::new(HashMap::new());
        let (discovered_tx, mut discovered_rx) = mpsc::channel(4);
        let instance = Name::from_ascii("VRChat-Client-TEST._osc._udp.local.").unwrap();
        let answer = || create_response(&instance, IpAddr::V4(Ipv4Addr::LOCALHOST), 9000);

        // another machine's VRChat, as a VPN or an mDNS reflector delivers it
        let remote = SocketAddr::from((Ipv4Addr::new(203, 0, 113, 7), MDNS_PORT));
        handle_response(
            answer(),
            remote,
            &interfaces,
            &discovered_tx,
            &registered,
            &followed,
            &cache,
        )
        .await;
        assert!(
            discovered_rx.try_recv().is_err(),
            "an answer from another machine was used"
        );

        // the same answer from this machine
        let local = SocketAddr::from((local_address, MDNS_PORT));
        handle_response(
            answer(),
            local,
            &interfaces,
            &discovered_tx,
            &registered,
            &followed,
            &cache,
        )
        .await;
        assert_eq!(discovered_rx.try_recv().unwrap().0, instance);
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

        let discovered = timeout(Duration::from_secs(5), async {
            loop {
                let service = discovered_rx
                    .recv()
                    .await
                    .expect("mDNS discovery channel closed");
                if service.0 == instance {
                    break service;
                }
            }
        })
        .await
        .expect("timed out waiting for mDNS discovery");
        assert_eq!(discovered.0, instance);
        assert_eq!(
            discovered.1,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49152)
        );
    }

    #[tokio::test]
    #[ignore = "requires a working local multicast interface"]
    async fn discovers_services_announced_on_every_interface() {
        let (discovered_tx, mut discovered_rx) = mpsc::channel(16);
        let listener = Mdns::new(discovered_tx).await.unwrap();
        listener
            .follow(Name::from_ascii(OSC_SERVICE).unwrap())
            .await
            .unwrap();

        // announce one service out of each interface
        let mut expected = HashMap::new();
        for (index, address) in current_interfaces()
            .unwrap()
            .first_address_by_index
            .into_values()
            .enumerate()
        {
            let instance =
                Name::from_ascii(format!("interface-test-{index}.{OSC_SERVICE}")).unwrap();
            let port = 40000 + index as u16;
            let bytes = create_response(&instance, IpAddr::V4(Ipv4Addr::LOCALHOST), port)
                .to_bytes()
                .unwrap();
            let sender = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
            SockRef::from(&sender)
                .set_multicast_if_v4(&address)
                .unwrap();
            if sender.send_to(&bytes, (MDNS_GROUP, MDNS_PORT)).is_ok() {
                expected.insert(instance, address);
            }
        }
        assert!(!expected.is_empty(), "no interface could send multicast");

        let mut missing = expected.clone();
        let _ = timeout(Duration::from_secs(5), async {
            while !missing.is_empty() {
                let (instance, _) = discovered_rx.recv().await.unwrap();
                missing.remove(&instance);
            }
        })
        .await;
        assert!(
            missing.is_empty(),
            "not discovered on interfaces {:?}",
            missing.values().collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    #[ignore = "requires a working local multicast interface"]
    async fn answers_a_query_once_however_many_interfaces_carry_it() {
        let advertiser = Mdns::new(mpsc::channel(1).0).await.unwrap();
        let instance = Name::from_ascii("answer-once-test._osc._udp.local.").unwrap();
        advertiser.register(instance.clone(), 49154).await.unwrap();
        let joined = advertiser.interfaces.joined.lock().unwrap().clone();
        let indexes: Vec<_> = joined.keys().copied().collect();
        let local_addresses: HashSet<IpAddr> = joined
            .values()
            .map(|address| IpAddr::V4(*address))
            .collect();

        // a querier joined on every interface
        let querier = bind_multicast_socket().unwrap();
        for index in &indexes {
            SockRef::from(&querier)
                .join_multicast_v4_n(&MDNS_GROUP, &InterfaceIndexOrAddress::Index(*index))
                .unwrap();
        }

        // one query out of every interface, so the advertiser receives a copy per interface
        let mut query = Message::query();
        query.add_query(Query::query(
            Name::from_ascii(OSC_SERVICE).unwrap(),
            RecordType::PTR,
        ));
        send_multicast(&querier, &advertiser.interfaces, &query.to_bytes().unwrap())
            .await
            .unwrap();

        // count the answers sent from this machine; a network's mDNS reflector may repeat them
        let mut answers = 0;
        let mut buffer = [0u8; MAX_MESSAGE_SIZE];
        let _ = timeout(Duration::from_millis(800), async {
            loop {
                let (length, sender) = querier.recv_from(&mut buffer).await.unwrap();
                if !local_addresses.contains(&sender.ip()) {
                    continue;
                }
                let Ok(message) = Message::from_bytes(&buffer[..length]) else {
                    continue;
                };
                let answers_instance = message.metadata.message_type == MessageType::Response
                    && extract_services(&message)
                        .iter()
                        .any(|service| service.0 == instance);
                if answers_instance {
                    answers += 1;
                }
            }
        })
        .await;
        advertiser.unregister(instance).await;
        assert!(answers > 0, "the query was not answered");
        assert!(
            answers <= indexes.len(),
            "{answers} answers for {} interfaces",
            indexes.len()
        );
    }

    #[tokio::test]
    #[ignore = "requires a working local multicast interface"]
    async fn discovers_services_on_interfaces_joined_later() {
        let (discovered_tx, mut discovered_rx) = mpsc::channel(16);
        let listener = Mdns::new(discovered_tx).await.unwrap();

        // leave every interface, as if none existed yet
        {
            let socket = SockRef::from(&*listener.socket);
            for (index, _) in listener.interfaces.joined.lock().unwrap().drain() {
                socket
                    .leave_multicast_v4_n(&MDNS_GROUP, &InterfaceIndexOrAddress::Index(index))
                    .unwrap();
            }
        }
        listener
            .follow(Name::from_ascii(OSC_SERVICE).unwrap())
            .await
            .unwrap();
        let advertiser = Mdns::new(mpsc::channel(1).0).await.unwrap();
        let instance = Name::from_ascii("late-interface-test._osc._udp.local.").unwrap();
        advertiser.register(instance.clone(), 49153).await.unwrap();
        let heard_before_join = timeout(Duration::from_secs(1), discovered_rx.recv())
            .await
            .is_ok();

        // the next refresh joins the interfaces again and queries
        let discovered = timeout(INTERFACE_REFRESH_INTERVAL * 2, async {
            loop {
                let service = discovered_rx.recv().await.unwrap();
                if service.0 == instance {
                    break service;
                }
            }
        })
        .await;
        advertiser.unregister(instance).await;
        assert!(
            !heard_before_join,
            "announcement arrived without a joined interface"
        );
        assert!(
            discovered.is_ok(),
            "not discovered after joining interfaces"
        );
    }

    #[tokio::test]
    #[ignore = "requires a working local multicast interface"]
    async fn rediscovers_after_client_reinit_while_server_stays_active() {
        async fn wait_for_osc_port(port: u16) -> bool {
            timeout(Duration::from_secs(5), async {
                loop {
                    if crate::client::get_vrchat_osc_port().await == Some(port) {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .is_ok()
        }

        let advertiser = Mdns::new(mpsc::channel(1).0).await.unwrap();
        let instance = Name::from_ascii("VRChat-Client-NATIVE-TEST._osc._udp.local.").unwrap();
        advertiser.register(instance.clone(), 49999).await.unwrap();
        crate::server::init("Native mDNS test", 49998)
            .await
            .unwrap();
        crate::server::advertise().await.unwrap();
        crate::client::init().await.unwrap();
        let first_discovery = wait_for_osc_port(49999).await;

        crate::client::deinit().await.unwrap();
        crate::client::init().await.unwrap();
        let second_discovery = wait_for_osc_port(49999).await;

        crate::client::deinit().await.unwrap();
        advertiser.unregister(instance).await;
        crate::server::deinit().await.unwrap();
        assert!(first_discovery, "initial VRChat service discovery failed");
        assert!(
            second_discovery,
            "VRChat service was not rediscovered after reinit"
        );
    }
}
