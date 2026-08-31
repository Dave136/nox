//! Runtime-neutral lifecycle service for LAN sync.
//!
//! The service deliberately keeps Tokio details private.  Callers submit
//! bounded commands and await a one-shot result; status is best effort.

use crate::{
    discovery::{
        Advertisement, AdvertisementHandle, DiscoveryEvent, DiscoveryKey, DiscoveryService,
    },
    noise::{SessionDirection, preferred_initiator},
    pairing::{PairingOffer, PairingOutcome, PairingStoreHandle, PairingTarget},
    replication::{ReplicationConfig, ReplicationStoreHandle},
    transport::{ConnectionLimiter, TransportLimits},
};
use async_channel::{Receiver, Sender, TrySendError};
use nox_core::{DeviceId, UnlockedSyncAccess, Vault, X25519Keypair};
use std::{
    collections::BTreeMap,
    fmt,
    net::{IpAddr, Ipv6Addr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, SystemTime},
};
use tokio::{
    net::{TcpListener, TcpStream},
    task::JoinSet,
    time::{self, Instant},
};

pub const MAX_BLOCKING_CORE_JOBS: usize = 2;
const COMMAND_CAPACITY: usize = 32;
const EVENT_CAPACITY: usize = 128;
const PAIRING_TICK: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug)]
pub struct SyncConfig {
    pub bind_addr: SocketAddr,
    pub transport: TransportLimits,
    pub replication: ReplicationConfig,
}

impl SyncConfig {
    #[must_use]
    pub fn v1() -> Self {
        Self {
            bind_addr: SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
            transport: TransportLimits::v1(),
            replication: ReplicationConfig::default(),
        }
    }

    fn validate(self) -> Result<(), SyncError> {
        if self.transport.connect_timeout.is_zero()
            || self.transport.handshake_timeout.is_zero()
            || self.transport.read_timeout.is_zero()
            || self.transport.write_timeout.is_zero()
            || self.transport.idle_timeout.is_zero()
            || !(1..=1024).contains(&self.transport.max_connections)
            || self.replication.deadline.is_zero()
            || self.replication.max_rounds == 0
            || self.replication.max_rounds > crate::replication::MAX_ROUNDS_PER_SESSION
        {
            return Err(SyncError::new(
                SyncErrorCode::Internal,
                SyncOperation::Start,
            ));
        }
        Ok(())
    }
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self::v1()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncOperation {
    Start,
    BeginPairing,
    CancelPairing,
    Join,
    SyncNow,
    BlockDevice,
    UnblockDevice,
    Stop,
    Accept,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncErrorCode {
    Busy,
    NotRunning,
    LockedOrUnavailable,
    DiscoveryUnavailable,
    PeerUnavailable,
    AuthenticationFailed,
    PairingFailed,
    ReplicationFailed,
    CapacityReached,
    Timeout,
    Storage,
    Internal,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub struct SyncError {
    pub code: SyncErrorCode,
    pub operation: SyncOperation,
}

impl SyncError {
    const fn new(code: SyncErrorCode, operation: SyncOperation) -> Self {
        Self { code, operation }
    }
}

impl fmt::Debug for SyncError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SyncError")
            .field("code", &self.code)
            .field("operation", &self.operation)
            .finish()
    }
}

impl fmt::Display for SyncError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "sync {:?} failed: {:?}",
            self.operation, self.code
        )
    }
}

impl std::error::Error for SyncError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorScope {
    Service,
    Discovery,
    Pairing,
    Peer(DeviceId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PeerState {
    Discovered,
    Connecting,
    Authenticating,
    Syncing,
    Idle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairingPublicState {
    Available,
    InProgress,
    Completed,
    Cancelled,
    Expired,
    LockedOut,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SyncEvent {
    ServiceReady {
        local_addr: SocketAddr,
    },
    DiscoveryUnavailable,
    PeerState {
        device_id: DeviceId,
        state: PeerState,
    },
    PairingState(PairingPublicState),
    ReplicationFinished {
        device_id: DeviceId,
        converged: bool,
    },
    LocalBlockChanged {
        device_id: DeviceId,
        blocked: bool,
    },
    RecoverableError {
        scope: ErrorScope,
        code: SyncErrorCode,
    },
    Stopped,
}

#[derive(Clone)]
pub struct PairingInvitation {
    pub instance_id: crate::pairing::PairingInstanceId,
    pub display_code: crate::pairing::PairingCodeDisplay,
    pub expires_at: SystemTime,
}

impl fmt::Debug for PairingInvitation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PairingInvitation")
            .field("instance_id", &self.instance_id)
            .field("display_code", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyncSummary {
    pub attempted_peers: usize,
    pub converged_peers: usize,
    pub sent_changes: u64,
    pub received_changes: u64,
}

pub struct JoinedVault {
    pub vault: Vault,
    pub outcome: PairingOutcome,
}

pub struct SyncRequest<T>(Receiver<Result<T, SyncError>>, SyncOperation);

impl<T> SyncRequest<T> {
    pub async fn receive(self) -> Result<T, SyncError> {
        self.0
            .recv()
            .await
            .unwrap_or_else(|_| Err(SyncError::new(SyncErrorCode::NotRunning, self.1)))
    }
}

pub struct SyncEventReceiver(Receiver<SyncEvent>);

impl SyncEventReceiver {
    pub async fn receive(&self) -> Result<SyncEvent, SyncError> {
        self.0
            .recv()
            .await
            .map_err(|_| SyncError::new(SyncErrorCode::NotRunning, SyncOperation::Stop))
    }
}

#[derive(Clone)]
pub struct SyncHandle {
    commands: Sender<InternalCommand>,
    stopped: Arc<AtomicBool>,
}

impl fmt::Debug for SyncHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SyncHandle(<redacted>)")
    }
}

pub struct SyncService {
    commands: Sender<InternalCommand>,
    thread: Option<thread::JoinHandle<Result<(), SyncError>>>,
    stopped: Arc<AtomicBool>,
}

impl fmt::Debug for SyncService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SyncService(<redacted>)")
    }
}

enum InternalCommand {
    BeginPairing(Sender<Result<PairingInvitation, SyncError>>),
    CancelPairing(Sender<Result<(), SyncError>>),
    Join(
        crate::pairing::JoinRequest,
        Sender<Result<JoinedVault, SyncError>>,
    ),
    SyncNow(Sender<Result<SyncSummary, SyncError>>),
    BlockDevice(DeviceId, Sender<Result<(), SyncError>>),
    UnblockDevice(DeviceId, Sender<Result<(), SyncError>>),
    Stop(Sender<Result<(), SyncError>>),
    #[allow(dead_code)]
    Discovery(Result<DiscoveryEvent, crate::discovery::DiscoveryError>),
}

impl SyncHandle {
    fn request<T>(
        &self,
        operation: SyncOperation,
        command: impl FnOnce(Sender<Result<T, SyncError>>) -> InternalCommand,
    ) -> Result<SyncRequest<T>, SyncError> {
        if self.stopped.load(Ordering::Acquire) && operation != SyncOperation::Stop {
            return Err(SyncError::new(SyncErrorCode::NotRunning, operation));
        }
        let (sender, receiver) = async_channel::bounded(1);
        self.commands
            .try_send(command(sender))
            .map_err(|error| match error {
                TrySendError::Full(_) => SyncError::new(SyncErrorCode::CapacityReached, operation),
                TrySendError::Closed(_) => SyncError::new(SyncErrorCode::NotRunning, operation),
            })?;
        Ok(SyncRequest(receiver, operation))
    }

