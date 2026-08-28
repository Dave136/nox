//! Privacy-minimal DNS-SD discovery for Nox.
//!
//! Discovery is deliberately only a transport hint.  The returned endpoint
//! is untrusted until membership authorization and the authenticated sync
//! handshakes accept it.

use nox_core::DeviceId;
use mdns_sd::{
    DaemonEvent, DaemonStatus, Receiver, ResolvedService, ScopedIp, ServiceDaemon, ServiceEvent,
    ServiceInfo,
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
    net::{IpAddr, SocketAddr, SocketAddrV4, SocketAddrV6},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};

pub const SERVICE_TYPE: &str = "_nox._tcp.local.";
pub const MAX_DISCOVERED_ENDPOINTS: usize = 128;
pub const MAX_TXT_VALUE_BYTES: usize = 128;

const TXT_VERSION: &[u8] = b"1";
const TXT_MEMBER: &[u8] = b"s";
const TXT_PAIRING: &[u8] = b"p";
const EVENT_CAPACITY: usize = MAX_DISCOVERED_ENDPOINTS;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Advertisement {
    MemberPresence { device_id: DeviceId, port: u16 },
    PairingInstance { instance_id: [u8; 16], port: u16 },
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DiscoveryKey {
    Member(DeviceId),
    Pairing([u8; 16]),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredEndpoint {
    pub key: DiscoveryKey,
    pub addresses: Vec<SocketAddr>,
    pub protocol_version: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiscoveryEvent {
    Found(DiscoveredEndpoint),
    Updated(DiscoveredEndpoint),
    Removed(DiscoveryKey),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryHealth {
    Ready,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryError {
    DaemonUnavailable,
    RegisterFailed,
    BrowseFailed,
    ShutdownFailed,
    InvalidAdvertisement(&'static str),
    InvalidService(&'static str),
    CapacityReached,
    Closed,
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::DaemonUnavailable => "mDNS daemon unavailable",
            Self::RegisterFailed => "mDNS advertisement registration failed",
            Self::BrowseFailed => "mDNS browse failed",
            Self::ShutdownFailed => "mDNS shutdown failed",
            Self::InvalidAdvertisement(_) => "invalid mDNS advertisement",
            Self::InvalidService(_) => "invalid discovered mDNS service",
            Self::CapacityReached => "mDNS discovery capacity reached",
            Self::Closed => "mDNS discovery stream closed",
        };
        formatter.write_str(text)
    }
}

impl std::error::Error for DiscoveryError {}

/// Convert an advertisement into the exact, privacy-minimal TXT records.
#[must_use]
pub fn encode_txt(advertisement: &Advertisement) -> Vec<(&'static str, String)> {
    match advertisement {
        Advertisement::MemberPresence { device_id, .. } => vec![
            ("v", "1".to_owned()),
            ("m", "s".to_owned()),
            ("d", hex_lower(device_id.as_bytes())),
        ],
        Advertisement::PairingInstance { instance_id, .. } => vec![
            ("v", "1".to_owned()),
            ("m", "p".to_owned()),
            ("i", hex_lower(instance_id)),
        ],
    }
}

/// Parse one resolved DNS-SD service without making any trust decision.
pub fn parse_resolved_service(
    service: &ResolvedService,
) -> Result<DiscoveredEndpoint, DiscoveryError> {
    if service.ty_domain != SERVICE_TYPE {
        return Err(DiscoveryError::InvalidService("service type"));
    }
    if service.fullname.is_empty() {
        return Err(DiscoveryError::InvalidService("service name"));
    }
    if service.port == 0 {
        return Err(DiscoveryError::InvalidService("port"));
    }

    let mut version = None;
    let mut mode = None;
    let mut member = None;
    let mut pairing = None;
    for property in service.txt_properties.iter() {
        if property
            .val()
            .is_some_and(|value| value.len() > MAX_TXT_VALUE_BYTES)
        {
            return Err(DiscoveryError::InvalidService("TXT value length"));
        }
        let value = property.val();
        match property.key() {
            "v" if version.is_some() => {
                return Err(DiscoveryError::InvalidService("duplicate version"));
            }
            "v" => version = Some(value.ok_or(DiscoveryError::InvalidService("TXT value"))?),
            "m" if mode.is_some() => {
                return Err(DiscoveryError::InvalidService("duplicate mode"));
            }
            "m" => mode = Some(value.ok_or(DiscoveryError::InvalidService("TXT value"))?),
            "d" if member.is_some() => {
                return Err(DiscoveryError::InvalidService("duplicate device id"));
            }
            "d" => member = Some(value.ok_or(DiscoveryError::InvalidService("TXT value"))?),
            "i" if pairing.is_some() => {
                return Err(DiscoveryError::InvalidService("duplicate instance id"));
            }
            "i" => pairing = Some(value.ok_or(DiscoveryError::InvalidService("TXT value"))?),
            _ => {
                // Unknown keys are intentionally ignored for forward
                // compatibility after the required v1 fields are checked.
            }
        }
    }

    if version != Some(TXT_VERSION) {
        return Err(DiscoveryError::InvalidService("version"));
    }
    let mode = mode.ok_or(DiscoveryError::InvalidService("mode"))?;
    let key = match mode {
        TXT_MEMBER => {
            if pairing.is_some() {
                return Err(DiscoveryError::InvalidService(
                    "pairing field in member mode",
                ));
            }
            let bytes = member.ok_or(DiscoveryError::InvalidService("device id"))?;
            DiscoveryKey::Member(DeviceId::from_bytes(decode_hex::<32>(bytes, "device id")?))
        }
        TXT_PAIRING => {
            if member.is_some() {
                return Err(DiscoveryError::InvalidService(
                    "device field in pairing mode",
                ));
            }
            let bytes = pairing.ok_or(DiscoveryError::InvalidService("instance id"))?;
            DiscoveryKey::Pairing(decode_hex::<16>(bytes, "instance id")?)
        }
        _ => return Err(DiscoveryError::InvalidService("mode")),
    };

    let mut addresses = service
        .addresses
        .iter()
        .filter_map(|address| socket_addr(address, service.port))
        .collect::<Vec<_>>();
    addresses.sort_unstable();
    addresses.dedup();
    if addresses.is_empty() {
        return Err(DiscoveryError::InvalidService("addresses"));
    }

    Ok(DiscoveredEndpoint {
        key,
        addresses,
        protocol_version: 1,
    })
}

fn decode_hex<const N: usize>(
    value: &[u8],
    field: &'static str,
) -> Result<[u8; N], DiscoveryError> {
    if value.len() != N * 2 {
        return Err(DiscoveryError::InvalidService(field));
    }
    let mut output = [0_u8; N];
    for (index, pair) in value.chunks_exact(2).enumerate() {
        let high = hex_nibble(pair[0]).map_err(|_| DiscoveryError::InvalidService(field))?;
        let low = hex_nibble(pair[1]).map_err(|_| DiscoveryError::InvalidService(field))?;
        output[index] = (high << 4) | low;
    }
    Ok(output)
}

fn hex_nibble(value: u8) -> Result<u8, DiscoveryError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(DiscoveryError::InvalidService("hex")),
    }
}

fn socket_addr(address: &ScopedIp, port: u16) -> Option<SocketAddr> {
    match address {
        ScopedIp::V4(value) => {
            let ip = *value.addr();
            usable_ip(IpAddr::V4(ip)).then(|| SocketAddr::V4(SocketAddrV4::new(ip, port)))
        }
        ScopedIp::V6(value) => {
            let ip = *value.addr();
            let scope_id = value.scope_id().index;
            if ip.is_unicast_link_local() && scope_id == 0 {
                return None;
            }
            usable_ip(IpAddr::V6(ip))
                .then(|| SocketAddr::V6(SocketAddrV6::new(ip, port, 0, scope_id)))
        }
        _ => None,
    }
}

fn usable_ip(address: IpAddr) -> bool {
    !address.is_unspecified() && !address.is_multicast()
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[(byte >> 4) as usize]));
        output.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    output
}

struct InstanceContribution {
    key: DiscoveryKey,
    addresses: Vec<SocketAddr>,
    protocol_version: u16,
}

#[derive(Default)]
struct CandidateRegistry {
    instances: BTreeMap<String, InstanceContribution>,
    by_key: BTreeMap<DiscoveryKey, BTreeSet<String>>,
}

impl CandidateRegistry {
    fn resolved(
        &mut self,
        service: &ResolvedService,
    ) -> Result<Vec<DiscoveryEvent>, DiscoveryError> {
        let endpoint = parse_resolved_service(service)?;
        let fullname = service.fullname.clone();
        let old_key = self.instances.get(&fullname).map(|value| value.key.clone());
        if old_key.as_ref() != Some(&endpoint.key)
            && !self.by_key.contains_key(&endpoint.key)
            && self.by_key.len() >= MAX_DISCOVERED_ENDPOINTS
        {
            return Ok(Vec::new());
        }

        let mut events = Vec::new();
        if old_key.is_some()
            && old_key.as_ref() != Some(&endpoint.key)
            && let Some(event) = self.remove_instance(&fullname)
        {
            events.push(event);
        }

        let was_present = self.by_key.contains_key(&endpoint.key);
        if old_key.as_ref() != Some(&endpoint.key) {
            self.by_key
                .entry(endpoint.key.clone())
                .or_default()
                .insert(fullname.clone());
        }
        self.instances.insert(
            fullname,
            InstanceContribution {
                key: endpoint.key.clone(),
                addresses: endpoint.addresses,
                protocol_version: endpoint.protocol_version,
            },
        );
        let endpoint = self.endpoint(&endpoint.key)?;
        events.push(if was_present {
            DiscoveryEvent::Updated(endpoint)
        } else {
            DiscoveryEvent::Found(endpoint)
        });
        Ok(events)
    }

    fn removed(&mut self, service_type: &str, fullname: &str) -> Vec<DiscoveryEvent> {
        if service_type != SERVICE_TYPE {
            return Vec::new();
        }
        self.remove_instance(fullname).into_iter().collect()
    }

    fn remove_instance(&mut self, fullname: &str) -> Option<DiscoveryEvent> {
        let old = self.instances.remove(fullname)?;
        let instances = self.by_key.get_mut(&old.key)?;
        instances.remove(fullname);
        if instances.is_empty() {
            self.by_key.remove(&old.key);
            Some(DiscoveryEvent::Removed(old.key))
        } else {
            self.endpoint(&old.key).ok().map(DiscoveryEvent::Updated)
        }
    }

    fn endpoint(&self, key: &DiscoveryKey) -> Result<DiscoveredEndpoint, DiscoveryError> {
        let instances = self
            .by_key
            .get(key)
            .ok_or(DiscoveryError::InvalidService("candidate state"))?;
        let mut addresses = BTreeSet::new();
        let mut protocol_version = None;
        for fullname in instances {
            let contribution = self
                .instances
                .get(fullname)
                .ok_or(DiscoveryError::InvalidService("candidate state"))?;
            addresses.extend(contribution.addresses.iter().copied());
            protocol_version.get_or_insert(contribution.protocol_version);
        }
        Ok(DiscoveredEndpoint {
            key: key.clone(),
            addresses: addresses.into_iter().collect(),
            protocol_version: protocol_version
                .ok_or(DiscoveryError::InvalidService("candidate state"))?,
        })
    }
}

type PendingEvent = Result<DiscoveryEvent, DiscoveryError>;

fn pending_key(event: &PendingEvent) -> Option<&DiscoveryKey> {
    match event {
        Ok(DiscoveryEvent::Found(endpoint) | DiscoveryEvent::Updated(endpoint)) => {
            Some(&endpoint.key)
        }
        Ok(DiscoveryEvent::Removed(key)) => Some(key),
        Err(_) => None,
    }
}

fn enqueue(pending: &mut VecDeque<PendingEvent>, event: PendingEvent) {
    if let Some(key) = pending_key(&event)
        && let Some(index) = pending
            .iter()
            .position(|current| pending_key(current) == Some(key))
    {
        pending[index] = event;
        return;
    }
    if pending.len() < EVENT_CAPACITY {
        pending.push_back(event);
    }
}

fn release_registration(
    registration: &Arc<Registration>,
    registrations: &Arc<RegistrationRegistry>,
    mut unregister: impl FnMut(&str),
) {
    if !registration.active.swap(false, Ordering::AcqRel) {
        return;
    }
    let fullname = registration.fullname.lock().ok().map(|value| value.clone());
    registrations.remove(registration);
    if let Some(fullname) = fullname {
        unregister(&fullname);
    }
}

fn take_active_registration_names(
    registrations: &Arc<RegistrationRegistry>,
) -> Result<Vec<String>, DiscoveryError> {
    let registrations = registrations
        .registrations
        .lock()
        .map_err(|_| DiscoveryError::ShutdownFailed)?
        .drain(..)
        .collect::<Vec<_>>();
    let mut names = Vec::with_capacity(registrations.len());
    for registration in registrations {
        if !registration.active.swap(false, Ordering::AcqRel) {
            continue;
        }
        names.push(
            registration
                .fullname
                .lock()
                .map_err(|_| DiscoveryError::ShutdownFailed)?
                .clone(),
        );
    }
    Ok(names)
}

async fn run_browser(
    source: Receiver<ServiceEvent>,
    sender: mpsc::Sender<PendingEvent>,
    mut cancel: oneshot::Receiver<()>,
) {
    let mut registry = CandidateRegistry::default();
    let mut pending = VecDeque::new();
    let mut source_closed = false;

    loop {
        if source_closed && pending.is_empty() {
            return;
        }
        if pending.is_empty() {
            tokio::select! {
                _ = &mut cancel => return,
                event = source.recv_async(), if !source_closed => match event {
                    Ok(event) => process_service_event(event, &mut registry, &mut pending),
                    Err(_) => {
                        source_closed = true;
                        enqueue(&mut pending, Err(DiscoveryError::Closed));
                    }
                },
            }
        } else {
            let next = pending.front().cloned().expect("pending is non-empty");
            tokio::select! {
                _ = &mut cancel => return,
                sent = sender.send(next) => {
                    if sent.is_err() {
                        return;
                    }
                    pending.pop_front();
                },
                event = source.recv_async(), if !source_closed => match event {
                    Ok(event) => process_service_event(event, &mut registry, &mut pending),
                    Err(_) => {
                        source_closed = true;
                        enqueue(&mut pending, Err(DiscoveryError::Closed));
                    }
                },
            }
        }
    }
}

fn process_service_event(
    event: ServiceEvent,
    registry: &mut CandidateRegistry,
    pending: &mut VecDeque<PendingEvent>,
) {
    match event {
        ServiceEvent::ServiceResolved(service) => match registry.resolved(&service) {
            Ok(events) => {
                for event in events {
                    enqueue(pending, Ok(event));
                }
            }
            Err(error) => enqueue(pending, Err(error)),
        },
        ServiceEvent::ServiceRemoved(service_type, fullname) => {
            for event in registry.removed(&service_type, &fullname) {
                enqueue(pending, Ok(event));
            }
        }
        _ => {}
    }
}

struct Registration {
    fullname: Mutex<String>,
    active: AtomicBool,
}

struct RegistrationRegistry {
    registrations: Mutex<Vec<Arc<Registration>>>,
    closed: AtomicBool,
}

impl RegistrationRegistry {
    fn new() -> Self {
        Self {
            registrations: Mutex::new(Vec::new()),
            closed: AtomicBool::new(false),
        }
    }

    fn rename(&self, original: &str, new_name: &str) {
        let Ok(registrations) = self.registrations.lock() else {
            return;
        };
        for registration in registrations.iter() {
            let Ok(mut fullname) = registration.fullname.lock() else {
                continue;
            };
            if fullname.as_str() == original {
                *fullname = new_name.to_owned();
            }
        }
    }

    fn remove(&self, target: &Arc<Registration>) {
        if let Ok(mut registrations) = self.registrations.lock() {
            registrations.retain(|registration| !Arc::ptr_eq(registration, target));
        }
    }
}

pub struct DiscoveryService {
    daemon: ServiceDaemon,
    host_name: String,
    registrations: Arc<RegistrationRegistry>,
    health_receiver: Mutex<Option<Receiver<DaemonEvent>>>,
    shutdown_complete: bool,
}

impl fmt::Debug for DiscoveryService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DiscoveryService(<redacted>)")
    }
}