    pub fn begin_pairing(&self) -> Result<SyncRequest<PairingInvitation>, SyncError> {
        self.request(SyncOperation::BeginPairing, InternalCommand::BeginPairing)
    }

    pub fn cancel_pairing(&self) -> Result<SyncRequest<()>, SyncError> {
        self.request(SyncOperation::CancelPairing, InternalCommand::CancelPairing)
    }

    pub fn join(
        &self,
        request: crate::pairing::JoinRequest,
    ) -> Result<SyncRequest<JoinedVault>, SyncError> {
        self.request(SyncOperation::Join, |sender| {
            InternalCommand::Join(request, sender)
        })
    }

    pub fn sync_now(&self) -> Result<SyncRequest<SyncSummary>, SyncError> {
        self.request(SyncOperation::SyncNow, InternalCommand::SyncNow)
    }

    pub fn block_device(&self, device_id: DeviceId) -> Result<SyncRequest<()>, SyncError> {
        self.request(SyncOperation::BlockDevice, |sender| {
            InternalCommand::BlockDevice(device_id, sender)
        })
    }

    pub fn unblock_device(&self, device_id: DeviceId) -> Result<SyncRequest<()>, SyncError> {
        self.request(SyncOperation::UnblockDevice, |sender| {
            InternalCommand::UnblockDevice(device_id, sender)
        })
    }

    pub fn stop(&self) -> Result<SyncRequest<()>, SyncError> {
        if self
            .stopped
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            let (sender, receiver) = async_channel::bounded(1);
            let _ = sender.try_send(Ok(()));
            return Ok(SyncRequest(receiver, SyncOperation::Stop));
        }
        let (sender, receiver) = async_channel::bounded(1);
        match self.commands.try_send(InternalCommand::Stop(sender)) {
            Ok(()) => Ok(SyncRequest(receiver, SyncOperation::Stop)),
            Err(TrySendError::Full(_)) => {
                self.stopped.store(false, Ordering::Release);
                Err(SyncError::new(
                    SyncErrorCode::CapacityReached,
                    SyncOperation::Stop,
                ))
            }
            Err(TrySendError::Closed(_)) => Err(SyncError::new(
                SyncErrorCode::NotRunning,
                SyncOperation::Stop,
            )),
        }
    }
}

impl SyncService {
    pub fn start_unlocked(
        access: UnlockedSyncAccess,
        config: SyncConfig,
    ) -> Result<(Self, SyncEventReceiver), SyncError> {
        Self::start(ServiceMode::Unlocked, Some(access), config)
    }

    pub fn start_joiner(config: SyncConfig) -> Result<(Self, SyncEventReceiver), SyncError> {
        Self::start(ServiceMode::Joiner, None, config)
    }

    pub fn handle(&self) -> SyncHandle {
        SyncHandle {
            commands: self.commands.clone(),
            stopped: Arc::clone(&self.stopped),
        }
    }

    pub fn shutdown(mut self) -> Result<(), SyncError> {
        let stop_result = self.handle().stop();
        if stop_result.is_err() {
            self.commands.close();
        }
        let result = self
            .thread
            .take()
            .ok_or(SyncError::new(
                SyncErrorCode::NotRunning,
                SyncOperation::Stop,
            ))?
            .join()
            .map_err(|_| SyncError::new(SyncErrorCode::Internal, SyncOperation::Stop))?;
        result.and(stop_result.map(|_| ()))
    }

    fn start(
        mode: ServiceMode,
        access: Option<UnlockedSyncAccess>,
        config: SyncConfig,
    ) -> Result<(Self, SyncEventReceiver), SyncError> {
        config.validate()?;
        if let Some(access) = access.as_ref() {
            access.authorization_snapshot().map_err(|_| {
                SyncError::new(SyncErrorCode::LockedOrUnavailable, SyncOperation::Start)
            })?;
        }
        let (pairing_store, replication_store, local_noise, peer_keys) =
            if let Some(access) = access.as_ref() {
                let pairing = access
                    .open_pairing_store()
                    .map(PairingStoreHandle::new)
                    .map_err(|_| SyncError::new(SyncErrorCode::Storage, SyncOperation::Start))?;
                let replication = access
                    .open_replication_store()
                    .map(ReplicationStoreHandle::new)
                    .map_err(|_| SyncError::new(SyncErrorCode::Storage, SyncOperation::Start))?;
                let noise = access.local_noise_keypair();
                let peers = access
                    .authorized_peer_keys()
                    .map_err(|_| SyncError::new(SyncErrorCode::Storage, SyncOperation::Start))?;
                (Some(pairing), Some(replication), Some(noise), peers)
            } else {
                (None, None, None, BTreeMap::new())
            };
        let (commands, command_receiver) = async_channel::bounded(COMMAND_CAPACITY);
        let (events, event_receiver) = async_channel::bounded(EVENT_CAPACITY);
        let (ready_sender, ready_receiver) = std::sync::mpsc::sync_channel(1);
        let thread_commands = commands.clone();
        let command_sender = commands.clone();
        let thread = thread::Builder::new()
            .name("locker-sync-supervisor".to_owned())
            .spawn(move || {
                supervisor(
                    mode,
                    access,
                    pairing_store,
                    replication_store,
                    local_noise,
                    peer_keys,
                    config,
                    command_receiver,
                    command_sender,
                    events,
                    ready_sender,
                )
            })
            .map_err(|_| SyncError::new(SyncErrorCode::Internal, SyncOperation::Start))?;
        let local_addr = match ready_receiver.recv() {
            Ok(Ok(addr)) => addr,
            Ok(Err(error)) => {
                commands.close();
                let _ = thread.join();
                return Err(error);
            }
            Err(_) => {
                commands.close();
                let _ = thread.join();
                return Err(SyncError::new(
                    SyncErrorCode::Internal,
                    SyncOperation::Start,
                ));
            }
        };
        let service = Self {
            commands: thread_commands,
            thread: Some(thread),
            stopped: Arc::new(AtomicBool::new(false)),
        };
        // The actor sends ServiceReady before the startup acknowledgement.
        let _ = local_addr;
        Ok((service, SyncEventReceiver(event_receiver)))
    }
}

impl Drop for SyncService {
    fn drop(&mut self) {
        if self.stopped.swap(true, Ordering::AcqRel) {
            return;
        }
        self.commands.close();
        // Intentionally do not join here: Drop is only a cleanup fallback and
        // must never block a foreground/UI callback.
        let _ = self.thread.take();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ServiceMode {
    Unlocked,
    Joiner,
}

struct ActivePairing {
    offer: PairingOffer,
    advertisement: Option<AdvertisementHandle>,
}

struct PeerSession {
    direction: SessionDirection,
    generation: u64,
    cancel: Sender<bool>,
}

enum TaskCompletion {
    ConnectionClosed {
        peer_addr: SocketAddr,
    },
    DiscoveryEnded,
    PairingFinished,
    PairingDone {
        epoch: u64,
        success: bool,
    },
    SyncDone {
        epoch: u64,
        sender: Sender<Result<SyncSummary, SyncError>>,
        summary: SyncSummary,
    },
}

struct ServiceState {
    epoch: u64,
    mode: ServiceMode,
    access: Option<UnlockedSyncAccess>,
    limiter: ConnectionLimiter,
    discovery: Option<DiscoveryService>,
    presence: Option<AdvertisementHandle>,
    pairing: Option<ActivePairing>,
    pairing_cancel: Option<Sender<bool>>,
    pairing_advertisement: Option<AdvertisementHandle>,
    pairing_task_epoch: Option<u64>,
    peers: BTreeMap<DeviceId, PeerSession>,
    tasks: JoinSet<TaskCompletion>,
    stopping: bool,
    events: Sender<SyncEvent>,
    coalesced: BTreeMap<EventKey, SyncEvent>,
    local_addr: SocketAddr,
    pairing_store: Option<PairingStoreHandle>,
    replication_store: Option<ReplicationStoreHandle>,
    local_noise: Option<X25519Keypair>,
    peer_keys: BTreeMap<DeviceId, [u8; 32]>,
    member_candidates: BTreeMap<DeviceId, Vec<SocketAddr>>,
    pairing_candidates: BTreeMap<crate::pairing::PairingInstanceId, Vec<SocketAddr>>,
    active_cancels: BTreeMap<SocketAddr, Sender<bool>>,
    sync_active: bool,
    sync_cancel: Option<Sender<bool>>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum EventKey {
    Peer(DeviceId),
    Pairing,
}

impl ServiceState {
    fn emit(&mut self, event: SyncEvent) {
        if self.events.try_send(event.clone()).is_err()
            && let Some(key) = event_key(&event)
        {
            self.coalesced.insert(key, event);
        }
    }

    fn flush_events(&mut self) {
        let keys = self.coalesced.keys().copied().collect::<Vec<_>>();
        for key in keys {
            let Some(event) = self.coalesced.get(&key).cloned() else {
                continue;
            };
            match self.events.try_send(event) {
                Ok(()) => {
                    self.coalesced.remove(&key);
                }
                Err(TrySendError::Full(_)) => break,
                Err(TrySendError::Closed(_)) => {
                    self.coalesced.clear();
                    break;
                }
            }
        }
    }

    fn discovery_event(&mut self, event: DiscoveryEvent) {
        match event {
            DiscoveryEvent::Found(endpoint) | DiscoveryEvent::Updated(endpoint) => {
                match endpoint.key {
                    DiscoveryKey::Pairing(instance) => {
                        self.pairing_candidates.insert(
                            crate::pairing::PairingInstanceId::from_bytes(instance),
                            endpoint.addresses,
                        );
                    }
                    DiscoveryKey::Member(device_id) => {
                        let Some(access) = self.access.as_ref() else {
                            return;
                        };
                        if device_id == access.local_device_id()
                            || !self.peer_keys.contains_key(&device_id)
                        {
                            return;
                        }
                        self.member_candidates.insert(device_id, endpoint.addresses);
                        let direction = preferred_initiator(access.local_device_id(), device_id)
                            .map(|preferred| {
                                if preferred == access.local_device_id() {
                                    SessionDirection::LocallyInitiated
                                } else {
                                    SessionDirection::RemotelyInitiated
                                }
                            })
                            .unwrap_or(SessionDirection::RemotelyInitiated);
                        let (cancel, _receiver) = async_channel::bounded(1);
                        let previous = self.peers.insert(
                            device_id,
                            PeerSession {
                                direction,
                                generation: self.epoch,
                                cancel,
                            },
                        );
                        if let Some(previous) = previous {
                            let _ = previous.cancel.try_send(true);
                        }
                        self.emit(SyncEvent::PeerState {
                            device_id,
                            state: PeerState::Discovered,
                        });
                    }
                }
            }
            DiscoveryEvent::Removed(DiscoveryKey::Member(device_id)) => {
                self.member_candidates.remove(&device_id);
                if let Some(peer) = self.peers.remove(&device_id) {
                    let _ = peer.cancel.try_send(true);
                }
            }
            DiscoveryEvent::Removed(DiscoveryKey::Pairing(instance)) => {
                self.pairing_candidates
                    .remove(&crate::pairing::PairingInstanceId::from_bytes(instance));
            }
        }
    }
}

fn event_key(event: &SyncEvent) -> Option<EventKey> {
    match event {
        SyncEvent::PeerState { device_id, .. }
        | SyncEvent::ReplicationFinished { device_id, .. }
        | SyncEvent::LocalBlockChanged { device_id, .. } => Some(EventKey::Peer(*device_id)),
        SyncEvent::PairingState(_) => Some(EventKey::Pairing),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn supervisor(
    mode: ServiceMode,
    access: Option<UnlockedSyncAccess>,
    pairing_store: Option<PairingStoreHandle>,
    replication_store: Option<ReplicationStoreHandle>,
    local_noise: Option<X25519Keypair>,
    peer_keys: BTreeMap<DeviceId, [u8; 32]>,
    config: SyncConfig,
    commands: Receiver<InternalCommand>,
    command_sender: Sender<InternalCommand>,
    events: Sender<SyncEvent>,
    ready: std::sync::mpsc::SyncSender<Result<SocketAddr, SyncError>>,
) -> Result<(), SyncError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_io()
        .enable_time()
        .thread_name("locker-sync-worker")
        .build()
        .map_err(|_| SyncError::new(SyncErrorCode::Internal, SyncOperation::Start))?;
    runtime.block_on(run_actor(
        mode,
        access,
        pairing_store,
        replication_store,
        local_noise,
        peer_keys,
        config,
        commands,
        command_sender,
        events,
        ready,
    ))
}

#[allow(clippy::too_many_arguments)]
async fn run_actor(
    mode: ServiceMode,
    access: Option<UnlockedSyncAccess>,
    pairing_store: Option<PairingStoreHandle>,
    replication_store: Option<ReplicationStoreHandle>,
    local_noise: Option<X25519Keypair>,
    peer_keys: BTreeMap<DeviceId, [u8; 32]>,
    config: SyncConfig,
    commands: Receiver<InternalCommand>,
    command_sender: Sender<InternalCommand>,
    events: Sender<SyncEvent>,
    ready: std::sync::mpsc::SyncSender<Result<SocketAddr, SyncError>>,
) -> Result<(), SyncError> {
    let listener = match TcpListener::bind(config.bind_addr).await {
        Ok(listener) => listener,
        Err(_) => {
            let error = SyncError::new(SyncErrorCode::Internal, SyncOperation::Start);
            let _ = ready.send(Err(error));
            return Err(error);
        }
    };
    let local_addr = listener
        .local_addr()
        .map_err(|_| SyncError::new(SyncErrorCode::Internal, SyncOperation::Start))?;
    let limiter = ConnectionLimiter::new(config.transport.max_connections)
        .map_err(|_| SyncError::new(SyncErrorCode::Internal, SyncOperation::Start))?;
    let mut state = ServiceState {
        epoch: 1,
        mode,
        access,
        limiter,
        discovery: None,
        presence: None,
        pairing: None,
        pairing_cancel: None,
        pairing_advertisement: None,
        pairing_task_epoch: None,
        peers: BTreeMap::new(),
        tasks: JoinSet::new(),
        stopping: false,
        events,
        coalesced: BTreeMap::new(),
        local_addr,
        pairing_store,
        replication_store,
        local_noise,
        peer_keys,
        member_candidates: BTreeMap::new(),
        pairing_candidates: BTreeMap::new(),
        active_cancels: BTreeMap::new(),
        sync_active: false,
        sync_cancel: None,
    };
    start_discovery(&mut state)?;
    if let Some(discovery) = state.discovery.as_ref() {
        match discovery.browse() {
            Ok(mut stream) => {
                state.tasks.spawn(async move {
                    loop {
                        let event = stream.next().await;
                        if command_sender
                            .send(InternalCommand::Discovery(event))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    TaskCompletion::DiscoveryEnded
                });
            }
            Err(_) => state.emit(SyncEvent::DiscoveryUnavailable),
        }
    }
    if let (ServiceMode::Unlocked, Some(access)) = (state.mode, state.access.as_ref())
        && let Some(discovery) = state.discovery.as_ref()
    {
        match discovery.advertise(Advertisement::MemberPresence {
            device_id: access.local_device_id(),
            port: local_addr.port(),
        }) {
            Ok(handle) => state.presence = Some(handle),
            Err(_) => state.emit(SyncEvent::DiscoveryUnavailable),
        }
    }
    state.emit(SyncEvent::ServiceReady { local_addr });
    let _ = ready.send(Ok(local_addr));
    let mut pairing_tick = time::interval(PAIRING_TICK);
    let mut listener = Some(listener);
    loop {
        state.flush_events();
        if state.stopping {
            break;
        }
        tokio::select! {
            command = commands.recv() => {
                match command {
                    Ok(command) => handle_command(&mut state, &mut listener, command, config).await,
                    Err(_) => {
                        stop_state(&mut state, &mut listener).await;
                    }
                }
            }
            accepted = async {
                listener.as_ref().expect("listener exists while running").accept().await
            }, if listener.is_some() => {
                match accepted {
                    Ok((stream, peer_addr)) => accept_connection(&mut state, stream, peer_addr, config).await,
                    Err(_) => state.emit(SyncEvent::RecoverableError { scope: ErrorScope::Service, code: SyncErrorCode::Internal }),
                }
            }
            _ = pairing_tick.tick() => expire_pairing(&mut state),
            result = state.tasks.join_next(), if !state.tasks.is_empty() => {
                match result {
                    Some(Ok(completion)) => match completion {
                        TaskCompletion::ConnectionClosed { peer_addr } => {
                            state.active_cancels.remove(&peer_addr);
                        }
                        TaskCompletion::DiscoveryEnded | TaskCompletion::PairingFinished => {}
                        TaskCompletion::PairingDone { epoch, success } => {
                            let current = state.pairing_task_epoch == Some(epoch);
                            if current {
                                state.pairing_task_epoch = None;
                                state.pairing_cancel.take();
                                state.pairing_advertisement.take();
                            }
                            if current && epoch == state.epoch && !state.stopping {
                                state.emit(SyncEvent::PairingState(if success {
                                    PairingPublicState::Completed
                                } else {
                                    PairingPublicState::Cancelled
                                }));
                            }
                        }
                        TaskCompletion::SyncDone {
                            epoch,
                            sender,
                            summary,
                        } => {
                            state.sync_active = false;
                            state.sync_cancel.take();
                            if epoch == state.epoch && !state.stopping {
                                let _ = sender.try_send(Ok(summary));
                            } else {
                                let _ = sender.try_send(Err(SyncError::new(
                                    SyncErrorCode::NotRunning,
                                    SyncOperation::SyncNow,
                                )));
                            }
                        }
                    },
                    Some(Err(_)) => {
                        state.sync_active = false;
                        state.emit(SyncEvent::RecoverableError {
                            scope: ErrorScope::Service,
                            code: SyncErrorCode::Internal,
                        });
                    }
                    None => {}
                }
            }
        }
    }
    stop_state(&mut state, &mut listener).await;
    state.emit(SyncEvent::Stopped);
    state.flush_events();
    Ok(())
}

fn start_discovery(state: &mut ServiceState) -> Result<(), SyncError> {
    match DiscoveryService::start() {
        Ok(discovery) => state.discovery = Some(discovery),
        Err(_) => state.emit(SyncEvent::DiscoveryUnavailable),
    }
    Ok(())
}

async fn accept_connection(
    state: &mut ServiceState,
    stream: TcpStream,
    peer_addr: SocketAddr,
    config: SyncConfig,
) {
    let permit = match state.limiter.try_acquire() {
        Ok(permit) => permit,
        Err(_) => {
            state.emit(SyncEvent::RecoverableError {
                scope: ErrorScope::Service,
                code: SyncErrorCode::CapacityReached,
            });
            return;
        }
    };
    let mut io = match crate::transport::FramedIo::new(stream, config.transport) {
        Ok(io) => io,
        Err(_) => return,
    };
    let first = match time::timeout(config.transport.handshake_timeout, io.read_frame()).await {
        Ok(Ok(frame)) => frame,
        _ => {
            spawn_close(state, io, permit, peer_addr);
            return;
        }
    };
    match first.class {
        crate::frame::FrameClass::Pairing => {}
        crate::frame::FrameClass::NoiseHandshake => {}
        crate::frame::FrameClass::NoiseTransport => {
            spawn_close(state, io, permit, peer_addr);
            return;
        }
    }
    if first.class == crate::frame::FrameClass::Pairing
        && let (Some(active), Some(store)) = (state.pairing.take(), state.pairing_store.clone())
    {
        let epoch = state.epoch;
        let (cancel, cancel_receiver) = async_channel::bounded(1);
        let ActivePairing {
            offer,
            advertisement,
        } = active;
        state.pairing_cancel = Some(cancel);
        state.pairing_advertisement = advertisement;
        state.pairing_task_epoch = Some(epoch);
        io.pushback_frame(first);
        state.emit(SyncEvent::PairingState(PairingPublicState::InProgress));
        state.tasks.spawn(async move {
            let _permit = permit;
            let mut offer = offer;
            let result = tokio::select! {
                _ = cancel_receiver.recv() => false,
                result = crate::pairing::run_inviter(io, &mut offer, store, Instant::now) => result.is_ok(),
            };
            TaskCompletion::PairingDone {
                epoch,
                success: result,
            }
        });
        return;
    }
    if first.class != crate::frame::FrameClass::NoiseHandshake {
        spawn_close(state, io, permit, peer_addr);
        return;
    }
    let known_remote_id = state
        .member_candidates
        .iter()
        .find(|(_, addresses)| addresses.contains(&peer_addr))
        .map(|(&device_id, _)| device_id);
    let candidates = known_remote_id
        .and_then(|device_id| {
            state
                .peer_keys
                .get(&device_id)
                .copied()
                .map(|key| vec![(device_id, key)])
        })
        .unwrap_or_else(|| {
            state
                .peer_keys
                .iter()
                .map(|(&id, &key)| (id, key))
                .collect()
        });
    if !candidates.is_empty()
        && let (Some(local_noise), Some(store), Some(access)) = (
            state.local_noise.as_ref(),
            state.replication_store.clone(),
            state.access.as_ref(),
        )
    {
        let local_noise = clone_noise(local_noise);
        let local_id = access.local_device_id();
        let vault_id = access.vault_id();
        let handshake_deadline = Instant::now() + config.transport.handshake_timeout;
        let replication_config = config.replication;
        let (cancel, cancel_receiver) = async_channel::bounded(1);
        state.active_cancels.insert(peer_addr, cancel.clone());
        if let Some(remote_id) = known_remote_id
            && let Some(peer) = state.peers.get_mut(&remote_id)
        {
            let previous = std::mem::replace(&mut peer.cancel, cancel.clone());
            let _ = previous.try_send(true);
        }
        io.pushback_frame(first);
        state.tasks.spawn(async move {
            let _permit = permit;
            let result = tokio::select! {
                _ = cancel_receiver.recv() => Err(()),
                result = async {
                    let first = io.read_frame().await.map_err(|_| ())?;
                    let (mut connection, _) = crate::noise::handshake_responder_candidates(
                        io,
                        first,
                        crate::frame::SYNC_PROTOCOL_VERSION,
                        vault_id,
                        local_id,
                        &local_noise,
                        candidates,
                        handshake_deadline,
                    )
                        .await
                        .map_err(|_| ())?;
                    crate::replication::replicate(&mut connection, store, replication_config)
                        .await
                        .map_err(|_| ())
                } => result,
            };
            let _ = result;
            TaskCompletion::ConnectionClosed { peer_addr }
        });
        return;
    }
    spawn_close(state, io, permit, peer_addr);
}

fn spawn_close(
    state: &mut ServiceState,
    mut io: crate::transport::FramedIo<TcpStream>,
    permit: crate::transport::ConnectionPermit,
    peer_addr: SocketAddr,
) {
    state.tasks.spawn(async move {
        let _permit = permit;
        let _ = io.shutdown().await;
        TaskCompletion::ConnectionClosed { peer_addr }
    });
}

async fn handle_command(
    state: &mut ServiceState,
    listener: &mut Option<TcpListener>,
    command: InternalCommand,
    _config: SyncConfig,
) {
    match command {
        InternalCommand::BeginPairing(sender) => {
            let result = begin_pairing(state);
            let _ = sender.try_send(result);
        }
        InternalCommand::CancelPairing(sender) => {
            let result = cancel_pairing(state);
            let _ = sender.try_send(result);
        }
        InternalCommand::Join(request, sender) => {
            join_command(state, request, sender, _config).await
        }
        InternalCommand::SyncNow(sender) => {
            sync_now_command(state, sender, _config).await;
        }
        InternalCommand::BlockDevice(device_id, sender) => {
            let result = block_device(state, device_id).await;
            let _ = sender.try_send(result);
        }
        InternalCommand::UnblockDevice(device_id, sender) => {
            let result = unblock_device(state, device_id).await;
            let _ = sender.try_send(result);
        }
        InternalCommand::Discovery(Ok(event)) => state.discovery_event(event),
        InternalCommand::Discovery(Err(_)) => state.emit(SyncEvent::DiscoveryUnavailable),
        InternalCommand::Stop(sender) => {
            stop_state(state, listener).await;
            let _ = sender.try_send(Ok(()));
        }
    }
}

fn begin_pairing(state: &mut ServiceState) -> Result<PairingInvitation, SyncError> {
    if state.mode != ServiceMode::Unlocked {
        return Err(SyncError::new(
            SyncErrorCode::LockedOrUnavailable,
            SyncOperation::BeginPairing,
        ));
    }
    if state.pairing.is_some() || state.pairing_cancel.is_some() {
        return Err(SyncError::new(
            SyncErrorCode::Busy,
            SyncOperation::BeginPairing,
        ));
    }
    if state.pairing_store.is_none() {
        return Err(SyncError::new(
            SyncErrorCode::LockedOrUnavailable,
            SyncOperation::BeginPairing,
        ));
    }
    let offer = crate::pairing::create_offer(Instant::now())
        .map_err(|_| SyncError::new(SyncErrorCode::PairingFailed, SyncOperation::BeginPairing))?;
    let invitation = PairingInvitation {
        instance_id: offer.instance_id,
        display_code: offer.display_code.clone(),
        expires_at: SystemTime::now()
            .checked_add(offer.expires_at().saturating_duration_since(Instant::now()))
            .unwrap_or(SystemTime::UNIX_EPOCH),
    };
    let advertisement = state.discovery.as_ref().and_then(|discovery| {
        discovery
            .advertise(Advertisement::PairingInstance {
                instance_id: *offer.instance_id.as_bytes(),
                port: state.local_addr.port(),
            })
            .ok()
    });
    state.pairing = Some(ActivePairing {
        offer,
        advertisement,
    });
    state.epoch = state.epoch.wrapping_add(1);
    state.emit(SyncEvent::PairingState(PairingPublicState::Available));
    Ok(invitation)
}

async fn join_command(
    state: &mut ServiceState,
    request: crate::pairing::JoinRequest,
    sender: Sender<Result<JoinedVault, SyncError>>,
    config: SyncConfig,
) {
    if state.mode != ServiceMode::Joiner {
        let _ = sender.try_send(Err(SyncError::new(
            SyncErrorCode::LockedOrUnavailable,
            SyncOperation::Join,
        )));
        return;
    }
    let matches = state
        .pairing_candidates
        .iter()
        .filter(|(instance, _)| crate::pairing::locator_for(**instance) == *request.code.locator())
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        let code = if matches.is_empty() {
            SyncErrorCode::PeerUnavailable
        } else {
            SyncErrorCode::Busy
        };
        let _ = sender.try_send(Err(SyncError::new(code, SyncOperation::Join)));
        return;
    }
    let (&instance_id, addresses) = matches[0];
    let Some(&address) = addresses.first() else {
        let _ = sender.try_send(Err(SyncError::new(
            SyncErrorCode::PeerUnavailable,
            SyncOperation::Join,
        )));
        return;
    };
    let target = PairingTarget {
        instance_id,
        inviter_endpoint: address,
    };
    let permit = match state.limiter.try_acquire() {
        Ok(permit) => permit,
        Err(_) => {
            let _ = sender.try_send(Err(SyncError::new(
                SyncErrorCode::CapacityReached,
                SyncOperation::Join,
            )));
            return;
        }
    };
    state.tasks.spawn(async move {
        let _permit = permit;
        let result = async {
            let io = crate::transport::connect(address, config.transport)
                .await
                .map_err(|_| SyncError::new(SyncErrorCode::PeerUnavailable, SyncOperation::Join))?;
            crate::pairing::run_joiner(io, target, request, Instant::now)
                .await
                .map(|(vault, outcome)| JoinedVault { vault, outcome })
                .map_err(|_| SyncError::new(SyncErrorCode::PairingFailed, SyncOperation::Join))
        }
        .await;
        let _ = sender.try_send(result);
        TaskCompletion::PairingFinished
    });
}

async fn sync_now_command(
    state: &mut ServiceState,
    sender: Sender<Result<SyncSummary, SyncError>>,
    config: SyncConfig,
) {
    if state.mode != ServiceMode::Unlocked {
        let _ = sender.try_send(Err(SyncError::new(
            SyncErrorCode::LockedOrUnavailable,
            SyncOperation::SyncNow,
        )));
        return;
    }
    if state.sync_active {
        let _ = sender.try_send(Err(SyncError::new(
            SyncErrorCode::Busy,
            SyncOperation::SyncNow,
        )));
        return;
    }
    let Some(access) = state.access.as_ref() else {
        let _ = sender.try_send(Err(SyncError::new(
            SyncErrorCode::LockedOrUnavailable,
            SyncOperation::SyncNow,
        )));
        return;
    };
    let Some(local_noise) = state.local_noise.take() else {
        let _ = sender.try_send(Err(SyncError::new(
            SyncErrorCode::LockedOrUnavailable,
            SyncOperation::SyncNow,
        )));
        return;
    };
    let Some(replication_store) = state.replication_store.clone() else {
        let _ = sender.try_send(Err(SyncError::new(
            SyncErrorCode::LockedOrUnavailable,
            SyncOperation::SyncNow,
        )));
        state.local_noise = Some(local_noise);
        return;
    };
    let local_id = access.local_device_id();
    let vault_id = access.vault_id();
    let epoch = state.epoch;
    let limiter = state.limiter.clone();
    let mut peers = Vec::new();
    for (&device_id, peer) in &state.peers {
        if peer.direction != SessionDirection::LocallyInitiated || peer.generation != epoch {
            continue;
        }
        let Some(addresses) = state.member_candidates.get(&device_id) else {
            continue;
        };
        if let Some(&remote_static_key) = state.peer_keys.get(&device_id)
            && let Some(&address) = addresses.first()
        {
            peers.push((device_id, address, remote_static_key));
        }
    }
    state.local_noise = Some(clone_noise(&local_noise));
    if peers.is_empty() {
        let _ = sender.try_send(Ok(SyncSummary {
            attempted_peers: 0,
            converged_peers: 0,
            sent_changes: 0,
            received_changes: 0,
        }));
        return;
    }
    state.sync_active = true;
    let (sync_cancel, sync_cancel_receiver) = async_channel::bounded(1);
    state.sync_cancel = Some(sync_cancel);
    state.tasks.spawn(async move {
        let summary = tokio::select! {
            _ = sync_cancel_receiver.recv() => SyncSummary {
                attempted_peers: 0,
                converged_peers: 0,
                sent_changes: 0,
                received_changes: 0,
            },
            summary = async {
                let mut summary = SyncSummary {
                    attempted_peers: peers.len(),
                    converged_peers: 0,
                    sent_changes: 0,
                    received_changes: 0,
                };
                for (device_id, address, remote_static_key) in peers {
                    let Ok(permit) = limiter.try_acquire() else {
                        continue;
                    };
                    let result = sync_peer(
                        address,
                        config,
                        vault_id,
                        local_id,
                        device_id,
                        clone_noise(&local_noise),
                        remote_static_key,
                        replication_store.clone(),
                    )
                    .await;
                    drop(permit);
                    if let Ok(report) = result {
                        if report.converged {
                            summary.converged_peers += 1;
                        }
                        summary.sent_changes = summary.sent_changes.saturating_add(report.sent_changes);
                        summary.received_changes = summary
                            .received_changes
                            .saturating_add(report.received_changes);
                    }
                }
                summary
            } => summary,
        };
        TaskCompletion::SyncDone {
            epoch,
            sender,
            summary,
        }
    });
}

#[allow(clippy::too_many_arguments)]
async fn sync_peer(
    address: SocketAddr,
    config: SyncConfig,
    vault_id: nox_core::VaultId,
    local_id: DeviceId,
    remote_id: DeviceId,
    local_noise: X25519Keypair,
    remote_static_key: [u8; 32],
    store: ReplicationStoreHandle,
) -> Result<crate::replication::ReplicationReport, SyncError> {
    let io = crate::transport::connect(address, config.transport)
        .await
        .map_err(|_| SyncError::new(SyncErrorCode::PeerUnavailable, SyncOperation::SyncNow))?;
    let noise_config = crate::noise::NoiseConfig {
        role: crate::noise::NoiseRole::Initiator,
        protocol_version: crate::frame::SYNC_PROTOCOL_VERSION,
        vault_id,
        local_device_id: local_id,
        remote_device_id: remote_id,
        local_static_keypair: &local_noise,
        remote_static_public_key: remote_static_key,
    };
    let mut connection = crate::noise::handshake(
        io,
        noise_config,
        Instant::now() + config.transport.handshake_timeout,
    )
    .await
    .map_err(|_| SyncError::new(SyncErrorCode::AuthenticationFailed, SyncOperation::SyncNow))?;
    crate::replication::replicate(&mut connection, store, config.replication)
        .await
        .map_err(|_| SyncError::new(SyncErrorCode::ReplicationFailed, SyncOperation::SyncNow))
}

fn clone_noise(keypair: &X25519Keypair) -> X25519Keypair {
    X25519Keypair::from_private_bytes(keypair.private_key_bytes())
}

fn cancel_pairing(state: &mut ServiceState) -> Result<(), SyncError> {
    if let Some(mut pairing) = state.pairing.take() {
        crate::pairing::cancel_offer(&mut pairing.offer);
        drop(pairing.advertisement);
    } else if let Some(cancel) = state.pairing_cancel.take() {
        let _ = cancel.try_send(true);
        state.pairing_advertisement.take();
        state.pairing_task_epoch = None;
    } else {
        return Err(SyncError::new(
            SyncErrorCode::LockedOrUnavailable,
            SyncOperation::CancelPairing,
        ));
    }
    state.epoch = state.epoch.wrapping_add(1);
    state.emit(SyncEvent::PairingState(PairingPublicState::Cancelled));
    Ok(())
}

fn expire_pairing(state: &mut ServiceState) {
    let expired = state
        .pairing
        .as_ref()
        .is_some_and(|pairing| Instant::now() >= pairing.offer.expires_at());
    if !expired {
        return;
    }
    if let Some(mut pairing) = state.pairing.take() {
        crate::pairing::cancel_offer(&mut pairing.offer);
        drop(pairing.advertisement);
        state.epoch = state.epoch.wrapping_add(1);
        state.emit(SyncEvent::PairingState(PairingPublicState::Expired));
    }
}

async fn block_device(state: &mut ServiceState, device_id: DeviceId) -> Result<(), SyncError> {
    let Some(access) = state.access.as_ref() else {
        return Err(SyncError::new(
            SyncErrorCode::LockedOrUnavailable,
            SyncOperation::BlockDevice,
        ));
    };
    let job = access.clone_for_blocking_job();
    crate::spawn_core_blocking(move || job.block_device(device_id))
        .await
        .map_err(|_| SyncError::new(SyncErrorCode::Internal, SyncOperation::BlockDevice))?
        .map_err(|_| SyncError::new(SyncErrorCode::Storage, SyncOperation::BlockDevice))?;
    state.epoch = state.epoch.wrapping_add(1);
    if let Some(peer) = state.peers.remove(&device_id) {
        let _ = peer.cancel.try_send(true);
    }
    if let Some(cancel) = state.sync_cancel.as_ref() {
        let _ = cancel.try_send(true);
    }
    for cancel in state.active_cancels.values() {
        let _ = cancel.try_send(true);
    }
    state.active_cancels.clear();
    state.member_candidates.remove(&device_id);
    state.peer_keys.remove(&device_id);
    state.emit(SyncEvent::LocalBlockChanged {
        device_id,
        blocked: true,
    });
    Ok(())
}

async fn unblock_device(state: &mut ServiceState, device_id: DeviceId) -> Result<(), SyncError> {
    let Some(access) = state.access.as_ref() else {
        return Err(SyncError::new(
            SyncErrorCode::LockedOrUnavailable,
            SyncOperation::UnblockDevice,
        ));
    };
    let job = access.clone_for_blocking_job();
    crate::spawn_core_blocking(move || job.unblock_device(device_id))
        .await
        .map_err(|_| SyncError::new(SyncErrorCode::Internal, SyncOperation::UnblockDevice))?
        .map_err(|_| SyncError::new(SyncErrorCode::Storage, SyncOperation::UnblockDevice))?;
    state.epoch = state.epoch.wrapping_add(1);
    let peers_job = access.clone_for_blocking_job();
    if let Ok(Ok(peers)) =
        crate::spawn_core_blocking(move || peers_job.authorized_peer_keys()).await
        && let Some(key) = peers.get(&device_id)
    {
        state.peer_keys.insert(device_id, *key);
    }
    state.emit(SyncEvent::LocalBlockChanged {
        device_id,
        blocked: false,
    });
    Ok(())
}

async fn stop_state(state: &mut ServiceState, listener: &mut Option<TcpListener>) {
    if state.stopping {
        return;
    }
    state.stopping = true;
    state.epoch = state.epoch.wrapping_add(1);
    listener.take();
    if let Some(mut pairing) = state.pairing.take() {
        crate::pairing::cancel_offer(&mut pairing.offer);
        drop(pairing.advertisement);
    }
    if let Some(cancel) = state.pairing_cancel.take() {
        let _ = cancel.try_send(true);
    }
    state.pairing_advertisement.take();
    state.pairing_task_epoch = None;
    if let Some(cancel) = state.sync_cancel.take() {
        let _ = cancel.try_send(true);
    }
    for cancel in state.active_cancels.values() {
        let _ = cancel.try_send(true);
    }
    state.active_cancels.clear();
    state.presence.take();
    state.peers.clear();
    state.tasks.abort_all();
    while state.tasks.join_next().await.is_some() {}
    if let Some(discovery) = state.discovery.take() {
        let _ = crate::spawn_core_blocking(move || discovery.shutdown()).await;
    }
    state.access.take();
}

#[cfg(test)]
mod tests {
    use super::*;
    use nox_core::{ItemPayload, ItemType, SecretBytes, Vault};
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };
    use tokio::io::duplex;

    fn test_config() -> SyncConfig {
        SyncConfig {
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            transport: TransportLimits::v1(),
            replication: ReplicationConfig {
                deadline: Duration::from_secs(10),
                max_rounds: 8,
            },
        }
    }

    async fn pair_profile(inviter: &Vault, destination: PathBuf, display_name: &str) -> Vault {
        let store = PairingStoreHandle::new(inviter.open_pairing_store().unwrap());
        let mut offer = crate::pairing::create_offer(Instant::now()).unwrap();
        let target = PairingTarget {
            instance_id: offer.instance_id,
            inviter_endpoint: "127.0.0.1:1".parse().unwrap(),
        };
        let request = crate::pairing::JoinRequest {
            code: offer.display_code.as_str().parse().unwrap(),
            destination,
            master_password: SecretBytes::new(b"joiner-password"),
            display_name: display_name.to_owned(),
        };
        let (left, right) = duplex(256 * 1024);
        let left = crate::transport::FramedIo::new(left, TransportLimits::v1()).unwrap();
        let right = crate::transport::FramedIo::new(right, TransportLimits::v1()).unwrap();
        let inviter_task = crate::pairing::run_inviter(left, &mut offer, store, Instant::now);
        let joiner_task = crate::pairing::run_joiner(right, target, request, Instant::now);
        let (_, joined) = tokio::join!(inviter_task, joiner_task);
        joined.unwrap().0
    }

    async fn ready_address(receiver: &SyncEventReceiver) -> SocketAddr {
        loop {
            if let SyncEvent::ServiceReady { local_addr } = receiver.receive().await.unwrap() {
                return local_addr;
            }
        }
    }

    fn inject_member(service: &SyncService, device_id: DeviceId, address: SocketAddr) {
        service
            .commands
            .try_send(InternalCommand::Discovery(Ok(DiscoveryEvent::Found(
                crate::discovery::DiscoveredEndpoint {
                    key: DiscoveryKey::Member(device_id),
                    addresses: vec![address],
                    protocol_version: crate::frame::SYNC_PROTOCOL_VERSION,
                },
            ))))
            .unwrap();
    }

    async fn sync_round(service: &SyncService) -> SyncSummary {
        service
            .handle()
            .sync_now()
            .unwrap()
            .receive()
            .await
            .unwrap()
    }

    fn payload(title: &str, password: &str) -> ItemPayload {
        ItemPayload {
            schema_version: nox_core::ITEM_SCHEMA_VERSION,
            item_type: ItemType::Login,
            title: title.to_owned(),
            username: "user".to_owned(),
            password: password.to_owned(),
            uris: vec!["https://example.test".to_owned()],
            notes: String::new(),
            created_at: 1,
            updated_at: 1,
        }
    }

    #[test]
    fn public_debug_values_redact_codes_and_errors() {
        let invitation = PairingInvitation {
            instance_id: crate::pairing::PairingInstanceId::from_bytes([1; 16]),
            display_code: crate::pairing::create_offer(Instant::now())
                .unwrap()
                .display_code,
            expires_at: UNIX_EPOCH,
        };
        assert!(!format!("{invitation:?}").contains(invitation.display_code.as_str()));
        assert!(
            !format!(
                "{:?}",
                SyncError::new(SyncErrorCode::Storage, SyncOperation::Start)
            )
            .contains("password")
        );
    }

    #[test]
    fn stop_retries_after_a_full_command_queue() {
        let (commands, receiver) = async_channel::bounded(1);
        let (result_sender, _result_receiver) = async_channel::bounded(1);
        commands
            .try_send(InternalCommand::BeginPairing(result_sender))
            .unwrap();
        let stopped = Arc::new(AtomicBool::new(false));
        let handle = SyncHandle {
            commands,
            stopped: Arc::clone(&stopped),
        };
        let error = match handle.stop() {
            Ok(_) => panic!("stop unexpectedly enqueued"),
            Err(error) => error,
        };
        assert_eq!(error.code, SyncErrorCode::CapacityReached);
        assert!(!stopped.load(Ordering::Acquire));
        drop(receiver);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn joiner_starts_ready_and_rejects_unavailable_commands() {
        let (service, events) = SyncService::start_joiner(test_config()).unwrap();
        let ready = events.receive().await.unwrap();
        assert!(matches!(ready, SyncEvent::ServiceReady { .. }));
        let request = service.handle().begin_pairing().unwrap();
        assert_eq!(
            request.receive().await.unwrap_err().code,
            SyncErrorCode::LockedOrUnavailable
        );
        service.shutdown().unwrap();
    }

    #[test]
    fn unlocked_startup_and_shutdown_release_the_listener() {
        let directory = std::env::temp_dir().join(format!("locker-sync-g-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        let vault = Vault::create(b"password", directory.join("vault.db")).unwrap();
        let access = vault.open_sync_access().unwrap();
        let (service, _events) = SyncService::start_unlocked(access, test_config()).unwrap();
        service.shutdown().unwrap();
        vault.lock();
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pairing_is_single_use_and_cancel_is_idempotent_for_state() {
        let directory =
            std::env::temp_dir().join(format!("locker-sync-g-pair-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        let vault = Vault::create(b"password", directory.join("vault.db")).unwrap();
        let access = vault.open_sync_access().unwrap();
        let (service, events) = SyncService::start_unlocked(access, test_config()).unwrap();
        while !matches!(
            events.receive().await.unwrap(),
            SyncEvent::ServiceReady { .. }
        ) {}
        let invitation = service
            .handle()
            .begin_pairing()
            .unwrap()
            .receive()
            .await
            .unwrap();
        assert!(!invitation.display_code.as_str().is_empty());
        assert_eq!(
            service
                .handle()
                .begin_pairing()
                .unwrap()
                .receive()
                .await
                .unwrap_err()
                .code,
            SyncErrorCode::Busy
        );
        service
            .handle()
            .cancel_pairing()
            .unwrap()
            .receive()
            .await
            .unwrap();
        service.shutdown().unwrap();
        vault.lock();
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn candidate_responder_handshake_round_trips() {
        let left_key = nox_core::X25519Keypair::generate().unwrap();
        let right_key = nox_core::X25519Keypair::generate().unwrap();
        let left_key_for_responder =
            nox_core::X25519Keypair::from_private_bytes(left_key.private_key_bytes());
        let right_key_for_responder =
            nox_core::X25519Keypair::from_private_bytes(right_key.private_key_bytes());
        let left_id = DeviceId::from_public_key([7; 32]);
        let right_id = DeviceId::from_public_key([8; 32]);
        let vault_id = nox_core::VaultId::from_bytes([9; 16]);
        let (left, right) = duplex(4096);
        let left = crate::transport::FramedIo::new(left, TransportLimits::v1()).unwrap();
        let right = crate::transport::FramedIo::new(right, TransportLimits::v1()).unwrap();
        let initiator = async move {
            crate::noise::handshake(
                left,
                crate::noise::NoiseConfig {
                    role: crate::noise::NoiseRole::Initiator,
                    protocol_version: crate::frame::SYNC_PROTOCOL_VERSION,
                    vault_id,
                    local_device_id: left_id,
                    remote_device_id: right_id,
                    local_static_keypair: &left_key,
                    remote_static_public_key: right_key.public_key(),
                },
                Instant::now() + Duration::from_secs(1),
            )
            .await
        };
        let responder = async move {
            let mut right = right;
            let first = right.read_frame().await.unwrap();
            crate::noise::handshake_responder_candidates(
                right,
                first,
                crate::frame::SYNC_PROTOCOL_VERSION,
                vault_id,
                right_id,
                &right_key_for_responder,
                vec![(left_id, left_key_for_responder.public_key())],
                Instant::now() + Duration::from_secs(1),
            )
            .await
        };
        let (initiator, responder) = tokio::join!(initiator, responder);
        assert!(initiator.is_ok(), "initiator={initiator:?}");
        assert!(responder.is_ok(), "responder={responder:?}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn deterministic_three_profile_loopback_converges_and_honors_blocking() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("locker-sync-g-three-{unique}"));
        fs::create_dir_all(&directory).unwrap();
        let mut profile_a = Vault::create(b"password-a", directory.join("a.db")).unwrap();
        let mut profile_b = pair_profile(&profile_a, directory.join("b.db"), "Profile B").await;
        let mut profile_c = pair_profile(&profile_a, directory.join("c.db"), "Profile C").await;

        let (service_a, events_a) =
            SyncService::start_unlocked(profile_a.open_sync_access().unwrap(), test_config())
                .unwrap();
        let (service_b, events_b) =
            SyncService::start_unlocked(profile_b.open_sync_access().unwrap(), test_config())
                .unwrap();
        let (service_c, events_c) =
            SyncService::start_unlocked(profile_c.open_sync_access().unwrap(), test_config())
                .unwrap();
        let address_a = ready_address(&events_a).await;
        let address_b = ready_address(&events_b).await;
        let address_c = ready_address(&events_c).await;

        inject_member(&service_a, profile_b.device_id(), address_b);
        inject_member(&service_a, profile_c.device_id(), address_c);
        inject_member(&service_b, profile_a.device_id(), address_a);
        inject_member(&service_b, profile_c.device_id(), address_c);
        inject_member(&service_c, profile_a.device_id(), address_a);
        inject_member(&service_c, profile_b.device_id(), address_b);

        let item_a = profile_a
            .create_item(&payload("from-a", "a-secret"))
            .unwrap();
        let item_b = profile_b
            .create_item(&payload("from-b", "b-secret"))
            .unwrap();
        let item_c = profile_c
            .create_item(&payload("from-c", "c-secret"))
            .unwrap();
        for _ in 0..3 {
            let _ = sync_round(&service_a).await;
            let _ = sync_round(&service_b).await;
            let _ = sync_round(&service_c).await;
        }
        let mut all_items_present = false;
        // 480 x 25ms = 12s, deliberately past `test_config`'s 10s replication
        // deadline: giving up before the transport itself does turns CPU
        // contention into a test failure. Three profiles converging here means
        // Noise handshakes plus Argon2 derivation, so under a saturated
        // `cargo test --workspace` the old 1s budget expired while the run was
        // merely slow, not stuck. The loop breaks the moment it converges, so
        // a larger budget costs nothing when things are healthy.
        for _ in 0..480 {
            all_items_present = [&profile_a, &profile_b, &profile_c].iter().all(|profile| {
                let ids = profile
                    .list_items()
                    .unwrap()
                    .into_iter()
                    .map(|(id, _)| id)
                    .collect::<std::collections::BTreeSet<_>>();
                ids.contains(&item_a) && ids.contains(&item_b) && ids.contains(&item_c)
            });
            if all_items_present {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(all_items_present);

        profile_b
            .update_item(item_a, &payload("edited-b", "b-edit"))
            .unwrap();
        profile_c.delete_item(item_a).unwrap();
        assert_eq!(
            profile_b.get_item(item_a).unwrap().unwrap().title,
            "edited-b"
        );
        assert!(
            !profile_c
                .list_items()
                .unwrap()
                .into_iter()
                .any(|(id, _)| id == item_a)
        );
        for _ in 0..2 {
            let _ = sync_round(&service_a).await;
            let _ = sync_round(&service_b).await;
            let _ = sync_round(&service_c).await;
        }
        service_c
            .handle()
            .block_device(profile_b.device_id())
            .unwrap()
            .receive()
            .await
            .unwrap();
        assert!(
            profile_c
                .authorization()
                .unwrap()
                .is_locally_blocked(profile_b.device_id())
        );

        service_a.shutdown().unwrap();
        service_b.shutdown().unwrap();
        service_c.shutdown().unwrap();
        profile_a.lock();
        fs::remove_dir_all(directory).unwrap();
    }
}