impl DiscoveryService {
    pub fn start() -> Result<Self, DiscoveryError> {
        let daemon = ServiceDaemon::new().map_err(|_| DiscoveryError::DaemonUnavailable)?;
        let health_receiver = daemon
            .monitor()
            .map_err(|_| DiscoveryError::DaemonUnavailable)?;
        let host_name = format!("{}.local.", random_label("nox-host")?);
        Ok(Self {
            daemon,
            host_name,
            registrations: Arc::new(RegistrationRegistry::new()),
            health_receiver: Mutex::new(Some(health_receiver)),
            shutdown_complete: false,
        })
    }

    pub fn advertise(
        &self,
        advertisement: Advertisement,
    ) -> Result<AdvertisementHandle, DiscoveryError> {
        if self.registrations.closed.load(Ordering::Acquire) {
            return Err(DiscoveryError::Closed);
        }
        let port = match &advertisement {
            Advertisement::MemberPresence { port, .. }
            | Advertisement::PairingInstance { port, .. } => *port,
        };
        if port == 0 {
            return Err(DiscoveryError::InvalidAdvertisement("port"));
        }
        let properties = encode_txt(&advertisement);
        if properties
            .iter()
            .any(|(_, value)| value.len() > MAX_TXT_VALUE_BYTES)
        {
            return Err(DiscoveryError::InvalidAdvertisement("TXT value length"));
        }
        let instance_name = random_label("nox")?;
        let service = ServiceInfo::new(
            SERVICE_TYPE,
            &instance_name,
            &self.host_name,
            (),
            port,
            properties.as_slice(),
        )
        .map_err(|_| DiscoveryError::RegisterFailed)?
        .enable_addr_auto();
        let fullname = service.get_fullname().to_owned();
        self.daemon
            .register(service)
            .map_err(|_| DiscoveryError::RegisterFailed)?;

        let registration = Arc::new(Registration {
            fullname: Mutex::new(fullname),
            active: AtomicBool::new(true),
        });
        let mut registrations = self
            .registrations
            .registrations
            .lock()
            .map_err(|_| DiscoveryError::RegisterFailed)?;
        if self.registrations.closed.load(Ordering::Acquire) {
            drop(registrations);
            let _ = self.daemon.unregister(
                registration
                    .fullname
                    .lock()
                    .map_err(|_| DiscoveryError::RegisterFailed)?
                    .as_str(),
            );
            return Err(DiscoveryError::Closed);
        }
        registrations.push(Arc::clone(&registration));
        Ok(AdvertisementHandle {
            daemon: self.daemon.clone(),
            registration,
            registrations: Arc::clone(&self.registrations),
        })
    }

    pub fn browse(&self) -> Result<DiscoveryStream, DiscoveryError> {
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|_| DiscoveryError::BrowseFailed)?;
        let source = self
            .daemon
            .browse(SERVICE_TYPE)
            .map_err(|_| DiscoveryError::BrowseFailed)?;
        let (sender, receiver) = mpsc::channel(EVENT_CAPACITY);
        let (cancel_sender, cancel_receiver) = oneshot::channel();
        runtime.spawn(run_browser(source, sender, cancel_receiver));
        Ok(DiscoveryStream {
            receiver,
            cancel: Some(cancel_sender),
        })
    }

    pub fn health(&self) -> Result<DiscoveryHealthStream, DiscoveryError> {
        let receiver = self
            .health_receiver
            .lock()
            .map_err(|_| DiscoveryError::Closed)?
            .take()
            .ok_or(DiscoveryError::Closed)?;
        Ok(DiscoveryHealthStream {
            receiver,
            registrations: Arc::clone(&self.registrations),
        })
    }

    pub fn shutdown(mut self) -> Result<(), DiscoveryError> {
        let result = self.shutdown_inner();
        self.shutdown_complete = true;
        result
    }

    fn shutdown_inner(&self) -> Result<(), DiscoveryError> {
        self.registrations.closed.store(true, Ordering::Release);
        let names = take_active_registration_names(&self.registrations)?;
        let mut unregister_receivers = Vec::new();
        let mut failed = false;
        for fullname in names {
            match self.daemon.unregister(&fullname) {
                Ok(receiver) => unregister_receivers.push(receiver),
                Err(_) => failed = true,
            }
        }
        let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
        for receiver in unregister_receivers {
            let timeout = deadline.saturating_duration_since(Instant::now());
            if receiver.recv_timeout(timeout).is_err() {
                failed = true;
            }
        }
        let shutdown_receiver = match self.daemon.shutdown() {
            Ok(receiver) => receiver,
            Err(_) => return Err(DiscoveryError::ShutdownFailed),
        };
        let timeout = deadline.saturating_duration_since(Instant::now());
        match shutdown_receiver.recv_timeout(timeout) {
            Ok(DaemonStatus::Shutdown) => {}
            Ok(DaemonStatus::Running) | Ok(_) | Err(_) => failed = true,
        }
        failed
            .then_some(DiscoveryError::ShutdownFailed)
            .map_or(Ok(()), Err)
    }
}

impl Drop for DiscoveryService {
    fn drop(&mut self) {
        if !self.shutdown_complete {
            let _ = self.shutdown_inner();
        }
    }
}

pub struct AdvertisementHandle {
    daemon: ServiceDaemon,
    registration: Arc<Registration>,
    registrations: Arc<RegistrationRegistry>,
}

impl fmt::Debug for AdvertisementHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AdvertisementHandle(<redacted>)")
    }
}

impl Drop for AdvertisementHandle {
    fn drop(&mut self) {
        release_registration(&self.registration, &self.registrations, |fullname| {
            let _ = self.daemon.unregister(fullname);
        });
    }
}

pub struct DiscoveryStream {
    receiver: mpsc::Receiver<PendingEvent>,
    cancel: Option<oneshot::Sender<()>>,
}

impl DiscoveryStream {
    pub async fn next(&mut self) -> Result<DiscoveryEvent, DiscoveryError> {
        self.receiver
            .recv()
            .await
            .unwrap_or(Err(DiscoveryError::Closed))
    }
}

impl Drop for DiscoveryStream {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
    }
}

pub struct DiscoveryHealthStream {
    receiver: Receiver<DaemonEvent>,
    registrations: Arc<RegistrationRegistry>,
}

impl DiscoveryHealthStream {
    pub async fn next(&mut self) -> Result<DiscoveryHealth, DiscoveryError> {
        match self.receiver.recv_async().await {
            Ok(DaemonEvent::Error(_)) => Ok(DiscoveryHealth::Unavailable),
            Ok(DaemonEvent::NameChange(change)) => {
                self.registrations
                    .rename(&change.original, &change.new_name);
                Ok(DiscoveryHealth::Ready)
            }
            Ok(_) => Ok(DiscoveryHealth::Ready),
            Err(_) => Err(DiscoveryError::Closed),
        }
    }
}

fn random_label(prefix: &str) -> Result<String, DiscoveryError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| DiscoveryError::DaemonUnavailable)?;
    Ok(format!("{prefix}-{}", hex_lower(&bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mdns_sd::{ResolvedService, ServiceInfo};
    use std::{
        collections::HashSet,
        net::{IpAddr, Ipv4Addr},
    };

    fn service(
        fullname: &str,
        properties: &[(&str, &str)],
        addresses: &[IpAddr],
    ) -> ResolvedService {
        let instance = fullname
            .strip_suffix(SERVICE_TYPE)
            .expect("test fullname has the service suffix")
            .trim_end_matches('.');
        ServiceInfo::new(
            SERVICE_TYPE,
            instance,
            "locker-host.local.",
            addresses,
            7000,
            properties,
        )
        .unwrap()
        .as_resolved_service()
    }

    #[test]
    fn txt_snapshots_are_exact_and_private() {
        let member = encode_txt(&Advertisement::MemberPresence {
            device_id: DeviceId::from_bytes([0xab; 32]),
            port: 7000,
        });
        assert_eq!(
            member,
            vec![
                ("v", "1".to_owned()),
                ("m", "s".to_owned()),
                ("d", "ab".repeat(32)),
            ]
        );
        let pairing = encode_txt(&Advertisement::PairingInstance {
            instance_id: [0xcd; 16],
            port: 7001,
        });
        assert_eq!(
            pairing,
            vec![
                ("v", "1".to_owned()),
                ("m", "p".to_owned()),
                ("i", "cd".repeat(16)),
            ]
        );
        let encoded = format!("{member:?}{pairing:?}");
        assert!(!encoded.contains("vault"));
        assert!(!encoded.contains("password"));
    }

    #[test]
    fn parser_validates_modes_hex_and_addresses() {
        let valid = service(
            "one._nox._tcp.local.",
            &[("v", "1"), ("m", "s"), ("d", &"00".repeat(32))],
            &[IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2))],
        );
        let endpoint = parse_resolved_service(&valid).unwrap();
        assert_eq!(
            endpoint.addresses,
            vec!["192.168.1.2:7000".parse().unwrap()]
        );
        assert_eq!(endpoint.protocol_version, 1);

        let invalid_cases = [
            service(
                "bad._nox._tcp.local.",
                &[("v", "2"), ("m", "s"), ("d", &"00".repeat(32))],
                &[IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2))],
            ),
            service(
                "bad._nox._tcp.local.",
                &[("v", "1"), ("m", "p"), ("d", &"00".repeat(32))],
                &[IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2))],
            ),
            service(
                "bad._nox._tcp.local.",
                &[("v", "1"), ("m", "s"), ("d", "00")],
                &[IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2))],
            ),
            service(
                "bad._nox._tcp.local.",
                &[("v", "1"), ("m", "s"), ("d", &"00".repeat(32))],
                &[IpAddr::V4(Ipv4Addr::UNSPECIFIED)],
            ),
        ];
        for invalid in &invalid_cases {
            assert!(parse_resolved_service(invalid).is_err());
        }
    }

    #[test]
    fn parser_maps_both_invalid_hex_nibbles_to_the_field() {
        assert_eq!(
            decode_hex::<1>(b"g0", "device id"),
            Err(DiscoveryError::InvalidService("device id"))
        );
        assert_eq!(
            decode_hex::<1>(b"0g", "device id"),
            Err(DiscoveryError::InvalidService("device id"))
        );
    }

    #[test]
    fn registry_coalesces_updates_and_multi_instance_removal() {
        let address_a = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2));
        let address_b = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 3));
        let properties = [("v", "1"), ("m", "s"), ("d", &"00".repeat(32))];
        let mut registry = CandidateRegistry::default();
        assert!(matches!(
            registry
                .resolved(&service("a._nox._tcp.local.", &properties, &[address_a]))
                .unwrap()
                .as_slice(),
            [DiscoveryEvent::Found(_)]
        ));
        assert!(matches!(
            registry
                .resolved(&service("b._nox._tcp.local.", &properties, &[address_b]))
                .unwrap()
                .as_slice(),
            [DiscoveryEvent::Updated(_)]
        ));
        let updated = registry
            .resolved(&service("a._nox._tcp.local.", &properties, &[address_b]))
            .unwrap();
        let DiscoveryEvent::Updated(endpoint) = &updated[0] else {
            panic!("expected update");
        };
        assert_eq!(
            endpoint.addresses,
            vec!["192.168.1.3:7000".parse().unwrap()]
        );
        let removed_a = registry.removed(SERVICE_TYPE, "a._nox._tcp.local.");
        assert!(matches!(removed_a.as_slice(), [DiscoveryEvent::Updated(_)]));
        assert!(matches!(
            registry
                .removed(SERVICE_TYPE, "b._nox._tcp.local.")
                .as_slice(),
            [DiscoveryEvent::Removed(DiscoveryKey::Member(_))]
        ));
    }

    #[test]
    fn registry_caps_new_keys_without_eviction() {
        let address = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2));
        let mut registry = CandidateRegistry::default();
        for value in 0..MAX_DISCOVERED_ENDPOINTS {
            let device = format!("{value:064x}");
            let properties = [("v", "1"), ("m", "s"), ("d", device.as_str())];
            let fullname = format!("{value}._nox._tcp.local.");
            assert_eq!(
                registry
                    .resolved(&service(&fullname, &properties, &[address]))
                    .unwrap()
                    .len(),
                1
            );
        }
        let properties = [("v", "1"), ("m", "s"), ("d", &"ff".repeat(32))];
        assert!(
            registry
                .resolved(&service(
                    "overflow._nox._tcp.local.",
                    &properties,
                    &[address]
                ))
                .unwrap()
                .is_empty()
        );
        assert_eq!(registry.by_key.len(), MAX_DISCOVERED_ENDPOINTS);
    }

    #[test]
    fn service_addresses_are_sorted_and_deduplicated() {
        let addresses = [
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 3)),
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2)),
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 3)),
        ];
        let properties = [("v", "1"), ("m", "p"), ("i", &"00".repeat(16))];
        let endpoint = parse_resolved_service(&service(
            "pair._nox._tcp.local.",
            &properties,
            &addresses,
        ))
        .unwrap();
        let expected: HashSet<_> = [
            "192.168.1.2:7000".parse().unwrap(),
            "192.168.1.3:7000".parse().unwrap(),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            endpoint.addresses.iter().copied().collect::<HashSet<_>>(),
            expected
        );
    }

    #[test]
    fn mocked_daemon_unregisters_once_and_shutdown_preserves_order() {
        struct MockDaemon {
            events: Vec<String>,
        }

        impl MockDaemon {
            fn unregister(&mut self, name: &str) {
                self.events.push(format!("unregister:{name}"));
            }

            fn shutdown(&mut self) {
                self.events.push("shutdown".to_owned());
            }
        }

        let registry = Arc::new(RegistrationRegistry::new());
        let first = Arc::new(Registration {
            fullname: Mutex::new("first._nox._tcp.local.".to_owned()),
            active: AtomicBool::new(true),
        });
        let second = Arc::new(Registration {
            fullname: Mutex::new("second._nox._tcp.local.".to_owned()),
            active: AtomicBool::new(true),
        });
        registry
            .registrations
            .lock()
            .unwrap()
            .extend([Arc::clone(&first), Arc::clone(&second)]);

        let mut daemon = MockDaemon { events: Vec::new() };
        release_registration(&first, &registry, |name| {
            daemon.unregister(name);
        });
        release_registration(&first, &registry, |name| {
            daemon.unregister(name);
        });
        assert_eq!(
            daemon.events.as_slice(),
            ["unregister:first._nox._tcp.local.".to_owned()].as_slice()
        );
        assert!(!first.active.load(Ordering::Acquire));

        let names = take_active_registration_names(&registry).unwrap();
        assert_eq!(names, ["second._nox._tcp.local."]);
        for name in names {
            daemon.unregister(&name);
        }
        daemon.shutdown();
        assert_eq!(
            daemon.events.as_slice(),
            [
                "unregister:first._nox._tcp.local.".to_owned(),
                "unregister:second._nox._tcp.local.".to_owned(),
                "shutdown".to_owned(),
            ]
            .as_slice()
        );
        assert!(registry.registrations.lock().unwrap().is_empty());
    }

    #[test]
    fn pending_event_storm_stays_bounded_and_keeps_latest_value() {
        let key = DiscoveryKey::Pairing([7; 16]);
        let mut pending = VecDeque::new();
        for port in 1..=10_000 {
            enqueue(
                &mut pending,
                Ok(DiscoveryEvent::Updated(DiscoveredEndpoint {
                    key: key.clone(),
                    addresses: vec![SocketAddr::from(([127, 0, 0, 1], port))],
                    protocol_version: 1,
                })),
            );
        }
        assert_eq!(pending.len(), 1);
        let Ok(DiscoveryEvent::Updated(endpoint)) = pending.front().unwrap() else {
            panic!("expected coalesced update");
        };
        assert_eq!(endpoint.addresses[0].port(), 10_000);

        for value in 0..(EVENT_CAPACITY + 32) {
            enqueue(
                &mut pending,
                Ok(DiscoveryEvent::Found(DiscoveredEndpoint {
                    key: DiscoveryKey::Pairing([value as u8; 16]),
                    addresses: vec![SocketAddr::from(([127, 0, 0, 1], 1))],
                    protocol_version: 1,
                })),
            );
        }
        assert_eq!(pending.len(), EVENT_CAPACITY);
    }
}
